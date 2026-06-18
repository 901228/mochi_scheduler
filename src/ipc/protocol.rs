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
    Remove {
        id: u32,
    },
    Clear,
    Shutdown,
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
        };

        write_msg(&mut writer, &sent).await.unwrap();
        let got: Request = read_msg(&mut reader).await.unwrap();

        match got {
            Request::Add {
                argv,
                label,
                cwd,
                gpus,
            } => {
                assert_eq!(argv, vec!["echo".to_string(), "hi".to_string()]);
                assert_eq!(label.as_deref(), Some("x"));
                assert_eq!(cwd, PathBuf::from("/tmp"));
                assert_eq!(gpus, 2);
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
