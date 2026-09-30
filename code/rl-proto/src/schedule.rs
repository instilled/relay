//! Schedules: time → an action. The spec is what `schedules.yaml` holds; the
//! row is what the daemon tracks about it.

use crate::action::action_struct;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

action_struct! {
    /// One entry of `schedules.yaml`, as written: when it fires, what it runs,
    /// and what to do about firings that clash or are missed. `cwd` is
    /// required for global schedules; a workspace schedule defaults to the
    /// folder, and a relative one is inside it.
    #[serde(deny_unknown_fields)]
    struct ScheduleSpec {
        before: {
            /// Five-field cron expression, local time. `cron` or `every`, not both.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub cron: Option<String>,
            /// An interval (`30m`, `4h`), counted from the last firing.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub every: Option<String>,
            /// Event kinds what it runs raises (`rl emit`), so the graph can
            /// wire it to the reactions that hear them before it ever has.
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            pub emits: Vec<String>,
        }
        after: {
            #[serde(default)]
            pub missed: Missed,
            #[serde(default)]
            pub overlap: Overlap,
        }
    }
}

impl ScheduleSpec {
    /// `cron "0 2 * * *"` or `every 4h`, for lists.
    pub fn when(&self) -> String {
        match (&self.cron, &self.every) {
            (Some(c), _) => format!("cron {c}"),
            (None, Some(e)) => format!("every {e}"),
            (None, None) => "never".into(),
        }
    }
}

/// What to do about a firing that was missed while asleep or down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Missed {
    #[default]
    Skip,
    CatchUpOnce,
}

/// What to do when the previous run is still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Overlap {
    #[default]
    Skip,
    Queue,
    Kill,
}

/// A schedule as the daemon tracks it: the spec, its version, and what has
/// happened to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    /// `global`, or a workspace path.
    pub scope: String,
    pub name: String,
    /// sha256 of the spec; the version.
    pub hash: String,
    pub source: PathBuf,
    pub spec: ScheduleSpec,
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at: Option<Timestamp>,
    /// Gone from its file; kept so old trails still resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<Timestamp>,
    /// The version the user said yes to. The user's own file approves as
    /// it loads; a workspace's waits for `rl up`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_hash: Option<String>,
}

impl Schedule {
    /// The current version may fire.
    pub fn approved(&self) -> bool {
        self.approved_hash.as_deref() == Some(self.hash.as_str())
    }

    /// `global`, or the workspace folder's name.
    pub fn scope_label(&self) -> String {
        scope_label(&self.scope)
    }
}

/// `global`, or the folder's own name: what a scope is called in a list.
pub fn scope_label(scope: &str) -> String {
    if scope == GLOBAL_SCOPE {
        return GLOBAL_SCOPE.to_string();
    }
    std::path::Path::new(scope)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| scope.to_string())
}

pub const GLOBAL_SCOPE: &str = "global";
/// A workspace keeps its Relay files here.
pub const WORKSPACE_DIR: &str = ".relay";

/// A folder registered with `rl up`: the trust boundary for its `.relay/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub path: PathBuf,
    pub name: String,
    pub registered_at: Timestamp,
}

/// What `rl up` found and did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpReport {
    pub workspace: Workspace,
    /// What reading the folder's `schedules.yaml` changed.
    pub report: ReloadReport,
    /// Its schedules as they now stand.
    pub schedules: Vec<Schedule>,
    /// Schedules approved by this call.
    pub approved: Vec<String>,
    /// What reading its `reactions.yaml` changed.
    #[serde(default)]
    pub reaction_report: ReloadReport,
    /// Its reactions as they now stand.
    #[serde(default)]
    pub reactions: Vec<crate::reaction::Reaction>,
    /// Reactions approved by this call.
    #[serde(default)]
    pub approved_reactions: Vec<String>,
}

/// What a reload found.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ReloadReport {
    pub loaded: usize,
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    /// What did not load, a line each, as `not_loaded` says it.
    pub errors: Vec<String>,
    /// What did not load, and where: a file that does not parse (its rules
    /// stay as they last loaded) or one rule in it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_loaded: Vec<crate::rule::NotLoaded>,
}
