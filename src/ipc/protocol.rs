use std::path::PathBuf;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::job::Job;

/// A request sent from the client to the daemon.
#[derive(Serialize, Deserialize, Debug)]
pub enum Request {
    Add {
        argv: Vec<String>,
        label: Option<String>,
        cwd: PathBuf,
        gpus: u32,
        /// Scheduling priority; higher runs first, ties break by id.
        priority: i32,
        /// The client's full environment at `add` time, so the job runs with the
        /// same PATH/env the user had in their shell (e.g. a pixi/venv/conda
        /// activation) rather than the daemon's frozen environment.
        env: Vec<(String, String)>,
    },
    List,
    Info {
        id: u32,
    },
    Cat {
        id: u32,
    },
    Kill {
        id: u32,
    },
    /// Kill every running job and drop every queued one.
    KillAll,
    /// Change a queued job's scheduling priority.
    SetPriority {
        id: u32,
        priority: i32,
    },
    /// Re-queue an existing job as a new job, re-running its command. The fresh
    /// copy is enqueued at `priority` (defaults to 0 on the client side, not the
    /// source job's priority).
    Rerun {
        id: u32,
        priority: i32,
    },
    /// Restart a running job in place: kill its process and re-queue the same job
    /// (same id and log file) so it runs again.
    Restart {
        id: u32,
    },
    Remove {
        id: u32,
    },
    Clear,
    /// Move every queued job to the paused state at once.
    PauseAllQueued,
    /// Move every paused job back into the queue at once.
    ResumeAllPaused,
    /// Pull a single queued job out of the queue until it is resumed.
    PauseJob {
        id: u32,
    },
    /// Put a paused job back into the queue.
    ResumeJob {
        id: u32,
    },
    Shutdown,
    /// Query the GPU devices detected by the daemon at startup.
    GetDevices,
    /// Read the current CPU-job concurrency limit.
    GetCpuLimit,
    /// Set the CPU-job concurrency limit (`None` = unlimited).
    SetCpuLimit {
        limit: Option<u32>,
    },
}

/// A response sent from the daemon back to the client.
#[derive(Serialize, Deserialize, Debug)]
pub enum Response {
    Ok(String),
    Error(String),

    Jobs(Vec<Job>),
    Job(Job),
    /// Path to a job's log file (resolved by the client, which reads it directly).
    LogPath(PathBuf),
}

/// Largest message we are willing to read, to guard against bad framing.
const MAX_MSG_LEN: usize = 16 * 1024 * 1024;

/// Write a length-prefixed, JSON-encoded message.
pub async fn write_msg<S, T>(stream: &mut S, msg: &T) -> anyhow::Result<()>
where
    S: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(msg)?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

/// Read a length-prefixed, JSON-encoded message.
pub async fn read_msg<S, T>(stream: &mut S) -> anyhow::Result<T>
where
    S: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_MSG_LEN {
        return Err(anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message too large",
        )));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf)
        .map_err(|e| anyhow::Error::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_then_read_roundtrips_a_message() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        let sent = Request::Add {
            argv: vec!["echo".into(), "hi".into()],
            label: Some("x".into()),
            cwd: PathBuf::from("/tmp"),
            gpus: 2,
            priority: 5,
            env: vec![("KEY".into(), "VALUE".into())],
        };

        write_msg(&mut writer, &sent).await.unwrap();
        let got: Request = read_msg(&mut reader).await.unwrap();

        match got {
            Request::Add {
                argv,
                label,
                cwd,
                gpus,
                priority,
                env,
            } => {
                assert_eq!(argv, vec!["echo".to_string(), "hi".to_string()]);
                assert_eq!(label.as_deref(), Some("x"));
                assert_eq!(cwd, PathBuf::from("/tmp"));
                assert_eq!(gpus, 2);
                assert_eq!(priority, 5);
                assert_eq!(env, vec![("KEY".to_string(), "VALUE".to_string())]);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_rejects_an_oversized_frame() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        // A length prefix past the cap must fail before any body is read.
        let len = (MAX_MSG_LEN as u32 + 1).to_be_bytes();
        writer.write_all(&len).await.unwrap();
        writer.flush().await.unwrap();

        let res: anyhow::Result<Request> = read_msg(&mut reader).await;
        assert!(res.is_err());
    }
}
