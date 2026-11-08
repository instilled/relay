//! The run-state vocabulary: what a run is, what state it is in, and the
//! evidence that says so. Lives here so the session holder can use it too.

use serde::{Deserialize, Serialize};

/// `as_str`, `FromStr` and `Display` for a unit enum, over its wire names.
#[macro_export]
macro_rules! str_enum {
    ($t:ty { $($v:ident => $s:literal),+ $(,)? }) => {
        impl $t {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$v => $s),+ }
            }
        }
        impl std::str::FromStr for $t {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, String> {
                match s { $($s => Ok(Self::$v),)+ other => Err(format!("unknown {}: {other}", stringify!($t))) }
            }
        }
        impl std::fmt::Display for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.as_str()) }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Agent,
    Shell,
    Command,
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
    /// Every variant, for anything that needs to enumerate them.
    pub const ALL: &'static [Self] = &[
        Self::Starting,
        Self::Working,
        Self::Idle,
        Self::NeedsInput,
        Self::Blocked,
        Self::Stale,
        Self::Finished,
        Self::Failed,
        Self::Stopped,
    ];

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Finished | Self::Failed | Self::Stopped)
    }
}

/// Where a state transition came from. Principle 3: signals over scraping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Pty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Guess,
    Likely,
    Exact,
}

/// Evidence about a run, from one source, at one confidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    pub state: RunState,
    pub source: Source,
    pub confidence: Confidence,
    /// What was seen: the question text, the tool name, the prompt line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Signal {
    pub fn pty(state: RunState, confidence: Confidence, detail: Option<String>) -> Self {
        Self {
            state,
            source: Source::Pty,
            confidence,
            detail,
        }
    }
}

str_enum!(RunKind { Agent => "agent", Shell => "shell", Command => "command" });
str_enum!(RunState {
    Starting => "starting", Working => "working", Idle => "idle", NeedsInput => "needs_input",
    Blocked => "blocked", Stale => "stale", Finished => "finished", Failed => "failed", Stopped => "stopped",
});
str_enum!(Source { Pty => "pty" });
str_enum!(Confidence { Guess => "guess", Likely => "likely", Exact => "exact" });

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_keep_their_wire_names() {
        assert_eq!(serde_json::to_string(&RunState::NeedsInput).unwrap(), "\"needs_input\"");
        assert_eq!(serde_json::to_string(&RunKind::Shell).unwrap(), "\"shell\"");
        let s = Signal::pty(RunState::Idle, Confidence::Likely, Some("$ ".into()));
        assert_eq!(s.source, Source::Pty);
        assert!(RunState::Stopped.is_terminal() && !RunState::NeedsInput.is_terminal());
    }
}
