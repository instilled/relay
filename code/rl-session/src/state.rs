//! What the holder believes about its run's state, and the sidecar file that
//! records it (`state.json`) so a reader learns it without asking the holder.

use jiff::Timestamp;
use rl_core::state::{Confidence, RunState};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateFile {
    pub state: RunState,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_line: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<bool>,
    pub at: Timestamp,
}

/// Written atomically (tmp + rename) so a reader never sees a partial file.
pub fn write_state_file(path: &Path, s: &StateFile) -> io::Result<()> {
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&tmp, serde_json::to_vec(s)?)?;
    std::fs::rename(&tmp, path)
}

/// `Ok(None)` when the file does not exist yet.
pub fn read_state_file(path: &Path) -> io::Result<Option<StateFile>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_file_round_trips_and_a_missing_one_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("state.json");
        assert_eq!(read_state_file(&p).unwrap(), None);
        let s = StateFile {
            state: RunState::NeedsInput,
            confidence: Confidence::Likely,
            detail: Some("Continue? [y/N]".into()),
            last_line: None,
            foreground: Some(false),
            at: Timestamp::UNIX_EPOCH,
        };
        write_state_file(&p, &s).unwrap();
        assert_eq!(read_state_file(&p).unwrap(), Some(s));
        assert!(!tmp.path().join("state.json.tmp").exists());
    }
}
