//! Domain vocabulary shared across the wire.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

pub use rl_core::state::{Confidence, RunKind, RunState, Source};

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

pub(crate) use rl_core::str_enum;

str_enum!(Mark { Done => "done", Blocked => "blocked", Working => "working" });

/// Why a run started. Serialised as one string: `manual`,
/// `decision:<id>`, `flow:<flow>/<step>`, `assistant:<job>`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Cause {
    #[default]
    Manual,
    Decision(String),
    /// A step of a flow.
    Flow {
        flow: String,
        step: String,
    },
    /// A job Relay handed to an agent (`assistant:interpret`).
    Assistant(String),
}

impl std::fmt::Display for Cause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cause::Manual => write!(f, "manual"),
            Cause::Decision(id) => write!(f, "decision:{id}"),
            Cause::Flow { flow, step } => write!(f, "flow:{flow}/{step}"),
            Cause::Assistant(job) => write!(f, "assistant:{job}"),
        }
    }
}

impl std::str::FromStr for Cause {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        if s == "manual" {
            return Ok(Cause::Manual);
        }
        let (kind, rest) = s.split_once(':').ok_or_else(|| format!("bad cause `{s}`"))?;
        if rest.is_empty() {
            return Err(format!("bad cause `{s}`"));
        }
        Ok(match kind {
            "decision" => Cause::Decision(rest.into()),
            "assistant" => Cause::Assistant(rest.into()),
            "flow" => {
                let (flow, step) = rest.split_once('/').ok_or_else(|| format!("bad cause `{s}`"))?;
                if flow.is_empty() || step.is_empty() {
                    return Err(format!("bad cause `{s}`"));
                }
                Cause::Flow {
                    flow: flow.into(),
                    step: step.into(),
                }
            }
            _ => return Err(format!("bad cause `{s}`")),
        })
    }
}

impl Serialize for Cause {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Cause {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A folder registered with `rl up`: where a flow may run (`in:`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub path: PathBuf,
    pub name: String,
    pub registered_at: Timestamp,
}

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
    /// Why it started.
    #[serde(default)]
    pub cause: Cause,
    /// The chain it belongs to: its own id when it started one.
    #[serde(default)]
    pub chain: String,
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
    /// When the user was done with it (`run.archive`): out of the panel and
    /// `rl since`, kept in the ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<Timestamp>,
    /// When it was pinned (`run.pin`): kept at the top of the panel, however
    /// old, until unpinned or archived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<Timestamp>,
    /// When it was taken off the panel's list (`run.hide`): shown only in the
    /// `hidden` lens, unless it comes to need you after. Never stopped by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_at: Option<Timestamp>,
    /// The repo's HEAD when it began, if it ran in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_head: Option<String>,
}

/// The tag that marks a run as a project's warm shell.
pub const SHELL_TAG: &str = "shell";

impl Run {
    /// A shell: an interactive one by its profile, or a project's warm shell
    /// by its tag, whatever its argv resolved to.
    pub fn is_shell(&self) -> bool {
        self.kind == RunKind::Shell || self.tags.iter().any(|t| t == SHELL_TAG)
    }

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
        let id: String = id.into();
        Self {
            chain: id.clone(),
            id,
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
            cause: Cause::Manual,
            parent_run: None,
            detail: None,
            last_line: None,
            seen_at: None,
            archived_at: None,
            pinned_at: None,
            hidden_at: None,
            start_head: None,
        }
    }

    /// Derived, never stored: `ref → branch → workspace`.
    pub fn group_key(&self) -> String {
        self.ref_
            .clone()
            .or_else(|| self.branch.clone())
            .unwrap_or_else(|| self.workspace.to_string_lossy().into_owned())
    }

    /// Where the list files it: under its ref, else its folder. Never its
    /// branch — a folder is one project whichever branch it is on.
    pub fn place_key(&self) -> String {
        self.ref_
            .clone()
            .unwrap_or_else(|| self.workspace.to_string_lossy().into_owned())
    }
}

/// A folder's git checkout as it stands now, read from its `.git` — not the
/// branch a run started on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkout {
    pub path: PathBuf,
    /// The branch checked out; none on a detached HEAD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The commit, short, when HEAD is detached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detached: Option<String>,
    /// Linked worktrees, counted on the main checkout only.
    #[serde(default)]
    pub worktrees: usize,
    /// What the configured badges said of it (`PR #95`), in their order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<String>,
}

impl Checkout {
    /// What HEAD is, in a word: the branch, or the detached commit.
    pub fn head(&self) -> &str {
        self.branch.as_deref().or(self.detached.as_deref()).unwrap_or("")
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

/// What an assistant said about an ended run, kept by the run; `s` in the
/// panel and `rl summary` make one, the pane shows it under the facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub run: String,
    /// Who wrote it.
    pub agent: String,
    /// The assistant run that wrote it; empty when nothing had to be asked
    /// (the agent's own recap, read from its transcript).
    pub by: String,
    pub at: Timestamp,
    pub text: String,
    /// The stretch it covers: a duration back from when it was made (`2h`).
    /// None, the whole run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// Where it came from: `claude recap` (the agent's own), `codex turns`
    /// (its turns, as facts), or `summarised` (an assistant read the stretch).
    #[serde(default)]
    pub source: String,
    /// How far the run had got when it was made, so asking again with nothing
    /// new gives this one back.
    #[serde(default)]
    pub reach: String,
    /// Not kept: whether the run has moved on since it was made.
    #[serde(default)]
    pub stale: bool,
    /// What was summarised (`SummaryTarget::key`); empty on a run's own recap
    /// row.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
    /// Read from the logs, not from the recaps.
    #[serde(default)]
    pub deep: bool,
    /// Where the stretch began; None for a whole run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Timestamp>,
    /// How many assistant calls it took.
    #[serde(default)]
    pub calls: u32,
    /// How many runs it covers: one for a run; for a flow, a project or
    /// everything, those in the stretch.
    #[serde(default)]
    pub runs: u32,
}

/// What a summary is of: a run, a flow's runs, a project's, or everything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SummaryTarget {
    Run {
        id: String,
    },
    /// A flow's runs: those of its steps, wherever they ran.
    Flow {
        name: String,
    },
    /// One firing of a flow: its steps' runs, all of it.
    Firing {
        id: String,
    },
    Project {
        workspace: PathBuf,
    },
    All,
}

impl SummaryTarget {
    /// How it is kept: `r:<id>`, `f:<name>`, `fi:<id>`, `p:<path>`, `all`.
    pub fn key(&self) -> String {
        match self {
            SummaryTarget::Run { id } => format!("r:{id}"),
            SummaryTarget::Flow { name } => format!("f:{name}"),
            SummaryTarget::Firing { id } => format!("fi:{id}"),
            SummaryTarget::Project { workspace } => format!("p:{}", workspace.display()),
            SummaryTarget::All => "all".into(),
        }
    }
}

/// What a run raised itself (`rl emit`): one kind, how often it came, and
/// what it last said in its own words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    pub kind: String,
    pub count: u32,
    /// The newest one's summary — what the probe said, not what the wire is
    /// called.
    pub said: String,
    /// When the newest one came.
    pub at: Timestamp,
}

/// Event kinds emitted by `rld` itself. Plugins add `plugin.<name>.*`.
pub mod event_kind {
    /// The families that are the daemon's own: a command may not `rl emit`
    /// one, and what a run raised is what falls outside them.
    pub const OWN: &[&str] = &["run.", "decision.", "flow.", "commit.", "branch."];

    /// Whether a kind belongs to the daemon rather than to what a run raised.
    pub fn is_own(kind: &str) -> bool {
        OWN.iter().any(|p| kind.starts_with(p))
    }

    /// A command a shell run finished; `data.exit` is its status when known.
    pub const SHELL_COMMAND: &str = "shell.command";
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
    /// A flow fired: by hand, on time, or on an event.
    pub const FLOW_FIRED: &str = "flow.fired";
    /// A firing is over: done, failed or stopped.
    pub const FLOW_ENDED: &str = "flow.ended";
    /// A flow or one of its steps did not run, and why.
    pub const FLOW_SKIPPED: &str = "flow.skipped";
    /// What a step left that Relay would not keep (a `$RELAY_OUT` line), and why: the step failed.
    pub const FLOW_REFUSED: &str = "flow.refused";
    pub const DECISION_RAISED: &str = "decision.raised";
    pub const DECISION_TAKEN: &str = "decision.taken";
    pub const DECISION_DISMISSED: &str = "decision.dismissed";
    pub const DECISION_EXPIRED: &str = "decision.expired";
    pub const PLUGIN_INVOKED: &str = "plugin.invoked";
    /// Something went wrong around a run without ending it: output lost
    /// while no daemon was there, a holder connection that failed, a holder
    /// gone without an exit. A warning that would otherwise only flash.
    pub const RUN_TROUBLE: &str = "run.trouble";
    /// Relay came back and the run's holder was gone with no exit on record —
    /// the machine restarted, most likely. `run.resume` can pick it back up.
    pub const RUN_LOST: &str = "run.lost";
    /// Retention took a run's journal; the run and its events stay.
    pub const RUN_JOURNAL_DROPPED: &str = "run.journal_dropped";
    /// An assistant wrote a run's recap (`summaries`); the agent's own is a
    /// seed and is not said.
    pub const RUN_SUMMARISED: &str = "run.summarised";

    /// Every kind above, for anything that lists them.
    pub const ALL: &[&str] = &[
        RUN_STARTED,
        RUN_STATE,
        RUN_NEEDS_INPUT,
        RUN_BLOCKED,
        RUN_RESUMED,
        RUN_FINISHED,
        RUN_FAILED,
        RUN_STOPPED,
        RUN_TROUBLE,
        RUN_LOST,
        COMMIT_LANDED,
        BRANCH_DONE,
        FLOW_FIRED,
        FLOW_ENDED,
        FLOW_SKIPPED,
        FLOW_REFUSED,
        DECISION_RAISED,
        DECISION_TAKEN,
        DECISION_DISMISSED,
        DECISION_EXPIRED,
        PLUGIN_INVOKED,
        RUN_JOURNAL_DROPPED,
        RUN_SUMMARISED,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flow_step_is_its_own_cause() {
        let c: Cause = "flow:prod-logs/triage".parse().unwrap();
        assert_eq!(
            c,
            Cause::Flow {
                flow: "prod-logs".into(),
                step: "triage".into()
            }
        );
        assert_eq!(c.to_string(), "flow:prod-logs/triage");
        assert!("flow:prod-logs".parse::<Cause>().is_err());
        assert!("flow:/triage".parse::<Cause>().is_err());
        assert!("flow:prod-logs/".parse::<Cause>().is_err());
        assert!("workflow:a/b".parse::<Cause>().is_err());
    }

    #[test]
    fn the_old_causes_are_refused() {
        assert!("schedule:x".parse::<Cause>().is_err());
        assert!("reaction:x".parse::<Cause>().is_err());
        assert!(serde_json::from_str::<Cause>("\"schedule:nightly\"").is_err());
    }

    #[test]
    fn a_target_is_kept_by_a_key_that_says_what_it_is() {
        assert_eq!(SummaryTarget::Run { id: "ab12".into() }.key(), "r:ab12");
        let flow = SummaryTarget::Flow { name: "nightly".into() };
        assert_eq!(flow.key(), "f:nightly");
        assert_eq!(
            SummaryTarget::Project {
                workspace: PathBuf::from("/w/api")
            }
            .key(),
            "p:/w/api"
        );
        assert_eq!(SummaryTarget::All.key(), "all");
    }

    #[test]
    fn a_firing_is_a_summarys_target_of_its_own() {
        let t = SummaryTarget::Firing { id: "fi1".into() };
        assert_eq!(t.key(), "fi:fi1");
        assert_eq!(
            serde_json::to_value(&t).unwrap(),
            serde_json::json!({"kind": "firing", "id": "fi1"})
        );
    }

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
        // The list keeps it with its folder, whatever the branch.
        assert_eq!(r.place_key(), "/w");
        r.ref_ = Some("notion:X".into());
        assert_eq!(r.group_key(), "notion:X");
        assert_eq!(r.place_key(), "notion:X");
    }
}
