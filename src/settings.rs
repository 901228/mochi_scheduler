use std::path::PathBuf;

use anyhow::Context;
use directories::ProjectDirs;
use interprocess::local_socket::{GenericFilePath, GenericNamespaced, Name, tokio::prelude::*};

/// Resolved filesystem locations and the local-socket name used by msc.
///
/// Everything lives under a single data directory, which can be overridden with
/// the `MOCHI_HOME` environment variable (useful for tests and for keeping separate queues).
#[derive(Debug, Clone)]
pub struct Settings {
    pub data_dir: PathBuf,
    pub state_file: PathBuf,
    pub log_dir: PathBuf,
    /// Name used for the namespaced socket (Linux abstract socket / Windows named pipe).
    pub socket_ns: String,
    /// Filesystem path used when namespaced sockets are not supported (e.g. macOS).
    pub socket_fs: PathBuf,
}

impl Settings {
    pub fn resolve() -> anyhow::Result<Self> {
        let data_dir = match std::env::var_os("MOCHI_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => ProjectDirs::from("org", "mochi", "msc")
                .context("could not determine a home directory for mochi")?
                .data_dir()
                .to_path_buf(),
        };

        let log_dir = data_dir.join("logs");
        std::fs::create_dir_all(&log_dir)
            .with_context(|| format!("creating data directory {}", log_dir.display()))?;

        // Make the socket name per-user so multiple accounts on one machine do
        // not collide (Windows named pipes share a global namespace).
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "default".to_string());
        let socket_ns = format!("mochi-{user}.sock");
        let socket_fs = data_dir.join(format!("mochi-{user}.sock"));

        Ok(Self {
            state_file: data_dir.join("state.json"),
            log_dir,
            socket_ns,
            socket_fs,
            data_dir,
        })
    }

    pub fn socket_name_display(&self) -> &str {
        if GenericNamespaced::is_supported() {
            self.socket_ns.as_str()
        } else {
            self.socket_fs.to_str().unwrap()
        }
    }

    pub fn socket_name(&self) -> anyhow::Result<Name<'_>> {
        if GenericNamespaced::is_supported() {
            self.socket_ns.as_str().to_ns_name::<GenericNamespaced>()
        } else {
            self.socket_fs.as_path().to_fs_name::<GenericFilePath>()
        }
        .context("Cannot get the socket name")
    }
}
