use std::{
    collections::HashMap,
    io,
    process::Stdio,
    sync::{Arc, Mutex},
};

use anyhow::Context;
use interprocess::local_socket::{
    ListenerOptions,
    tokio::{Listener, Stream, prelude::*},
};
use tokio::sync::{Notify, oneshot};

use super::{
    protocol::{self, Request, Response},
    scheduler::{AppState, KillOutcome, RemoveOutcome, RunResult, RunSpec, SetPriorityOutcome},
};
use crate::{gpu, settings::Settings};

/// Shared daemon state, cloned (via Arc) into every connection handler and the scheduler.
#[derive(Clone)]
struct Daemon {
    settings: Settings,
    state: Arc<Mutex<AppState>>,
    /// Wakes the scheduler whenever a job is added or finishes.
    notify: Arc<Notify>,
    /// Kill switches for currently running jobs, keyed by job id.
    kills: Arc<Mutex<HashMap<u32, oneshot::Sender<()>>>>,
    /// Total GPUs on this host and the stack they belong to (detected at startup).
    gpu: gpu::GpuInfo,
}

pub async fn run(settings: Settings) -> anyhow::Result<()> {
    let listener = match bind(&settings) {
        Ok(listener) => listener,
        Err(e) => {
            if let Some(io_err) = e.downcast_ref::<io::Error>() {
                if io_err.kind() == io::ErrorKind::AddrInUse {
                    eprintln!(
                        "Error: the socket is occupied. Please check if it is in use by another process and try again."
                    );
                }
            }
            return Err(e);
        }
    };

    let state = AppState::load(&settings.state_file)?;
    // Persist the post-load reconciliation (running -> failed) immediately.
    state.save(&settings.state_file)?;

    let gpu = gpu::detect();
    println!("msc daemon: detected {} GPU(s) ({:?})", gpu.count, gpu.vendor);

    let daemon = Daemon {
        settings,
        state: Arc::new(Mutex::new(state)),
        notify: Arc::new(Notify::new()),
        kills: Arc::new(Mutex::new(HashMap::new())),
        gpu,
    };

    // Start the scheduler and kick it once in case there are leftover queued jobs.
    tokio::spawn(scheduler(daemon.clone()));
    daemon.notify.notify_one();

    loop {
        let conn = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("msc daemon: accept error: {e}");
                continue;
            }
        };

        let daemon = daemon.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(conn, daemon).await {
                eprintln!("msc daemon: connection error: {e}");
            }
        });
    }
}

fn bind(settings: &Settings) -> anyhow::Result<Listener> {
    let (name, socket_display_name) = settings.socket_name(false)?;
    let opts = ListenerOptions::new().name(name);

    opts.create_tokio()
        .with_context(|| format!("failed to bind socket {socket_display_name}"))
}

async fn handle_conn(mut conn: Stream, daemon: Daemon) -> anyhow::Result<()> {
    let request: Request = protocol::read_msg(&mut conn).await?;
    println!("Received request: {:?}", request);
    let response = handle_request(request, &daemon);
    protocol::write_msg(&mut conn, &response).await?;
    Ok(())
}

fn handle_request(request: Request, daemon: &Daemon) -> Response {
    match request {
        Request::Add {
            argv,
            label,
            cwd,
            gpus,
            priority,
            env,
        } => {
            // Reject a request for more GPUs than exist, otherwise it would sit
            // in the queue forever (the pool can never satisfy it).
            if gpus > daemon.gpu.count {
                return Response::Error(format!(
                    "job requests {gpus} GPU(s) but only {} are available",
                    daemon.gpu.count
                ));
            }
            let id = {
                let mut state = daemon.state.lock().unwrap();
                let id = state.add(&daemon.settings.log_dir, argv, label, cwd, gpus, priority, env);
                if let Err(e) = state.save(&daemon.settings.state_file) {
                    return Response::Error(format!("persisting state: {e}"));
                }
                id
            };
            // Wake the scheduler only after the state lock above is released,
            // so it doesn't immediately block trying to re-acquire it.
            daemon.notify.notify_one();
            Response::Ok(format!("Queued job {id}"))
        }
        Request::List => {
            let jobs = daemon.state.lock().unwrap().list();
            Response::Jobs(jobs)
        }
        Request::Info { id } => {
            let job = daemon.state.lock().unwrap().get(id);
            if let Some(j) = job {
                Response::Job(j)
            } else {
                Response::Error(format!("No such job (id {id})"))
            }
        }
        Request::Cat { id } => {
            let path = daemon.state.lock().unwrap().get(id).map(|j| j.log_path);
            if let Some(p) = path {
                Response::LogPath(p)
            } else {
                Response::Error(format!("No such job (id {id})"))
            }
        }
        Request::Kill { id } => {
            let outcome = daemon.state.lock().unwrap().request_kill(id);
            match outcome {
                KillOutcome::Running => {
                    if let Some(tx) = daemon.kills.lock().unwrap().remove(&id) {
                        let _ = tx.send(());
                    }
                    Response::Ok(format!("Killed job {id}"))
                }
                KillOutcome::Dequeued => {
                    persist(daemon);
                    Response::Ok(format!("Dequeued job {id}"))
                }
                KillOutcome::AlreadyDone => Response::Error(format!("Job {id} is already finished")),
                KillOutcome::NotFound => Response::Error(format!("No such job (id {id})")),
            }
        }
        Request::SetPriority { id, priority } => {
            let outcome = daemon.state.lock().unwrap().set_priority(id, priority);
            match outcome {
                SetPriorityOutcome::Updated => {
                    persist(daemon);
                    // A reorder may let a different queued job win the next slot.
                    daemon.notify.notify_one();
                    Response::Ok(format!("Set job {id} priority to {priority}"))
                }
                SetPriorityOutcome::NotQueued => Response::Error(format!(
                    "Job {id} is not queued; priority only applies to queued jobs"
                )),
                SetPriorityOutcome::NotFound => Response::Error(format!("No such job (id {id})")),
            }
        }
        Request::Remove { id } => {
            let mut state = daemon.state.lock().unwrap();
            match state.remove(id) {
                RemoveOutcome::Removed => {
                    if let Err(e) = state.save(&daemon.settings.state_file) {
                        return Response::Error(format!("Error saving persisting state: {e}"));
                    }
                    Response::Ok(format!("Removed job {id}"))
                }
                RemoveOutcome::Running => Response::Error(format!("Job {id} is running; use `kill` first")),
                RemoveOutcome::NotFound => Response::Error(format!("No such job (id {id})")),
            }
        }
        Request::Clear => {
            let mut state = daemon.state.lock().unwrap();
            let removed = state.clear_finished();
            if let Err(e) = state.save(&daemon.settings.state_file) {
                return Response::Error(format!("Error saving persisting state: {e}"));
            }
            Response::Ok(format!("Cleared {removed} finished jobs"))
        }
        Request::GetCpuLimit => {
            let limit = daemon.state.lock().unwrap().cpu_limit();
            Response::Ok(format!("CPU job limit: {}", describe_cpu_limit(limit)))
        }
        Request::SetCpuLimit { limit } => {
            {
                let mut state = daemon.state.lock().unwrap();
                state.set_cpu_limit(limit);
                if let Err(e) = state.save(&daemon.settings.state_file) {
                    return Response::Error(format!("persisting state: {e}"));
                }
            }
            // Raising the limit may make queued CPU jobs runnable; wake the scheduler.
            daemon.notify.notify_one();
            Response::Ok(format!("CPU job limit set to {}", describe_cpu_limit(limit)))
        }
        Request::Shutdown => {
            // Reply is sent by the caller before we exit, so schedule the exit.
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                std::process::exit(0);
            });
            Response::Ok("Shutting down".into())
        }
    }
}

/// Human-readable form of a CPU-job limit for command replies.
fn describe_cpu_limit(limit: Option<u32>) -> String {
    match limit {
        None => "unlimited".to_string(),
        Some(n) => n.to_string(),
    }
}

fn persist(daemon: &Daemon) {
    let state = daemon.state.lock().unwrap();
    if let Err(e) = state.save(&daemon.settings.state_file) {
        eprintln!("msc daemon: failed to persist state: {e}");
    }
}

/// Launch one job, redirecting its output to the job's log file, and wait for it to finish or be killed.
async fn run_one(spec: &RunSpec, vendor: gpu::Vendor, kill_rx: oneshot::Receiver<()>) -> RunResult {
    let stdout = match std::fs::File::create(&spec.log_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("msc daemon: cannot create log file for job {}: {e}", spec.id);
            return RunResult::SpawnFailed;
        }
    };
    // stdout and stderr share one handle to the same file, so the job's output
    // is captured interleaved in a single log (what `cat` later prints).
    let stderr = match stdout.try_clone() {
        Ok(f) => f,
        Err(_) => return RunResult::SpawnFailed,
    };

    let mut cmd = tokio::process::Command::new(&spec.argv[0]);
    cmd.args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    // Run with the environment the client captured at `add` time, so the job
    // sees the user's active shell (pixi/venv/conda PATH, etc.) instead of the
    // daemon's. Replace the inherited env entirely; the captured snapshot is a
    // complete environment. Legacy jobs (empty snapshot) keep inheriting the
    // daemon env. GPU variables are applied afterwards so they always win.
    if !spec.env.is_empty() {
        cmd.env_clear();
        cmd.envs(spec.env.iter().map(|(k, v)| (k, v)));
    }

    // Isolate the job to exactly the GPUs it was assigned by exporting the
    // vendor's visible-devices variable(s), e.g. CUDA_VISIBLE_DEVICES=0,2.
    // For a 0-GPU job the value is empty, which hides every GPU on Unix. On
    // Windows an empty value can't be set (the OS drops empty env vars), so a
    // 0-GPU job there simply inherits whatever it would normally see.
    let visible = spec
        .assigned_gpus
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    for var in vendor.visible_devices_env() {
        cmd.env(var, &visible);
    }

    // On Windows a console program launched by the (console-less) daemon would
    // otherwise allocate its own console window. The job's stdio is already
    // redirected to the log file, so suppress the window.
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = std::fs::write(&spec.log_path, format!("failed to start command: {e}\n"));
            return RunResult::SpawnFailed;
        }
    };

    tokio::select! {
        status = child.wait() => match status {
            Ok(status) => RunResult::Exited(status.code()),
            Err(_) => RunResult::SpawnFailed,
        },
        _ = kill_rx => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            RunResult::Killed
        }
    }
}

/// The scheduler starts jobs as GPU capacity allows, running them concurrently.
///
/// The outer loop blocks on `notify` (signalled by `add`, at startup, and when
/// a job finishes); the inner loop then starts every job that currently fits
/// the free GPU pool. Unlike a serial queue it does *not* await each job here —
/// each runs in its own task so multiple jobs proceed in parallel. A finishing
/// job releases its GPUs and re-notifies, letting the scheduler backfill the
/// freed capacity. `Notify` only buffers a single permit, but because each wake
/// drains everything that fits, no queued job is left stranded.
async fn scheduler(daemon: Daemon) {
    loop {
        daemon.notify.notified().await;

        loop {
            let spec = {
                let mut state = daemon.state.lock().unwrap();
                state.take_next_runnable(daemon.gpu.count)
            };
            let Some(spec) = spec else { break };
            persist(&daemon);

            let (kill_tx, kill_rx) = oneshot::channel();
            daemon.kills.lock().unwrap().insert(spec.id, kill_tx);

            // Run the job concurrently; when it ends, free its GPUs and wake the
            // scheduler so a waiting job can take the released capacity.
            let daemon = daemon.clone();
            tokio::spawn(async move {
                let result = run_one(&spec, daemon.gpu.vendor, kill_rx).await;

                daemon.kills.lock().unwrap().remove(&spec.id);
                {
                    let mut state = daemon.state.lock().unwrap();
                    state.finish(spec.id, result);
                }
                persist(&daemon);
                daemon.notify.notify_one();
            });
        }
    }
}
