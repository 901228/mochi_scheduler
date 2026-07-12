use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "msc", author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Add a command to the queue.
    ///
    /// Everything after `add` is treated as the command to run, e.g.
    /// `msc add cargo build --release`.
    Add {
        /// Optional human-readable label for the job.
        #[arg(short, long)]
        label: Option<String>,

        /// Number of GPUs to reserve for the job (0 = none).
        #[arg(short, long, default_value_t = 0)]
        gpus: u32,

        /// Scheduling priority; higher runs first, ties break by id (default 0).
        #[arg(short, long, default_value_t = 0, allow_hyphen_values = true)]
        priority: i32,

        /// The command and its arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        argv: Vec<String>,
    },

    /// List jobs and their state. Shows running and queued jobs by default.
    ///
    /// The default view sorts by execution order: running first, then queued
    /// jobs in priority order (highest first). Use `--by-id` to sort by id
    /// instead. `--all` and `--state` views always use id order.
    List {
        /// Show jobs in every state.
        #[arg(short, long, conflicts_with = "state")]
        all: bool,

        /// Only show jobs in these states (repeatable), e.g. `-s finished -s failed`.
        #[arg(short, long, value_enum)]
        state: Vec<StateFilter>,

        /// Sort by id instead of execution order (only affects the default active-jobs view).
        #[arg(long)]
        by_id: bool,
    },

    /// Show full details for a single job.
    Info {
        /// Job id.
        id: u32,
    },

    /// Print the captured output (stdout + stderr) of a job.
    Cat {
        /// Job id.
        id: u32,
    },

    /// Follow a running job's output live. Press Ctrl+C to stop watching; the job keeps running.
    ///
    /// Only running jobs can be watched. Omit the id to watch the sole running
    /// job, or to list the running jobs to choose from when there are several.
    /// By default only output produced from now on is shown; pass `--from-start`
    /// to replay the whole log so far first.
    Watch {
        /// Job id. Omit to auto-pick / list the running jobs.
        id: Option<u32>,

        /// Show the entire log from the beginning before following, instead of
        /// only new output from now on.
        #[arg(short = 'a', long = "from-start")]
        from_start: bool,
    },

    /// Kill one or more running jobs, or drop jobs that are still queued.
    ///
    /// Accepts multiple ids and ranges, e.g. `msc kill 12 15-18`.
    Kill {
        /// Job id(s) to kill. Accepts ranges like `12-15` (kills 12, 13, 14, 15).
        /// Omit when using `--all`.
        #[arg(required_unless_present = "all", conflicts_with = "all", num_args = 1..)]
        ids: Vec<String>,

        /// Kill every running job and drop every queued one.
        #[arg(long)]
        all: bool,
    },

    /// Change the priority of one or more queued jobs to let them jump the queue.
    ///
    /// Usage: `msc priority <id(s)> <new-priority>`. The last argument is the
    /// priority value; all preceding arguments are job ids (ranges accepted).
    /// Example: `msc priority 12 15-18 10` sets jobs 12, 15-18 to priority 10.
    Priority {
        /// Job id(s) followed by the new priority. The last argument is the
        /// priority value (integer); all preceding arguments are job ids
        /// (ranges like `12-15` are accepted).
        #[arg(required = true, num_args = 2.., allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Re-run one or more jobs: queue a fresh copy (same command, dir, and environment).
    ///
    /// Accepts multiple ids and ranges, e.g. `msc rerun 12 15-18`. The re-queued
    /// job starts at priority 0 by default (not the source job's priority); pass
    /// `-p N` to give the fresh copy a different priority.
    Rerun {
        /// Job id(s) to re-run. Accepts ranges like `12-15`.
        #[arg(required = true, num_args = 1..)]
        ids: Vec<String>,

        /// Priority for the re-queued job(s); higher runs first (default 0,
        /// regardless of the source job's priority).
        #[arg(short, long, default_value_t = 0, allow_hyphen_values = true)]
        priority: i32,
    },

    /// Restart one or more running jobs in place: stop and re-run them.
    ///
    /// Each listed job's process is killed and the same job (same id and log
    /// file) is re-queued to run again from the start. Only running jobs can be
    /// restarted — use `rerun` to queue a fresh copy of a finished job. Accepts
    /// multiple ids and ranges, e.g. `msc restart 12 15-18`.
    Restart {
        /// Job id(s) to restart. Accepts ranges like `12-15`.
        #[arg(required = true, num_args = 1..)]
        ids: Vec<String>,
    },

    /// Set or change a job's label.
    ///
    /// Works on a job in any state. Pass an empty string (`msc label 3 ""`) to
    /// clear the label.
    Label {
        /// Job id.
        id: u32,

        /// New label. An empty string clears the label.
        label: String,
    },

    /// Pause every queued job, or pull specific queued jobs out of the queue.
    ///
    /// With no id, pauses all currently queued jobs at once (running jobs finish
    /// but nothing new starts). With one or more ids (ranges accepted, e.g.
    /// `msc pause 12 15-18`), pauses just those queued jobs so the scheduler
    /// skips them until they are resumed.
    Pause {
        /// Job id(s) to pause. Accepts ranges like `12-15`. Omit to pause every
        /// queued job.
        #[arg(num_args = 0..)]
        ids: Vec<String>,
    },

    /// Resume every paused job, or put specific paused jobs back into the queue.
    ///
    /// The inverse of `pause`: with no id it re-queues all paused jobs (asking
    /// for confirmation first if other jobs are still queued); with ids (ranges
    /// accepted) it re-queues just those paused jobs.
    Resume {
        /// Job id(s) to resume. Accepts ranges like `12-15`. Omit to resume every
        /// paused job.
        #[arg(num_args = 0..)]
        ids: Vec<String>,
    },

    /// Remove a finished or queued job from the list.
    Remove {
        /// Job id.
        id: u32,
    },

    /// Remove all finished/killed/failed jobs from the list.
    Clear,

    /// Inspect or control the background daemon (devices, settings, shutdown).
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },

    /// Internal: run the background daemon (not meant to be called directly).
    #[command(name = "__daemon", hide = true)]
    RunDaemon,
}

/// Daemon-level operations, grouped under `msc daemon` to keep them out of the
/// top-level job commands. These act on the daemon/host, not a single job.
#[derive(Subcommand, Debug)]
pub enum DaemonCommand {
    /// Show the GPU devices detected by the daemon at startup.
    Devices,

    /// View or change daemon settings.
    Config {
        #[command(subcommand)]
        setting: ConfigCommand,
    },

    /// Stop the background daemon.
    Shutdown,
}

/// Job states that `msc list --state` can filter on. Mirrors `JobState`.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum StateFilter {
    Queued,
    Paused,
    Running,
    Finished,
    Killed,
    Failed,
}

/// Settings managed under `msc config <setting>`. New daemon-wide settings go
/// here so they share one namespace and show up together in `msc config --help`.
#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Get or set how many CPU (non-GPU) jobs may run at once.
    ///
    /// With no argument, prints the current limit. Pass a number to set it;
    /// `0` means unlimited. GPU jobs are bounded by the GPU pool, not this.
    CpuLimit {
        /// New limit (0 = unlimited). Omit to show the current value.
        limit: Option<u32>,
    },
}
