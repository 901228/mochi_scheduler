use std::{process::Stdio, time::Duration};

use anyhow::{Context, bail};
// use comfy_table::{CellAlignment, ContentArrangement, Table, modifiers, presets};
use interprocess::local_socket::tokio::{Stream, prelude::*};

use super::{
    job::{Job, JobState},
    protocol::{self, Request, Response},
};
use crate::{cli::Command, settings::Settings, utils::pretty_table::Table};

pub async fn run(settings: Settings, command: Command) -> anyhow::Result<()> {
    // `watch` is not a single request/response: it polls and tails a log, so it
    // gets its own path instead of going through build_request/render.
    if let Command::Watch { id } = command {
        return watch(&settings, id).await;
    }

    let request = build_request(command)?;
    let mut conn = connect_or_spawn(&settings).await?;
    protocol::write_msg(&mut conn, &request).await?;
    let response: Response = protocol::read_msg(&mut conn).await?;

    drop(conn);

    render(response).await
}

fn build_request(command: Command) -> anyhow::Result<Request> {
    Ok(match command {
        Command::Add { label, gpus, argv } => Request::Add {
            argv,
            label,
            cwd: std::env::current_dir().context("getting current directory")?,
            gpus,
            // Snapshot the caller's environment so the job inherits the active
            // shell's PATH/env (pixi, venv, conda, ...) instead of the daemon's.
            env: std::env::vars().collect(),
        },
        Command::List => Request::List,
        Command::Info { id } => Request::Info { id },
        Command::Cat { id } => Request::Cat { id },
        Command::Kill { id } => Request::Kill { id },
        Command::Remove { id } => Request::Remove { id },
        Command::Clear => Request::Clear,
        // No argument -> query; a number sets it, with 0 meaning unlimited.
        Command::CpuLimit { limit: None } => Request::GetCpuLimit,
        Command::CpuLimit { limit: Some(0) } => Request::SetCpuLimit { limit: None },
        Command::CpuLimit { limit: Some(n) } => Request::SetCpuLimit { limit: Some(n) },
        Command::Shutdown => Request::Shutdown,
        Command::Watch { .. } => unreachable!("watch is handled in run"),
        Command::Daemon => unreachable!("daemon is dispatched in main"),
    })
}

/// Follow a job's output live (like `tail -f`) until it reaches a terminal
/// state or the user presses Ctrl+C.
///
/// Ctrl+C only stops watching — the job keeps running, because it is a child of
/// the daemon, not of this client process. `Info` already returns the job's log
/// path and state, so we poll it and stream new bytes from the log file.
async fn watch(settings: &Settings, id: u32) -> anyhow::Result<()> {
    // Fail fast with a clear error if the job doesn't exist.
    if fetch_job(settings, id).await?.is_none() {
        eprintln!("[ERROR] No such job (id {id})");
        std::process::exit(1);
    }

    tokio::select! {
        res = follow(settings, id) => res,
        _ = tokio::signal::ctrl_c() => {
            println!();
            eprintln!("(stopped watching job {id}; it keeps running)");
            Ok(())
        }
    }
}

/// Poll the job and stream new log bytes until it finishes.
async fn follow(settings: &Settings, id: u32) -> anyhow::Result<()> {
    let mut pos: u64 = 0;
    loop {
        let Some(job) = fetch_job(settings, id).await? else {
            eprintln!("(job {id} no longer exists)");
            return Ok(());
        };
        drain_log(&job.log_path, &mut pos).await?;
        if job.state.is_terminal() {
            // Drain once more in case bytes landed between the read and exit.
            drain_log(&job.log_path, &mut pos).await?;
            eprintln!("(job {id} {})", job.state.as_str());
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Write any bytes appended past `pos` to stdout, advancing `pos`. A log file
/// that doesn't exist yet (job not started) is treated as empty.
async fn drain_log(path: &std::path::Path, pos: &mut u64) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut f = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).context("opening job log"),
    };
    f.seek(std::io::SeekFrom::Start(*pos)).await?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).await?;
    if !buf.is_empty() {
        use std::io::Write;
        let mut out = std::io::stdout();
        out.write_all(&buf)?;
        out.flush()?;
        *pos += buf.len() as u64;
    }
    Ok(())
}

/// Fetch a job's current state via an `Info` request. `Ok(None)` means no such job.
async fn fetch_job(settings: &Settings, id: u32) -> anyhow::Result<Option<Job>> {
    let mut conn = connect_or_spawn(settings).await?;
    protocol::write_msg(&mut conn, &Request::Info { id }).await?;
    let resp: Response = protocol::read_msg(&mut conn).await?;
    match resp {
        Response::Job(job) => Ok(Some(job)),
        Response::Error(_) => Ok(None),
        other => bail!("unexpected response to info: {other:?}"),
    }
}

async fn connect(settings: &Settings) -> anyhow::Result<Stream> {
    let (name, socket_display_name) = settings.socket_name(false)?;

    Stream::connect(name)
        .await
        .with_context(|| format!("fail to connect daemon {socket_display_name}"))
}

/// Try to connect to the daemon. If nothing is listening, spawn it and retry for a short while.
async fn connect_or_spawn(settings: &Settings) -> anyhow::Result<Stream> {
    if let Ok(conn) = connect(settings).await {
        return Ok(conn);
    }

    spawn_daemon().context("spawning background daemon")?;

    // Give the daemon a moment to create its socket.
    for _ in 0..100 {
        if let Ok(conn) = connect(settings).await {
            return Ok(conn);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!("could not connect to the msc daemon after starting it");
}

/// Launch `msc __daemon` as a detached background process.
fn spawn_daemon() -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("locating own executable")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("__daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Detach from the controlling terminal / process group so the daemon
        // outlives the shell that first triggered it.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    cmd.spawn()?;
    Ok(())
}

async fn render(response: Response) -> anyhow::Result<()> {
    match response {
        Response::Ok(msg) => println!("{msg}"),
        Response::Error(msg) => {
            eprintln!("[ERROR] {msg}");
            std::process::exit(1);
        }

        Response::LogPath(path) => match tokio::fs::read(&path).await {
            Ok(bytes) => {
                use std::io::Write;
                let mut stdout = std::io::stdout();
                stdout.write_all(&bytes)?;
                stdout.flush()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("(no output yet)");
            }
            Err(e) => return Err(e).context("Error reading job output"),
        },
        Response::Job(job) => print_job_details(&job)?,
        Response::Jobs(jobs) => print_jobs(&jobs)?,
    }

    Ok(())
}

fn print_job_details(job: &Job) -> anyhow::Result<()> {
    // Every field is always shown. Missing values render as "-", except an
    // absent label, which is left blank.
    let dash = || "-".to_string();
    let label = job.label.clone().unwrap_or_default();
    let gpus = if job.gpus > 0 {
        job.gpus.to_string()
    } else {
        dash()
    };
    let assigned = if job.assigned_gpus.is_empty() {
        dash()
    } else {
        format_gpu_list(&job.assigned_gpus)
    };
    let exit = job.exit_code.map(|c| c.to_string()).unwrap_or_else(dash);
    let started = job.started_at.map(|t| t.to_rfc3339()).unwrap_or_else(dash);
    let finished = job.finished_at.map(|t| t.to_rfc3339()).unwrap_or_else(dash);

    let mut table = Table::new();
    table
        .set_header(vec!["key", "value"])
        .add_row(vec!["id".to_string(), job.id.to_string()])
        .add_row(vec!["state".to_string(), job.state.as_str().to_string()])
        .add_row(vec!["label".to_string(), label])
        .add_row(vec!["command".to_string(), job.command_line()])
        .add_row(vec!["cwd".to_string(), job.cwd.display().to_string()])
        .add_row(vec!["gpus".to_string(), gpus])
        .add_row(vec!["assigned gpus".to_string(), assigned])
        .add_row(vec!["exit code".to_string(), exit])
        .add_row(vec!["log".to_string(), job.log_path.display().to_string()])
        .add_row(vec!["enqueued".to_string(), job.enqueued_at.to_rfc3339()])
        .add_row(vec!["started".to_string(), started])
        .add_row(vec!["finished".to_string(), finished]);

    println!("{table}");
    if job.state == JobState::Running {
        println!("(still running)");
    }

    Ok(())
}

fn print_jobs(jobs: &[Job]) -> anyhow::Result<()> {
    let mut table = Table::new();

    table
        // .load_preset(presets::UTF8_FULL_CONDENSED)
        // .apply_modifier(modifiers::UTF8_ROUND_CORNERS)
        // .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["id", "state", "exit", "gpus", "label", "command"])
        .add_rows(jobs.iter().map(|job| {
            let exit = job
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_string());
            // Show the assigned indices while running, otherwise the request count.
            let gpus = if !job.assigned_gpus.is_empty() {
                format_gpu_list(&job.assigned_gpus)
            } else if job.gpus > 0 {
                job.gpus.to_string()
            } else {
                "-".to_string()
            };
            vec![
                job.id.to_string(),
                job.state.as_str().to_string(),
                exit,
                gpus,
                job.label.clone().unwrap_or_default(),
                job.command_line(),
            ]
        }));

    // Keep the table within the terminal by truncating the (last) command column.
    // When output isn't a terminal (piped/redirected), leave it untruncated.
    if let Some((terminal_size::Width(w), _)) = terminal_size::terminal_size() {
        const COMMAND_COLUMN: usize = 5;
        table.fit_to_width(w as usize, COMMAND_COLUMN);
    }

    println!("{table}");
    Ok(())
}

/// Render assigned GPU indices like `[0,2]` for table display.
fn format_gpu_list(indices: &[u32]) -> String {
    let joined = indices
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!("[{joined}]")
}
