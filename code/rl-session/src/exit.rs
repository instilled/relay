//! How a child ended, and the sidecar file that records it so `rl` learns
//! the outcome even if it was not running at the time.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exit {
    /// Exit code; `128 + signal` when killed by a signal.
    pub code: i32,
    /// Signal name (`SIGTERM`) when killed by one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    pub at: Timestamp,
}

impl Exit {
    pub fn success(&self) -> bool {
        self.code == 0 && self.signal.is_none()
    }
}

/// Written atomically (tmp + rename) so a reader never sees a partial file.
pub fn write_exit_file(path: &Path, exit: &Exit) -> io::Result<()> {
    rl_core::atomic::write_json(path, exit)
}

/// `Ok(None)` when the file does not exist yet.
pub fn read_exit_file(path: &Path) -> io::Result<Option<Exit>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Name for a Unix signal number, matching what `portable-pty` reports.
pub fn signal_name(n: i32) -> String {
    match n {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        6 => "SIGABRT",
        9 => "SIGKILL",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => return format!("SIG{n}"),
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_file_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("k7x2m9qa.exit");
        assert_eq!(read_exit_file(&p).unwrap(), None);
        let e = Exit {
            code: 143,
            signal: Some("SIGTERM".into()),
            at: Timestamp::UNIX_EPOCH,
        };
        write_exit_file(&p, &e).unwrap();
        assert_eq!(read_exit_file(&p).unwrap(), Some(e));
        assert!(
            std::fs::read_dir(tmp.path())
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
        );
    }
}
