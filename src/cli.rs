use clap::{Parser, Subcommand};

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

        /// The command and its arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        argv: Vec<String>,
    },

    /// List all jobs and their state (this is the default when no command is given).
    List,

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

    /// Follow a job's output live. Press Ctrl+C to stop watching; the job keeps running.
    Watch {
        /// Job id.
        id: u32,
    },

    /// Kill a running job, or drop a job that is still queued.
    Kill {
        /// Job id.
        id: u32,
    },

    /// Remove a finished or queued job from the list.
    Remove {
        /// Job id.
        id: u32,
    },

    /// Remove all finished/killed/failed jobs from the list.
    Clear,

    /// Stop the background daemon.
    Shutdown,

    /// Internal: run the background daemon (not meant to be called directly).
    #[command(name = "__daemon", hide = true)]
    Daemon,
}
