use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

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
    /// GPU indices reserved for this job; passed to the child as visible devices.
    pub assigned_gpus: Vec<u32>,
    /// Environment to run the job under (captured from the client at `add` time).
    pub env: Vec<(String, String)>,
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

pub enum SetPriorityOutcome {
    Updated,
    /// The job exists but isn't queued (already running or terminal), so
    /// re-prioritising it has no effect.
    NotQueued,
    NotFound,
}

/// What `cancel_all` touched: the running jobs whose kill switch the daemon must
/// still fire, and how many queued jobs were dropped.
pub struct CancelAll {
    pub running: Vec<u32>,
    pub dequeued: usize,
}

/// The full in-memory state of the queue, persisted to disk as JSON.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct AppState {
    next_id: u32,
    jobs: BTreeMap<u32, Job>,
    /// Max number of CPU (0-GPU) jobs allowed to run at once. `None` = unlimited.
    /// GPU jobs are bounded by the GPU pool instead and ignore this. `serde(default)`
    /// keeps older state files loadable.
    #[serde(default)]
    cpu_limit: Option<u32>,
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
    pub fn add(
        &mut self,
        log_dir: &PathBuf,
        argv: Vec<String>,
        label: Option<String>,
        cwd: PathBuf,
        gpus: u32,
        priority: i32,
        env: Vec<(String, String)>,
    ) -> u32 {
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
            priority,
            gpus,
            assigned_gpus: Vec::new(),
            env,
        };
        self.jobs.insert(id, job);
        id
    }

    /// Re-queue an existing job as a fresh job (new id, new log file), copying
    /// its command, working dir, GPU request, priority, label, and the
    /// environment captured at the original `add`. The source job's record is
    /// left untouched. Returns the new id, or `None` if no such job exists.
    pub fn rerun(&mut self, log_dir: &PathBuf, id: u32) -> Option<u32> {
        let job = self.jobs.get(&id)?;
        // Clone the fields first so the immutable borrow ends before `add`.
        let (argv, label, cwd, gpus, priority, env) = (
            job.argv.clone(),
            job.label.clone(),
            job.cwd.clone(),
            job.gpus,
            job.priority,
            job.env.clone(),
        );
        Some(self.add(log_dir, argv, label, cwd, gpus, priority, env))
    }

    pub fn list(&self) -> Vec<Job> {
        self.jobs.values().cloned().collect()
    }

    pub fn get(&self, id: u32) -> Option<Job> {
        self.jobs.get(&id).cloned()
    }

    /// GPU indices that are currently free: the full pool `0..gpu_total` minus
    /// every index held by a running job.
    fn free_gpus(&self, gpu_total: u32) -> BTreeSet<u32> {
        let mut free: BTreeSet<u32> = (0..gpu_total).collect();
        for job in self.jobs.values() {
            if job.state == JobState::Running {
                for idx in &job.assigned_gpus {
                    free.remove(idx);
                }
            }
        }
        free
    }

    /// Number of CPU (0-GPU) jobs currently running, used to enforce `cpu_limit`.
    fn running_cpu_count(&self) -> u32 {
        self.jobs
            .values()
            .filter(|j| j.state == JobState::Running && j.gpus == 0)
            .count() as u32
    }

    /// The configured CPU-job concurrency cap (`None` = unlimited).
    pub fn cpu_limit(&self) -> Option<u32> {
        self.cpu_limit
    }

    /// Set the CPU-job concurrency cap (`None` = unlimited).
    pub fn set_cpu_limit(&mut self, limit: Option<u32>) {
        self.cpu_limit = limit;
    }

    /// Pick the next queued job that fits the free GPU pool, reserve its GPUs,
    /// mark it running, and return its run spec.
    ///
    /// Among the queued jobs that currently fit, the highest `priority` wins and
    /// id breaks ties (lowest first). A job that doesn't currently fit is skipped
    /// rather than blocking the queue (greedy backfill): a smaller / higher-fitting
    /// job can start ahead of one still waiting for capacity. A GPU job fits when
    /// enough GPUs are free; a CPU (0-GPU) job fits while the number of running
    /// CPU jobs is below `cpu_limit`. Returns `None` when nothing fits, so the
    /// daemon can call this repeatedly to fill all available capacity.
    pub fn take_next_runnable(&mut self, gpu_total: u32) -> Option<RunSpec> {
        let free = self.free_gpus(gpu_total);
        let cpu_has_room = self
            .cpu_limit
            .is_none_or(|limit| self.running_cpu_count() < limit);

        let id = self
            .jobs
            .values()
            .filter(|j| {
                j.state == JobState::Queued
                    && if j.gpus == 0 {
                        cpu_has_room
                    } else {
                        j.gpus as usize <= free.len()
                    }
            })
            // Higher priority first (negate), then lowest id.
            .min_by_key(|j| (-j.priority, j.id))
            .map(|j| j.id)?;

        let assigned: Vec<u32> = free.into_iter().take(self.jobs[&id].gpus as usize).collect();
        let job = self.jobs.get_mut(&id).expect("job exists");
        job.state = JobState::Running;
        job.started_at = Some(Utc::now());
        job.assigned_gpus = assigned.clone();
        Some(RunSpec {
            id,
            argv: job.argv.clone(),
            cwd: job.cwd.clone(),
            log_path: job.log_path.clone(),
            assigned_gpus: assigned,
            env: job.env.clone(),
        })
    }

    pub fn finish(&mut self, id: u32, result: RunResult) {
        if let Some(job) = self.jobs.get_mut(&id) {
            job.finished_at = Some(Utc::now());
            // Release the job's GPUs back into the pool.
            job.assigned_gpus.clear();
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

    /// Change a queued job's priority. Only queued jobs can be re-prioritised; a
    /// running job has already started and a terminal one is done.
    pub fn set_priority(&mut self, id: u32, priority: i32) -> SetPriorityOutcome {
        match self.jobs.get_mut(&id) {
            None => SetPriorityOutcome::NotFound,
            Some(job) if job.state == JobState::Queued => {
                job.priority = priority;
                SetPriorityOutcome::Updated
            }
            Some(_) => SetPriorityOutcome::NotQueued,
        }
    }

    /// Cancel every active job: drop all queued jobs (mark them killed) and
    /// report the running ones so the daemon can fire their kill switches. The
    /// running jobs are transitioned to `Killed` by `finish` once their child
    /// actually exits, mirroring single-job `kill`.
    pub fn cancel_all(&mut self) -> CancelAll {
        let now = Utc::now();
        let mut running = Vec::new();
        let mut dequeued = 0;
        for job in self.jobs.values_mut() {
            match job.state {
                JobState::Running => running.push(job.id),
                JobState::Queued => {
                    job.state = JobState::Killed;
                    job.finished_at = Some(now);
                    dequeued += 1;
                }
                _ => {}
            }
        }
        CancelAll { running, dequeued }
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
        enqueue_gpu(state, argv, 0)
    }

    fn enqueue_gpu(state: &mut AppState, argv: &str, gpus: u32) -> u32 {
        state.add(
            &log_dir(),
            vec![argv.to_string()],
            None,
            PathBuf::from("."),
            gpus,
            0,
            Vec::new(),
        )
    }

    fn enqueue_prio(state: &mut AppState, argv: &str, priority: i32) -> u32 {
        state.add(
            &log_dir(),
            vec![argv.to_string()],
            None,
            PathBuf::from("."),
            0,
            priority,
            Vec::new(),
        )
    }

    #[test]
    fn add_assigns_sequential_ids_and_queues() {
        let mut s = AppState::default();
        let id0 = s.add(
            &log_dir(),
            vec!["echo".into(), "hi".into()],
            None,
            ".".into(),
            0,
            0,
            Vec::new(),
        );
        let id1 = s.add(
            &log_dir(),
            vec!["ls".into()],
            Some("list".into()),
            ".".into(),
            0,
            0,
            Vec::new(),
        );

        assert_eq!((id0, id1), (0, 1));
        let j0 = s.get(0).unwrap();
        assert_eq!(j0.state, JobState::Queued);
        assert_eq!(j0.log_path, log_dir().join("0.log"));
        assert_eq!(s.get(1).unwrap().label.as_deref(), Some("list"));
    }

    #[test]
    fn take_next_runnable_picks_lowest_id_and_marks_running() {
        let mut s = AppState::default();
        enqueue(&mut s, "a");
        enqueue(&mut s, "b");

        let spec = s.take_next_runnable(0).unwrap();
        assert_eq!(spec.id, 0);
        let j0 = s.get(0).unwrap();
        assert_eq!(j0.state, JobState::Running);
        assert!(j0.started_at.is_some());

        assert_eq!(s.take_next_runnable(0).unwrap().id, 1);
        assert!(s.take_next_runnable(0).is_none());
    }

    #[test]
    fn finish_sets_state_per_result() {
        let mut s = AppState::default();
        enqueue(&mut s, "a");
        enqueue(&mut s, "b");
        enqueue(&mut s, "c");
        s.take_next_runnable(0);
        s.take_next_runnable(0);
        s.take_next_runnable(0);

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
        s.take_next_runnable(0); // picks id 1, since 0 is terminal
        assert!(matches!(s.request_kill(1), KillOutcome::Running));

        s.finish(1, RunResult::Exited(Some(0)));
        assert!(matches!(s.request_kill(1), KillOutcome::AlreadyDone));
        assert!(matches!(s.request_kill(99), KillOutcome::NotFound));
    }

    #[test]
    fn remove_covers_every_outcome() {
        let mut s = AppState::default();
        enqueue(&mut s, "a");
        s.take_next_runnable(0); // 0 running

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
        s.take_next_runnable(0); // 0 running
        s.finish(0, RunResult::Exited(Some(0))); // 0 finished
        s.take_next_runnable(0); // 1 running, left running

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
            0,
            0,
            Vec::new(),
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
        s.take_next_runnable(0); // 0 running when the daemon "dies"
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

    #[test]
    fn gpu_jobs_run_concurrently_until_pool_is_exhausted() {
        let mut s = AppState::default();
        enqueue_gpu(&mut s, "a", 1); // 0
        enqueue_gpu(&mut s, "b", 1); // 1
        enqueue_gpu(&mut s, "c", 1); // 2

        // Pool of 2: the first two each grab a distinct GPU.
        let a = s.take_next_runnable(2).unwrap();
        let b = s.take_next_runnable(2).unwrap();
        assert_eq!(a.assigned_gpus, vec![0]);
        assert_eq!(b.assigned_gpus, vec![1]);
        // Pool is full, so the third job stays queued.
        assert!(s.take_next_runnable(2).is_none());
        assert_eq!(s.get(2).unwrap().state, JobState::Queued);

        // Finishing one releases its GPU, which the waiting job then reuses.
        s.finish(0, RunResult::Exited(Some(0)));
        let c = s.take_next_runnable(2).unwrap();
        assert_eq!(c.id, 2);
        assert_eq!(c.assigned_gpus, vec![0]);
    }

    #[test]
    fn multi_gpu_job_reserves_contiguous_lowest_indices() {
        let mut s = AppState::default();
        enqueue_gpu(&mut s, "big", 3);
        let spec = s.take_next_runnable(4).unwrap();
        assert_eq!(spec.assigned_gpus, vec![0, 1, 2]);
        assert_eq!(s.get(0).unwrap().assigned_gpus, vec![0, 1, 2]);
    }

    #[test]
    fn backfill_runs_smaller_job_ahead_of_a_blocked_larger_one() {
        let mut s = AppState::default();
        enqueue_gpu(&mut s, "needs-two", 2); // 0, won't fit in 1 free GPU
        enqueue_gpu(&mut s, "needs-one", 1); // 1, fits
        enqueue(&mut s, "no-gpu"); // 2, always fits

        // Only 1 GPU total: job 0 is skipped, job 1 takes the GPU.
        let first = s.take_next_runnable(1).unwrap();
        assert_eq!(first.id, 1);
        assert_eq!(first.assigned_gpus, vec![0]);

        // The 0-GPU job still runs even though job 0 is blocked.
        let second = s.take_next_runnable(1).unwrap();
        assert_eq!(second.id, 2);
        assert!(second.assigned_gpus.is_empty());

        // Job 0 remains queued, waiting for the GPU to free up.
        assert!(s.take_next_runnable(1).is_none());
        assert_eq!(s.get(0).unwrap().state, JobState::Queued);
    }

    #[test]
    fn captured_env_flows_through_to_the_run_spec() {
        let mut s = AppState::default();
        let env = vec![("PATH".to_string(), "/pixi/bin".to_string())];
        s.add(
            &log_dir(),
            vec!["python".into()],
            None,
            ".".into(),
            0,
            0,
            env.clone(),
        );

        let spec = s.take_next_runnable(0).unwrap();
        assert_eq!(spec.env, env);
    }

    #[test]
    fn finish_releases_gpus_back_into_the_pool() {
        let mut s = AppState::default();
        enqueue_gpu(&mut s, "a", 2);
        s.take_next_runnable(2);
        assert!(s.get(0).unwrap().assigned_gpus == vec![0, 1]);

        s.finish(0, RunResult::Exited(Some(0)));
        assert!(s.get(0).unwrap().assigned_gpus.is_empty());
        // Both GPUs are free again for a fresh job.
        let id = enqueue_gpu(&mut s, "b", 2);
        let spec = s.take_next_runnable(2).unwrap();
        assert_eq!(spec.id, id);
        assert_eq!(spec.assigned_gpus, vec![0, 1]);
    }

    #[test]
    fn cpu_limit_caps_concurrent_cpu_jobs() {
        let mut s = AppState::default();
        s.set_cpu_limit(Some(2));
        enqueue(&mut s, "a"); // 0
        enqueue(&mut s, "b"); // 1
        enqueue(&mut s, "c"); // 2

        assert_eq!(s.take_next_runnable(0).unwrap().id, 0);
        assert_eq!(s.take_next_runnable(0).unwrap().id, 1);
        // Limit of 2 reached, so the third stays queued.
        assert!(s.take_next_runnable(0).is_none());
        assert_eq!(s.get(2).unwrap().state, JobState::Queued);

        // Finishing one frees a CPU slot for the waiting job.
        s.finish(0, RunResult::Exited(Some(0)));
        assert_eq!(s.take_next_runnable(0).unwrap().id, 2);
    }

    #[test]
    fn cpu_limit_does_not_throttle_gpu_jobs() {
        let mut s = AppState::default();
        s.set_cpu_limit(Some(1));
        enqueue_gpu(&mut s, "cpu", 0); // 0
        enqueue_gpu(&mut s, "g1", 1); // 1
        enqueue_gpu(&mut s, "g2", 1); // 2

        // One CPU job runs (limit 1); GPU jobs run per the GPU pool regardless.
        assert_eq!(s.take_next_runnable(2).unwrap().id, 0);
        assert_eq!(s.take_next_runnable(2).unwrap().id, 1);
        assert_eq!(s.take_next_runnable(2).unwrap().id, 2);
        assert!(s.take_next_runnable(2).is_none());
    }

    #[test]
    fn higher_priority_job_runs_first_even_if_queued_later() {
        let mut s = AppState::default();
        enqueue(&mut s, "low"); // 0, priority 0
        enqueue_prio(&mut s, "high", 5); // 1, jumps ahead
        enqueue_prio(&mut s, "mid", 1); // 2

        assert_eq!(s.take_next_runnable(0).unwrap().id, 1); // highest priority
        assert_eq!(s.take_next_runnable(0).unwrap().id, 2); // next highest
        assert_eq!(s.take_next_runnable(0).unwrap().id, 0); // default priority last
    }

    #[test]
    fn equal_priority_breaks_ties_by_id() {
        let mut s = AppState::default();
        enqueue_prio(&mut s, "a", 3); // 0
        enqueue_prio(&mut s, "b", 3); // 1

        assert_eq!(s.take_next_runnable(0).unwrap().id, 0);
        assert_eq!(s.take_next_runnable(0).unwrap().id, 1);
    }

    #[test]
    fn set_priority_lets_a_queued_job_jump_the_queue() {
        let mut s = AppState::default();
        enqueue(&mut s, "a"); // 0
        enqueue(&mut s, "b"); // 1
        enqueue(&mut s, "c"); // 2

        // Bump the last job above the others; it now runs first.
        assert!(matches!(s.set_priority(2, 10), SetPriorityOutcome::Updated));
        assert_eq!(s.take_next_runnable(0).unwrap().id, 2);
        assert_eq!(s.take_next_runnable(0).unwrap().id, 0);
    }

    #[test]
    fn set_priority_covers_non_queued_and_missing_jobs() {
        let mut s = AppState::default();
        enqueue(&mut s, "a"); // 0
        s.take_next_runnable(0); // 0 now running

        assert!(matches!(s.set_priority(0, 5), SetPriorityOutcome::NotQueued));
        assert!(matches!(s.set_priority(99, 5), SetPriorityOutcome::NotFound));
    }

    #[test]
    fn cancel_all_kills_running_and_drops_queued() {
        let mut s = AppState::default();
        enqueue(&mut s, "a"); // 0
        enqueue(&mut s, "b"); // 1
        enqueue(&mut s, "c"); // 2
        s.take_next_runnable(0); // 0 running
        s.take_next_runnable(0); // 1 running
        s.finish(0, RunResult::Exited(Some(0))); // 0 finished (terminal, untouched)

        let outcome = s.cancel_all();
        // Job 1 is running -> reported for the daemon to signal; not yet killed.
        assert_eq!(outcome.running, vec![1]);
        assert_eq!(s.get(1).unwrap().state, JobState::Running);
        // Job 2 was queued -> dropped immediately.
        assert_eq!(outcome.dequeued, 1);
        assert_eq!(s.get(2).unwrap().state, JobState::Killed);
        // The already-finished job is left as-is.
        assert_eq!(s.get(0).unwrap().state, JobState::Finished);
    }

    #[test]
    fn rerun_clones_job_into_a_fresh_queued_job() {
        let mut s = AppState::default();
        let env = vec![("PATH".to_string(), "/pixi/bin".to_string())];
        let id = s.add(
            &log_dir(),
            vec!["echo".into(), "hi".into()],
            Some("greet".into()),
            PathBuf::from("/work"),
            2,
            5,
            env.clone(),
        );
        s.take_next_runnable(4);
        s.finish(id, RunResult::Exited(Some(1))); // original is now terminal

        let new_id = s.rerun(&log_dir(), id).unwrap();
        assert_ne!(new_id, id);

        // Original record is untouched.
        let orig = s.get(id).unwrap();
        assert_eq!(orig.state, JobState::Finished);
        assert_eq!(orig.exit_code, Some(1));

        // New job is a fresh queued clone with its own log file.
        let new = s.get(new_id).unwrap();
        assert_eq!(new.state, JobState::Queued);
        assert_eq!(new.argv, vec!["echo".to_string(), "hi".to_string()]);
        assert_eq!(new.label.as_deref(), Some("greet"));
        assert_eq!(new.cwd, PathBuf::from("/work"));
        assert_eq!(new.gpus, 2);
        assert_eq!(new.priority, 5);
        assert_eq!(new.env, env);
        assert_eq!(new.exit_code, None);
        assert!(new.started_at.is_none());
        assert_ne!(new.log_path, orig.log_path);
    }

    #[test]
    fn rerun_missing_job_returns_none() {
        let mut s = AppState::default();
        assert!(s.rerun(&log_dir(), 42).is_none());
    }

    #[test]
    fn cpu_limit_lets_a_gpu_job_backfill_past_a_blocked_cpu_job() {
        let mut s = AppState::default();
        s.set_cpu_limit(Some(1));
        enqueue(&mut s, "cpu1"); // 0
        enqueue(&mut s, "cpu2"); // 1
        enqueue_gpu(&mut s, "g", 1); // 2

        assert_eq!(s.take_next_runnable(2).unwrap().id, 0); // cpu1 fills the 1 CPU slot
        assert_eq!(s.take_next_runnable(2).unwrap().id, 2); // cpu2 blocked, GPU job backfills
        assert!(s.take_next_runnable(2).is_none());
        assert_eq!(s.get(1).unwrap().state, JobState::Queued);
    }
}
