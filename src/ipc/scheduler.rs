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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A unique path under the OS temp dir, so parallel tests don't collide.
    fn temp_state_file() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("mochi_test_{}_{n}.json", std::process::id()))
    }

    /// `add` only joins the log dir into a path; it never touches the filesystem,
    /// so an arbitrary placeholder is fine for the in-memory tests.
    fn log_dir() -> PathBuf {
        PathBuf::from("logs")
    }

    fn enqueue(state: &mut AppState, argv: &str) -> u32 {
        state.add(&log_dir(), vec![argv.to_string()], None, PathBuf::from("."))
    }

    #[test]
    fn add_assigns_sequential_ids_and_queues() {
        let mut s = AppState::default();
        let id0 = s.add(&log_dir(), vec!["echo".into(), "hi".into()], None, ".".into());
        let id1 = s.add(&log_dir(), vec!["ls".into()], Some("list".into()), ".".into());

        assert_eq!((id0, id1), (0, 1));
        let j0 = s.get(0).unwrap();
        assert_eq!(j0.state, JobState::Queued);
        assert_eq!(j0.log_path, log_dir().join("0.log"));
        assert_eq!(s.get(1).unwrap().label.as_deref(), Some("list"));
    }

    #[test]
    fn take_next_queued_picks_lowest_id_and_marks_running() {
        let mut s = AppState::default();
        enqueue(&mut s, "a");
        enqueue(&mut s, "b");

        let spec = s.take_next_queued().unwrap();
        assert_eq!(spec.id, 0);
        let j0 = s.get(0).unwrap();
        assert_eq!(j0.state, JobState::Running);
        assert!(j0.started_at.is_some());

        assert_eq!(s.take_next_queued().unwrap().id, 1);
        assert!(s.take_next_queued().is_none());
    }

    #[test]
    fn finish_sets_state_per_result() {
        let mut s = AppState::default();
        enqueue(&mut s, "a");
        enqueue(&mut s, "b");
        enqueue(&mut s, "c");
        s.take_next_queued();
        s.take_next_queued();
        s.take_next_queued();

        s.finish(0, RunResult::Exited(Some(3)));
        s.finish(1, RunResult::Killed);
        s.finish(2, RunResult::SpawnFailed);

        let j0 = s.get(0).unwrap();
        assert_eq!(j0.state, JobState::Finished);
        assert_eq!(j0.exit_code, Some(3));
        assert!(j0.finished_at.is_some());
        assert_eq!(s.get(1).unwrap().state, JobState::Killed);
        assert_eq!(s.get(2).unwrap().state, JobState::Failed);
    }

    #[test]
    fn request_kill_covers_every_outcome() {
        let mut s = AppState::default();
        enqueue(&mut s, "a"); // 0
        enqueue(&mut s, "b"); // 1

        // A queued job is dropped immediately and marked killed.
        assert!(matches!(s.request_kill(0), KillOutcome::Dequeued));
        assert_eq!(s.get(0).unwrap().state, JobState::Killed);

        // A running job needs the daemon to signal its kill switch.
        s.take_next_queued(); // picks id 1, since 0 is terminal
        assert!(matches!(s.request_kill(1), KillOutcome::Running));

        s.finish(1, RunResult::Exited(Some(0)));
        assert!(matches!(s.request_kill(1), KillOutcome::AlreadyDone));
        assert!(matches!(s.request_kill(99), KillOutcome::NotFound));
    }

    #[test]
    fn remove_covers_every_outcome() {
        let mut s = AppState::default();
        enqueue(&mut s, "a");
        s.take_next_queued(); // 0 running

        assert!(matches!(s.remove(0), RemoveOutcome::Running));
        s.finish(0, RunResult::Exited(Some(0)));
        assert!(matches!(s.remove(0), RemoveOutcome::Removed));
        assert!(s.get(0).is_none());
        assert!(matches!(s.remove(0), RemoveOutcome::NotFound));
    }

    #[test]
    fn clear_finished_keeps_running_and_queued() {
        let mut s = AppState::default();
        enqueue(&mut s, "a"); // 0
        enqueue(&mut s, "b"); // 1
        enqueue(&mut s, "c"); // 2
        s.take_next_queued(); // 0 running
        s.finish(0, RunResult::Exited(Some(0))); // 0 finished
        s.take_next_queued(); // 1 running, left running

        assert_eq!(s.clear_finished(), 1);
        assert!(s.get(0).is_none()); // terminal -> removed
        assert!(s.get(1).is_some()); // running -> kept
        assert!(s.get(2).is_some()); // queued -> kept
    }

    #[test]
    fn save_then_load_preserves_jobs_and_next_id() {
        let mut s = AppState::default();
        s.add(
            &log_dir(),
            vec!["echo".into(), "hi".into()],
            Some("greet".into()),
            ".".into(),
        );
        let path = temp_state_file();
        s.save(&path).unwrap();

        let mut loaded = AppState::load(&path).unwrap();
        assert_eq!(loaded.get(0).unwrap().label.as_deref(), Some("greet"));
        // next_id is persisted, so a fresh add keeps climbing instead of reusing 0.
        assert_eq!(enqueue(&mut loaded, "next"), 1);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_reconciles_running_jobs_to_failed() {
        let mut s = AppState::default();
        enqueue(&mut s, "a");
        s.take_next_queued(); // 0 running when the daemon "dies"
        let path = temp_state_file();
        s.save(&path).unwrap();

        let loaded = AppState::load(&path).unwrap();
        let j = loaded.get(0).unwrap();
        assert_eq!(j.state, JobState::Failed);
        assert!(j.finished_at.is_some());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_missing_file_yields_empty_state() {
        let s = AppState::load(&temp_state_file()).unwrap();
        assert!(s.list().is_empty());
    }
}
