use std::{collections::BTreeSet, process::Stdio, time::Duration};

use anyhow::{Context, bail};
use chrono::{DateTime, Local, Utc};
// use comfy_table::{CellAlignment, ContentArrangement, Table, modifiers, presets};
use interprocess::local_socket::tokio::{Stream, prelude::*};

use super::{
    job::{Job, JobState},
    protocol::{self, Request, Response},
};
use crate::{
    cli::{Command, ConfigCommand, StateFilter},
    settings::Settings,
    utils::pretty_table::Table,
};

pub async fn run(settings: Settings, command: Command) -> anyhow::Result<()> {
    match command {
        Command::Watch { id } => watch(&settings, id).await,
        Command::List { all, state, by_id } => list_jobs(&settings, all, &state, by_id).await,
        Command::Kill { ids, all: false } => kill_many(&settings, &ids).await,
        Command::Priority { args } => set_priority_many(&settings, &args).await,
        Command::Rerun { ids } => rerun_many(&settings, &ids).await,
        command => {
            let request = build_request(command)?;
            let mut conn = connect_or_spawn(&settings).await?;
            protocol::write_msg(&mut conn, &request).await?;
            let response: Response = protocol::read_msg(&mut conn).await?;
            drop(conn);
            render(response).await
        }
    }
}

fn build_request(command: Command) -> anyhow::Result<Request> {
    Ok(match command {
        Command::Add {
            label,
            gpus,
            priority,
            argv,
        } => Request::Add {
            argv,
            label,
            cwd: std::env::current_dir().context("getting current directory")?,
            gpus,
            priority,
            // Snapshot the caller's environment so the job inherits the active
            // shell's PATH/env (pixi, venv, conda, ...) instead of the daemon's.
            env: std::env::vars().collect(),
        },
        Command::List { .. } => unreachable!("list is handled in run"),
        Command::Info { id } => Request::Info { id },
        Command::Cat { id } => Request::Cat { id },
        Command::Kill { all: true, .. } => Request::KillAll,
        Command::Kill { .. } => unreachable!("kill with ids is handled in run"),
        Command::Priority { .. } => unreachable!("priority is handled in run"),
        Command::Rerun { .. } => unreachable!("rerun is handled in run"),
        Command::Remove { id } => Request::Remove { id },
        Command::Clear => Request::Clear,
        Command::Devices => Request::GetDevices,
        Command::Config { setting } => match setting {
            // No argument -> query; a number sets it, with 0 meaning unlimited.
            ConfigCommand::CpuLimit { limit: None } => Request::GetCpuLimit,
            ConfigCommand::CpuLimit { limit: Some(0) } => Request::SetCpuLimit { limit: None },
            ConfigCommand::CpuLimit { limit: Some(n) } => Request::SetCpuLimit { limit: Some(n) },
        },
        Command::Shutdown => Request::Shutdown,
        Command::Watch { .. } => unreachable!("watch is handled in run"),
        Command::Daemon => unreachable!("daemon is dispatched in main"),
    })
}

/// Expand a slice of id args (plain numbers or `start-end` ranges) into a
/// sorted, deduplicated list of job ids.
fn parse_job_ids(args: &[String]) -> anyhow::Result<Vec<u32>> {
    let mut ids: Vec<u32> = Vec::new();
    let mut seen = BTreeSet::new();
    for arg in args {
        if let Some((start, end)) = arg.split_once('-') {
            let a: u32 = start
                .parse()
                .with_context(|| format!("invalid id in range '{arg}'"))?;
            let b: u32 = end
                .parse()
                .with_context(|| format!("invalid id in range '{arg}'"))?;
            if a > b {
                bail!("invalid range '{arg}': start ({a}) is greater than end ({b})");
            }
            for id in a..=b {
                if seen.insert(id) {
                    ids.push(id);
                }
            }
        } else {
            let id: u32 = arg
                .parse()
                .with_context(|| format!("'{arg}' is not a valid job id"))?;
            if seen.insert(id) {
                ids.push(id);
            }
        }
    }
    Ok(ids)
}

/// Kill multiple jobs by id, continuing past errors and printing each result.
async fn kill_many(settings: &Settings, id_args: &[String]) -> anyhow::Result<()> {
    let ids = parse_job_ids(id_args)?;
    let mut had_error = false;
    for id in ids {
        let mut conn = connect_or_spawn(settings).await?;
        protocol::write_msg(&mut conn, &Request::Kill { id }).await?;
        let resp: Response = protocol::read_msg(&mut conn).await?;
        drop(conn);
        match resp {
            Response::Ok(msg) => println!("{msg}"),
            Response::Error(msg) => {
                eprintln!("[ERROR] {msg}");
                had_error = true;
            }
            other => bail!("unexpected response to kill: {other:?}"),
        }
    }
    if had_error {
        std::process::exit(1);
    }
    Ok(())
}

/// Change the priority of multiple jobs. `args` must have at least 2 elements;
/// the last is the priority value and all preceding elements are job ids.
async fn set_priority_many(settings: &Settings, args: &[String]) -> anyhow::Result<()> {
    // Enforced by clap (num_args = 2..), but guard defensively.
    assert!(args.len() >= 2, "priority requires at least one id and a priority value");

    let priority_str = args.last().unwrap();
    let priority: i32 = priority_str.parse().with_context(|| {
        format!("'{priority_str}' is not a valid priority value (expected integer)")
    })?;
    let ids = parse_job_ids(&args[..args.len() - 1])?;

    let mut had_error = false;
    for id in ids {
        let mut conn = connect_or_spawn(settings).await?;
        protocol::write_msg(&mut conn, &Request::SetPriority { id, priority }).await?;
        let resp: Response = protocol::read_msg(&mut conn).await?;
        drop(conn);
        match resp {
            Response::Ok(msg) => println!("{msg}"),
            Response::Error(msg) => {
                eprintln!("[ERROR] {msg}");
                had_error = true;
            }
            other => bail!("unexpected response to set-priority: {other:?}"),
        }
    }
    if had_error {
        std::process::exit(1);
    }
    Ok(())
}

/// Re-run multiple jobs by id, continuing past errors and printing each result.
async fn rerun_many(settings: &Settings, id_args: &[String]) -> anyhow::Result<()> {
    let ids = parse_job_ids(id_args)?;
    let mut had_error = false;
    for id in ids {
        let mut conn = connect_or_spawn(settings).await?;
        protocol::write_msg(&mut conn, &Request::Rerun { id }).await?;
        let resp: Response = protocol::read_msg(&mut conn).await?;
        drop(conn);
        match resp {
            Response::Ok(msg) => println!("{msg}"),
            Response::Error(msg) => {
                eprintln!("[ERROR] {msg}");
                had_error = true;
            }
            other => bail!("unexpected response to rerun: {other:?}"),
        }
    }
    if had_error {
        std::process::exit(1);
    }
    Ok(())
}

/// Sort key for execution-order listing:
///   running jobs  (group 0) — by id
///   queued jobs   (group 1) — by descending priority then id (matches scheduler)
///   terminal jobs (group 2) — by id
fn execution_order_key(job: &Job) -> (u8, i64, u32) {
    match job.state {
        JobState::Running => (0, 0, job.id),
        JobState::Queued => (1, -(job.priority as i64), job.id),
        _ => (2, 0, job.id),
    }
}

/// Follow a *running* job's output live (like `tail -f`) until it reaches a
/// terminal state or the user presses Ctrl+C.
///
/// Watching only makes sense for a running job — a terminal job's output is
/// complete (use `cat` for that), so we refuse to "follow" one rather than
/// dumping its whole log. With no id we auto-pick the sole running job, or list
/// the running jobs when there is more than one.
///
/// Ctrl+C only stops watching — the job keeps running, because it is a child of
/// the daemon, not of this client process. `Info` already returns the job's log
/// path and state, so we poll it and stream new bytes from the log file.
async fn watch(settings: &Settings, id: Option<u32>) -> anyhow::Result<()> {
    let id = match id {
        Some(id) => match resolve_watch_target(settings, id).await? {
            Some(id) => id,
            None => return Ok(()),
        },
        None => match pick_running_job(settings).await? {
            Some(id) => id,
            None => return Ok(()),
        },
    };

    tokio::select! {
        res = follow(settings, id) => res,
        _ = tokio::signal::ctrl_c() => {
            println!();
            eprintln!("(stopped watching job {id}; it keeps running)");
            Ok(())
        }
    }
}

/// Validate an explicitly requested watch target. Returns `Some(id)` only when
/// the job exists and is currently running; otherwise prints an error/warning
/// and returns `None` (exiting for a missing job).
async fn resolve_watch_target(settings: &Settings, id: u32) -> anyhow::Result<Option<u32>> {
    let Some(job) = fetch_job(settings, id).await? else {
        eprintln!("[ERROR] No such job (id {id})");
        std::process::exit(1);
    };
    if job.state != JobState::Running {
        eprintln!(
            "[WARN] job {id} is {}, not running; watch only follows running jobs.",
            job.state.as_str()
        );
        if job.state.is_terminal() {
            eprintln!("(use `msc cat {id}` to see its captured output)");
        }
        return Ok(None);
    }
    Ok(Some(job.id))
}

/// Pick a running job to watch when no id is given: the sole running job if
/// there is exactly one, otherwise print the running jobs (or a notice when
/// there are none) and return `None`.
async fn pick_running_job(settings: &Settings) -> anyhow::Result<Option<u32>> {
    let running: Vec<Job> = fetch_all_jobs(settings)
        .await?
        .into_iter()
        .filter(|job| job.state == JobState::Running)
        .collect();

    match running.as_slice() {
        [] => {
            eprintln!("(no running jobs to watch)");
            Ok(None)
        }
        [job] => Ok(Some(job.id)),
        _ => {
            eprintln!("Multiple running jobs; watch one with `msc watch <id>`:");
            print_jobs(&running)?;
            Ok(None)
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

/// Fetch every job from the daemon via a `List` request.
async fn fetch_all_jobs(settings: &Settings) -> anyhow::Result<Vec<Job>> {
    let mut conn = connect_or_spawn(settings).await?;
    protocol::write_msg(&mut conn, &Request::List).await?;
    let resp: Response = protocol::read_msg(&mut conn).await?;
    drop(conn);

    match resp {
        Response::Jobs(jobs) => Ok(jobs),
        Response::Error(msg) => {
            eprintln!("[ERROR] {msg}");
            std::process::exit(1);
        }
        other => bail!("unexpected response to list: {other:?}"),
    }
}

/// Fetch every job, filter by the requested states client-side, and print.
///
/// Default (no `--all`, no `--state`): running and queued jobs. `--all` shows
/// every state; one or more `--state` show exactly those.
/// Jobs are sorted by execution order for the default active-jobs view only.
/// --all and --state views keep id order (chronological, natural for history).
/// --by-id overrides to id order for the default view.
async fn list_jobs(settings: &Settings, all: bool, states: &[StateFilter], by_id: bool) -> anyhow::Result<()> {
    let mut filtered: Vec<Job> = fetch_all_jobs(settings)
        .await?
        .into_iter()
        .filter(|job| state_wanted(all, states, &job.state))
        .collect();

    if !by_id && !all && states.is_empty() {
        filtered.sort_by_key(execution_order_key);
    }

    if filtered.is_empty() {
        if all || !states.is_empty() {
            eprintln!("(no matching jobs)");
        } else {
            eprintln!("(no running or queued jobs; pass --all to show every state)");
        }
        return Ok(());
    }
    print_jobs(&filtered)
}

/// Whether a job's state passes the list filter.
fn state_wanted(all: bool, states: &[StateFilter], state: &JobState) -> bool {
    if all {
        return true;
    }
    if states.is_empty() {
        // Default view: only the active jobs.
        return matches!(state, JobState::Running | JobState::Queued);
    }
    states.iter().any(|f| filter_matches(*f, state))
}

fn filter_matches(filter: StateFilter, state: &JobState) -> bool {
    matches!(
        (filter, state),
        (StateFilter::Queued, JobState::Queued)
            | (StateFilter::Running, JobState::Running)
            | (StateFilter::Finished, JobState::Finished)
            | (StateFilter::Killed, JobState::Killed)
            | (StateFilter::Failed, JobState::Failed)
    )
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
    let started = job.started_at.map(|t| fmt_local(t)).unwrap_or_else(dash);
    let finished = job.finished_at.map(|t| fmt_local(t)).unwrap_or_else(dash);

    let mut table = Table::new();
    table
        .set_header(vec!["key", "value"])
        .add_row(vec!["id".to_string(), job.id.to_string()])
        .add_row(vec!["state".to_string(), job.state.as_str().to_string()])
        .add_row(vec!["label".to_string(), label])
        .add_row(vec!["command".to_string(), job.command_line()])
        .add_row(vec!["cwd".to_string(), job.cwd.display().to_string()])
        .add_row(vec!["priority".to_string(), job.priority.to_string()])
        .add_row(vec!["gpus".to_string(), gpus])
        .add_row(vec!["assigned gpus".to_string(), assigned])
        .add_row(vec!["exit code".to_string(), exit])
        .add_row(vec!["log".to_string(), job.log_path.display().to_string()])
        .add_row(vec!["enqueued".to_string(), fmt_local(job.enqueued_at)])
        .add_row(vec!["started".to_string(), started])
        .add_row(vec!["finished".to_string(), finished]);

    // Wrap the value column onto multiple lines so a long command/path doesn't
    // blow the table past the terminal width. When output isn't a terminal
    // (piped/redirected), leave values intact on one line.
    if let Some((terminal_size::Width(w), _)) = terminal_size::terminal_size() {
        const VALUE_COLUMN: usize = 1;
        table.wrap_to_width(w as usize, VALUE_COLUMN);
    }

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
        .set_header(vec!["id", "state", "exit", "pri", "gpus", "label", "command"])
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
                job.priority.to_string(),
                gpus,
                job.label.clone().unwrap_or_default(),
                job.command_line(),
            ]
        }));

    // Right-align the numeric id column.
    table.right_align_column(0);

    // Keep the table within the terminal by truncating the (last) command column.
    // When output isn't a terminal (piped/redirected), leave it untruncated.
    if let Some((terminal_size::Width(w), _)) = terminal_size::terminal_size() {
        const COMMAND_COLUMN: usize = 6;
        table.fit_to_width(w as usize, COMMAND_COLUMN);
    }

    println!("{table}");
    Ok(())
}

/// Render assigned GPU indices like `[0,2]` for table display.
fn fmt_local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string()
}

fn format_gpu_list(indices: &[u32]) -> String {
    let joined = indices
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!("[{joined}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_view_is_running_and_queued() {
        assert!(state_wanted(false, &[], &JobState::Running));
        assert!(state_wanted(false, &[], &JobState::Queued));
        assert!(!state_wanted(false, &[], &JobState::Finished));
        assert!(!state_wanted(false, &[], &JobState::Killed));
        assert!(!state_wanted(false, &[], &JobState::Failed));
    }

    #[test]
    fn all_shows_every_state() {
        for s in [
            JobState::Queued,
            JobState::Running,
            JobState::Finished,
            JobState::Killed,
            JobState::Failed,
        ] {
            assert!(state_wanted(true, &[], &s));
        }
    }

    #[test]
    fn explicit_states_filter_exactly() {
        let filters = [StateFilter::Finished, StateFilter::Failed];
        assert!(state_wanted(false, &filters, &JobState::Finished));
        assert!(state_wanted(false, &filters, &JobState::Failed));
        assert!(!state_wanted(false, &filters, &JobState::Running));
        assert!(!state_wanted(false, &filters, &JobState::Queued));
    }

    #[test]
    fn parse_job_ids_handles_singles_ranges_and_dedup() {
        let s = |x: &str| x.to_string();

        assert_eq!(parse_job_ids(&[s("5")]).unwrap(), vec![5]);
        assert_eq!(parse_job_ids(&[s("3-5")]).unwrap(), vec![3, 4, 5]);
        assert_eq!(parse_job_ids(&[s("0-0")]).unwrap(), vec![0]);

        // overlapping: 4 appears in the range and again explicitly — deduplicated
        assert_eq!(
            parse_job_ids(&[s("3-5"), s("4"), s("7")]).unwrap(),
            vec![3, 4, 5, 7]
        );

        // multiple ranges
        assert_eq!(
            parse_job_ids(&[s("1-2"), s("5-6")]).unwrap(),
            vec![1, 2, 5, 6]
        );
    }

    #[test]
    fn parse_job_ids_rejects_bad_input() {
        let s = |x: &str| x.to_string();

        assert!(parse_job_ids(&[s("5-3")]).is_err()); // start > end
        assert!(parse_job_ids(&[s("abc")]).is_err()); // not a number
        assert!(parse_job_ids(&[s("1-abc")]).is_err()); // bad range end
    }
}
