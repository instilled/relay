//! Domain vocabulary shared across the wire. Kept deliberately small; fields
//! on a run are added as the daemon learns to fill them.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Agent,
    Shell,
    Command,
    Plugin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Starting,
    Working,
    Idle,
    NeedsInput,
    Blocked,
    Stale,
    Finished,
    Failed,
    Stopped,
}

impl RunState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Finished | Self::Failed | Self::Stopped)
    }
}

/// Where a state transition came from. Principle 3: signals over scraping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Hook,
    Transcript,
    Git,
    Pty,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Guess,
    Likely,
    Exact,
}
