//! Domain vocabulary shared across the wire.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

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
    /// Something is waiting on a human.
    pub fn needs_attention(self) -> bool {
        matches!(self, Self::NeedsInput | Self::Blocked | Self::Failed)
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

/// What `rl mark` may say. `done` is "nothing more for me here": the run
/// goes `idle`, not to a terminal state — the process is still alive and its
/// exit will be recorded when it comes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    Done,
    Blocked,
    Working,
}

impl Mark {
    pub fn state(self) -> RunState {
        match self {
            Mark::Done => RunState::Idle,
            Mark::Blocked => RunState::Blocked,
            Mark::Working => RunState::Working,
        }
    }
}

/// How a viewer attaches: watching is read-only; joining owns the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachMode {
    Watch,
    Join,
}

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

str_enum!(RunKind { Agent => "agent", Shell => "shell", Command => "command", Plugin => "plugin" });
str_enum!(RunState {
    Starting => "starting", Working => "working", Idle => "idle", NeedsInput => "needs_input",
    Blocked => "blocked", Stale => "stale", Finished => "finished", Failed => "failed", Stopped => "stopped",
});
str_enum!(Source { Hook => "hook", Transcript => "transcript", Git => "git", Pty => "pty", Manual => "manual" });
str_enum!(Confidence { Guess => "guess", Likely => "likely", Exact => "exact" });
str_enum!(Mark { Done => "done", Blocked => "blocked", Working => "working" });
str_enum!(AttachMode { Watch => "watch", Join => "join" });

/// The one noun. Everything `rld` starts and owns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub workspace: PathBuf,
    pub kind: RunKind,
    pub title: String,
    /// Detected CLI (`claude`, `codex`, …), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    /// Opaque, optionally scheme-prefixed (`notion:FL-123`). Never interpreted.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub state: RunState,
    pub source: Source,
    pub confidence: Confidence,
    /// When `state` last changed.
    pub state_at: Timestamp,
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run: Option<String>,
    /// What an exact source last said it was doing (`tool: Bash`, a mark's
    /// note); refreshed by every such signal, not only by state changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Last meaningful output line, for fleet rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_line: Option<String>,
    /// When the user last looked at this run's news (`run.seen`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<Timestamp>,
}

impl Run {
    /// A fresh run in `starting`, everything optional unset.
    pub fn new(
        id: impl Into<String>,
        kind: RunKind,
        title: impl Into<String>,
        argv: Vec<String>,
        cwd: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
        now: Timestamp,
    ) -> Self {
        Self {
            id: id.into(),
            workspace: workspace.into(),
            kind,
            title: title.into(),
            cli: None,
            argv,
            cwd: cwd.into(),
            branch: None,
            worktree: None,
            ref_: None,
            tags: Vec::new(),
            state: RunState::Starting,
            source: Source::Manual,
            confidence: Confidence::Exact,
            state_at: now,
            started_at: now,
            ended_at: None,
            exit_code: None,
            cost: None,
            schedule_run: None,
            plugin: None,
            parent_run: None,
            detail: None,
            last_line: None,
            seen_at: None,
        }
    }

    /// Derived, never stored: `ref → branch → workspace`.
    pub fn group_key(&self) -> String {
        self.ref_
            .clone()
            .or_else(|| self.branch.clone())
            .unwrap_or_else(|| self.workspace.to_string_lossy().into_owned())
    }
}

/// An append-only fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub at: Timestamp,
    pub workspace: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    pub group_key: String,
    /// Dotted, open vocabulary: `run.started`, `plugin.<name>.<kind>`.
    pub kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Event kinds emitted by `rld` itself. Plugins add `plugin.<name>.*`.
pub mod event_kind {
    pub const RUN_STARTED: &str = "run.started";
    pub const RUN_STATE: &str = "run.state";
    pub const RUN_NEEDS_INPUT: &str = "run.needs_input";
    pub const RUN_BLOCKED: &str = "run.blocked";
    pub const RUN_RESUMED: &str = "run.resumed";
    pub const RUN_FINISHED: &str = "run.finished";
    pub const RUN_FAILED: &str = "run.failed";
    pub const RUN_STOPPED: &str = "run.stopped";
    pub const COMMIT_LANDED: &str = "commit.landed";
    pub const BRANCH_DONE: &str = "branch.done";
    pub const SCHEDULE_FIRED: &str = "schedule.fired";
    pub const PLUGIN_INVOKED: &str = "plugin.invoked";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_roundtrip_as_snake_case() {
        assert_eq!(serde_json::to_string(&RunState::NeedsInput).unwrap(), "\"needs_input\"");
        assert_eq!("needs_input".parse::<RunState>().unwrap(), RunState::NeedsInput);
        assert_eq!(RunState::NeedsInput.as_str(), "needs_input");
        assert!("nope".parse::<RunState>().is_err());
        assert!(Confidence::Guess < Confidence::Exact);
    }

    #[test]
    fn group_key_falls_back() {
        let mut r = Run::new("a", RunKind::Command, "t", vec![], "/w", "/w", Timestamp::UNIX_EPOCH);
        assert_eq!(r.group_key(), "/w");
        r.branch = Some("b".into());
        assert_eq!(r.group_key(), "b");
        r.ref_ = Some("notion:X".into());
        assert_eq!(r.group_key(), "notion:X");
    }
}
