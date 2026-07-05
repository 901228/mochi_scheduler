//! Whole–process-tree control for jobs.
//!
//! A job's command is usually a *chain* of processes, e.g.
//! `pixi run accelerate launch ... python`, where the program that actually does
//! the work (and holds the RAM / GPU) is a grandchild, not the direct child.
//! Terminating only the direct child — what `tokio::process::Child::start_kill`
//! does — leaves those grandchildren orphaned and still running. To kill a job
//! for real we must terminate its entire tree at once.
//!
//! - **Windows:** assign the child to a *Job Object* configured with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. `TerminateJobObject` kills every
//!   process in the job, and because the job dies once its last handle closes,
//!   the tree is also cleaned up automatically if the daemon itself exits.
//! - **Unix:** put the child in its own session/process group at spawn time
//!   (`setsid`) and later signal the whole group with `killpg`.
//!
//! Known asymmetry: on Unix a *daemon crash* does not reap running jobs (the
//! process group outlives it); only an explicit kill does. The Windows
//! kill-on-close behaviour covers both. (NOTE: the Unix path is currently
//! untested — see CLAUDE.md.)

use tokio::process::Command;

/// Apply spawn-time configuration required for later whole-tree kill.
///
/// Call this on the `Command` before spawning. On Windows the Job Object is set
/// up after spawn (see [`Guard::attach`]), so this is a no-op there; the caller
/// is still responsible for any other flags (e.g. `CREATE_NO_WINDOW`).
pub fn configure(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Put the child in a fresh session (hence its own process group) so we can
        // later signal every descendant with one `killpg`. `setsid` only fails if
        // the caller is already a group leader, which a just-forked child is not.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

#[cfg(windows)]
mod imp {
    use std::ptr;

    use tokio::process::Child;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
            TerminateJobObject,
        },
    };

    /// Owns a Job Object handle. A raw `HANDLE` is a pointer and thus not `Send`,
    /// but the handle is safe to use from any thread, so we assert `Send` to let
    /// it live across `.await` inside the scheduler's spawned task.
    struct JobHandle(HANDLE);
    unsafe impl Send for JobHandle {}

    impl Drop for JobHandle {
        fn drop(&mut self) {
            // With KILL_ON_JOB_CLOSE this also reaps any survivors if we close
            // while the job is still alive (e.g. the daemon shutting down).
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Holds the Job Object that the job's process tree belongs to.
    pub struct Guard {
        job: Option<JobHandle>,
    }

    impl Guard {
        /// Create a kill-on-close Job Object and assign the freshly spawned child
        /// to it. On any failure the guard is inert (`kill` becomes a no-op) and
        /// the job simply loses whole-tree cleanup rather than breaking the run.
        pub fn attach(child: &Child) -> Self {
            Guard {
                job: create_and_assign(child),
            }
        }

        /// Terminate every process in the job (the whole tree).
        pub fn kill(&self) {
            if let Some(job) = &self.job {
                unsafe { TerminateJobObject(job.0, 1) };
            }
        }
    }

    fn create_and_assign(child: &Child) -> Option<JobHandle> {
        let process = child.raw_handle()?;
        unsafe {
            let job = CreateJobObjectW(ptr::null(), ptr::null());
            if job.is_null() {
                return None;
            }
            // Kill the whole tree when the last handle to the job closes, so a
            // daemon crash also cleans up the job's processes.
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let info_ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            // `child.raw_handle()` and the Win32 HANDLE are both `*mut c_void`.
            if info_ok == 0 || AssignProcessToJobObject(job, process as HANDLE) == 0 {
                CloseHandle(job);
                return None;
            }
            Some(JobHandle(job))
        }
    }
}

#[cfg(unix)]
mod imp {
    use tokio::process::Child;

    /// Holds the child's process-group id so the whole group can be signalled.
    pub struct Guard {
        pgid: Option<i32>,
    }

    impl Guard {
        /// Capture the child's pgid. Thanks to `setsid` in [`super::configure`],
        /// the child is its own group leader, so its pgid equals its pid.
        pub fn attach(child: &Child) -> Self {
            Guard {
                pgid: child.id().map(|id| id as i32),
            }
        }

        /// Send `SIGKILL` to the whole process group, reaching every descendant.
        pub fn kill(&self) {
            if let Some(pgid) = self.pgid {
                unsafe { libc::killpg(pgid, libc::SIGKILL) };
            }
        }
    }
}

pub use imp::Guard;
