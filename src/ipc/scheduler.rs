use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Context;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::job::{Job, JobState};

/// What the scheduler needs in order to actually launch a job.
pub struct RunSpec {
    pub id: u32,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub log_path: PathBuf,
}

/// The outcome of running a job.
pub enum RunResult {
    Exited(Option<i32>),
    Killed,
    /// The command could not be started; details are written to the job's log.
    SpawnFailed,
}

pub enum KillOutcome {
    Running,
    Dequeued,
    AlreadyDone,
    NotFound,
}

pub enum RemoveOutcome {
    Removed,
    Running,
    NotFound,
}

/// The full in-memory state of the queue, persisted to disk as JSON.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct AppState {
    next_id: u32,
    jobs: BTreeMap<u32, Job>,
}

/// recovery app state from disk
impl AppState {
    /// Load persisted state, reconciling any jobs that were mid-run when the daemon last stopped (their child processes are gone, so mark them failed).
    pub fn load(state_file: &PathBuf) -> anyhow::Result<Self> {
        let mut state: AppState = match std::fs::read(state_file) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", state_file.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => AppState::default(),
            Err(e) => return Err(e).context("reading state file"),
        };

        let now = Utc::now();
        for job in state.jobs.values_mut() {
            if job.state == JobState::Running {
                job.state = JobState::Failed;
                job.finished_at = Some(now);
            }
        }
        Ok(state)
    }

    /// Write to a temp file and rename, so a crash never leaves a half-written state file behind.
    pub fn save(&self, state_file: &PathBuf) -> anyhow::Result<()> {
        let tmp = state_file.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, state_file)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), state_file.display()))?;
        Ok(())
    }
}

/// schedule
impl AppState {
    pub fn add(&mut self, log_dir: &PathBuf, argv: Vec<String>, label: Option<String>, cwd: PathBuf) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let job = Job {
            id,
            argv,
            label,
            cwd,
            state: JobState::Queued,
            exit_code: None,
            log_path: log_dir.join(format!("{id}.log")),
            enqueued_at: Utc::now(),
            started_at: None,
            finished_at: None,
        };
        self.jobs.insert(id, job);
        id
    }

    pub fn list(&self) -> Vec<Job> {
        self.jobs.values().cloned().collect()
    }

    pub fn get(&self, id: u32) -> Option<Job> {
        self.jobs.get(&id).cloned()
    }

    /// Pick the lowest-id queued job, mark it running, and return its run spec.
    pub fn take_next_queued(&mut self) -> Option<RunSpec> {
        let id = self
            .jobs
            .values()
            .find(|j| j.state == JobState::Queued)
            .map(|j| j.id)?;
        let job = self.jobs.get_mut(&id).expect("job exists");
        job.state = JobState::Running;
        job.started_at = Some(Utc::now());
        Some(RunSpec {
            id,
            argv: job.argv.clone(),
            cwd: job.cwd.clone(),
            log_path: job.log_path.clone(),
        })
    }

    pub fn finish(&mut self, id: u32, result: RunResult) {
        if let Some(job) = self.jobs.get_mut(&id) {
            job.finished_at = Some(Utc::now());
            match result {
                RunResult::Exited(code) => {
                    job.exit_code = code;
                    job.state = JobState::Finished;
                }
                RunResult::Killed => job.state = JobState::Killed,
                RunResult::SpawnFailed => job.state = JobState::Failed,
            }
        }
    }

    /// Returns true if the job is currently running (so the caller can signal a kill).
    pub fn request_kill(&mut self, id: u32) -> KillOutcome {
        match self.jobs.get_mut(&id) {
            None => KillOutcome::NotFound,
            Some(job) => match job.state {
                JobState::Running => KillOutcome::Running,
                JobState::Queued => {
                    job.state = JobState::Killed;
                    job.finished_at = Some(Utc::now());
                    KillOutcome::Dequeued
                }
                _ => KillOutcome::AlreadyDone,
            },
        }
    }

    pub fn remove(&mut self, id: u32) -> RemoveOutcome {
        match self.jobs.get(&id) {
            None => RemoveOutcome::NotFound,
            Some(job) if job.state == JobState::Running => RemoveOutcome::Running,
            Some(_) => {
                self.jobs.remove(&id);
                RemoveOutcome::Removed
            }
        }
    }

    /// Remove all jobs in a terminal state. Returns how many were removed.
    pub fn clear_finished(&mut self) -> usize {
        let before = self.jobs.len();
        self.jobs.retain(|_, j| !j.state.is_terminal());
        before - self.jobs.len()
    }
}
