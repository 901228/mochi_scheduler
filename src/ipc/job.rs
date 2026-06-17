use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Finished,
    Killed,
    /// The command could not be started, or the daemon died while it ran.
    Failed,
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobState::Finished | JobState::Killed | JobState::Failed
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Finished => "finished",
            JobState::Killed => "killed",
            JobState::Failed => "failed",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Job {
    pub id: u32,
    pub argv: Vec<String>,
    pub label: Option<String>,
    pub cwd: PathBuf,
    pub state: JobState,
    pub exit_code: Option<i32>,
    pub log_path: PathBuf,
    pub enqueued_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl Job {
    pub fn command_line(&self) -> String {
        self.argv.join(" ")
    }
}
