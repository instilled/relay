//! `rl flow new`: a new flow's folder written, then handed to the edit path for its yes.

use std::path::PathBuf;

/// A file a new flow starts with: its path in the folder, its bytes, and whether a step runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub exec: bool,
}

impl NewFile {
    /// The flow's `flow.md`.
    pub fn flow_md(text: impl Into<String>) -> Self {
        NewFile {
            path: PathBuf::from("flow.md"),
            bytes: text.into().into_bytes(),
            exec: false,
        }
    }
}
