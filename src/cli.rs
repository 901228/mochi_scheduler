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
    List {
        /// Show jobs in every state.
        #[arg(short, long, conflicts_with = "state")]
        all: bool,

        /// Only show jobs in these states (repeatable), e.g. `-s finished -s failed`.
        #[arg(short, long, value_enum)]
        state: Vec<StateFilter>,
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
    Watch {
        /// Job id. Omit to auto-pick / list the running jobs.
        id: Option<u32>,
    },

    /// Kill a running job, or drop a job that is still queued.
    Kill {
        /// Job id (omit when using `--all`).
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        id: Option<u32>,

        /// Kill every running job and drop every queued one.
        #[arg(long)]
        all: bool,
    },

    /// Change the priority of a queued job to let it jump the queue.
    Priority {
        /// Job id.
        id: u32,

        /// New priority; higher runs first (default 0).
        #[arg(allow_hyphen_values = true)]
        priority: i32,
    },

    /// Re-run a job: queue a fresh copy of it (same command, dir, and environment).
    Rerun {
        /// Job id to re-run.
        id: u32,
    },

    /// Remove a finished or queued job from the list.
    Remove {
        /// Job id.
        id: u32,
    },

    /// Remove all finished/killed/failed jobs from the list.
    Clear,

    /// View or change daemon settings.
    Config {
        #[command(subcommand)]
        setting: ConfigCommand,
    },

    /// Stop the background daemon.
    Shutdown,

    /// Internal: run the background daemon (not meant to be called directly).
    #[command(name = "__daemon", hide = true)]
    Daemon,
}

/// Job states that `msc list --state` can filter on. Mirrors `JobState`.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum StateFilter {
    Queued,
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
