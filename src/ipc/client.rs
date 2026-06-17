use std::{process::Stdio, time::Duration};

use anyhow::{Context, bail};
// use comfy_table::{CellAlignment, ContentArrangement, Table, modifiers, presets};
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced,
    tokio::{Stream, prelude::*},
};

use super::{
    job::{Job, JobState},
    protocol::{self, Request, Response},
};
use crate::{
    cli::Command,
    settings::Settings,
    utils::pretty_table::{Table, style::FrameCorner},
};

pub async fn run(settings: Settings, command: Command) -> anyhow::Result<()> {
    let request = build_request(command)?;
    let mut conn = connect_or_spawn(&settings).await?;
    protocol::write_msg(&mut conn, &request).await?;
    let response: Response = protocol::read_msg(&mut conn).await?;

    drop(conn);

    render(response).await
}

fn build_request(command: Command) -> anyhow::Result<Request> {
    Ok(match command {
        Command::Add { label, argv } => Request::Add {
            argv,
            label,
            cwd: std::env::current_dir().context("getting current directory")?,
        },
        Command::List => Request::List,
        Command::Info { id } => Request::Info { id },
        Command::Cat { id } => Request::Cat { id },
        Command::Kill { id } => Request::Kill { id },
        Command::Remove { id } => Request::Remove { id },
        Command::Clear => Request::Clear,
        Command::Shutdown => Request::Shutdown,
        Command::Daemon => unreachable!("daemon is dispatched in main"),
    })
}

async fn connect(settings: &Settings) -> anyhow::Result<Stream> {
    let (name, socket_display_name) = if GenericNamespaced::is_supported() {
        let name = settings.socket_ns.as_str().to_ns_name::<GenericNamespaced>()?;
        (name, settings.socket_ns.clone())
    } else {
        let name = settings.socket_fs.as_path().to_fs_name::<GenericFilePath>()?;
        (name, settings.socket_fs.display().to_string())
    };

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
    let mut table = Table::new();
    table
        // .load_preset(presets::UTF8_FULL_CONDENSED)
        // .apply_modifier(modifiers::UTF8_ROUND_CORNERS)
        // .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["key", "value"])
        .add_row(vec!["id", job.id.to_string().as_str()])
        .add_row(vec!["state", job.state.as_str()])
        .add_row_if(
            |_| job.label.is_some(),
            |_| vec!["label", job.label.as_ref().unwrap()],
        )
        .add_row(vec!["command", job.command_line().as_str()])
        .add_row(vec!["cwd", job.cwd.display().to_string().as_str()])
        .add_row_if(
            |_| job.exit_code.is_some(),
            |_| vec!["exit code".into(), job.exit_code.as_ref().unwrap().to_string()],
        )
        .add_row(vec!["log", job.log_path.display().to_string().as_str()])
        .add_row(vec!["enqueued", job.enqueued_at.to_rfc3339().as_str()])
        .add_row_if(
            |_| job.started_at.is_some(),
            |_| vec!["started".into(), job.started_at.as_ref().unwrap().to_rfc3339()],
        )
        .add_row_if(
            |_| job.finished_at.is_some(),
            |_| vec!["finished".into(), job.finished_at.as_ref().unwrap().to_rfc3339()],
        );

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
        .set_header(vec!["id", "state", "exit", "label", "command"])
        .add_rows(jobs.iter().map(|job| {
            let exit = job
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_string());
            vec![
                job.id.to_string(),
                job.state.as_str().to_string(),
                exit,
                job.label.clone().unwrap_or_default(),
                job.command_line(),
            ]
        }));

    println!("{table}");
    Ok(())
}
