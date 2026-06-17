use std::{
    collections::HashMap,
    io,
    process::Stdio,
    sync::{Arc, Mutex},
};

use anyhow::Context;
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerOptions,
    tokio::{Listener, Stream, prelude::*},
};
use tokio::sync::{Notify, oneshot};

use super::{
    protocol::{self, Request, Response},
    scheduler::{AppState, KillOutcome, RemoveOutcome, RunResult, RunSpec},
};
use crate::settings::Settings;

/// Shared daemon state, cloned (via Arc) into every connection handler and the scheduler.
#[derive(Clone)]
struct Daemon {
    settings: Settings,
    state: Arc<Mutex<AppState>>,
    /// Wakes the scheduler whenever a job is added or finishes.
    notify: Arc<Notify>,
    /// Kill switches for currently running jobs, keyed by job id.
    kills: Arc<Mutex<HashMap<u32, oneshot::Sender<()>>>>,
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

    let daemon = Daemon {
        settings,
        state: Arc::new(Mutex::new(state)),
        notify: Arc::new(Notify::new()),
        kills: Arc::new(Mutex::new(HashMap::new())),
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
    let (opts, socket_display_name) = if GenericNamespaced::is_supported() {
        let name = settings.socket_ns.as_str().to_ns_name::<GenericNamespaced>()?;
        (ListenerOptions::new().name(name), settings.socket_ns.clone())
    } else {
        // Best-effort cleanup of a stale socket file (only used on platforms
        // without namespaced sockets).
        let _ = std::fs::remove_file(&settings.socket_fs);
        let name = settings.socket_fs.as_path().to_fs_name::<GenericFilePath>()?;
        (
            ListenerOptions::new().name(name),
            settings.socket_fs.display().to_string(),
        )
    };

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
        Request::Add { argv, label, cwd } => {
            let id = {
                let mut state = daemon.state.lock().unwrap();
                let id = state.add(&daemon.settings.log_dir, argv, label, cwd);
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

fn persist(daemon: &Daemon) {
    let state = daemon.state.lock().unwrap();
    if let Err(e) = state.save(&daemon.settings.state_file) {
        eprintln!("msc daemon: failed to persist state: {e}");
    }
}

/// Launch one job, redirecting its output to the job's log file, and wait for it to finish or be killed.
async fn run_one(spec: &RunSpec, kill_rx: oneshot::Receiver<()>) -> RunResult {
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

/// The scheduler runs queued jobs one at a time, in id order.
///
/// The outer loop blocks on `notify` (signalled by `add` and at startup); the
/// inner loop then fully drains the queue. `Notify` only buffers a single
/// permit, so draining completely here means a burst of `add`s that arrive
/// while a job is running can't leave queued jobs stranded.
async fn scheduler(daemon: Daemon) {
    loop {
        daemon.notify.notified().await;

        loop {
            let spec = {
                let mut state = daemon.state.lock().unwrap();
                state.take_next_queued()
            };
            let Some(spec) = spec else { break };
            persist(&daemon);

            let (kill_tx, kill_rx) = oneshot::channel();
            daemon.kills.lock().unwrap().insert(spec.id, kill_tx);

            let result = run_one(&spec, kill_rx).await;

            daemon.kills.lock().unwrap().remove(&spec.id);
            {
                let mut state = daemon.state.lock().unwrap();
                state.finish(spec.id, result);
            }
            persist(&daemon);
        }
    }
}
