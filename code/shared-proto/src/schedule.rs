//! Schedules: time → an action. The spec is what `schedules.yaml` holds; the
//! row is what the daemon tracks about it.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One entry of `schedules.yaml`, as written. Exactly one of `cron` / `every`
/// and exactly one of `run` / `agent`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleSpec {
    /// Five-field cron expression, local time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// An interval (`30m`, `4h`), counted from the last firing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,
    /// A command, argv.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run: Vec<String>,
    /// An agent CLI, run headless with `prompt` (or `prompt_file`) and `args`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Relative to the file the schedule came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Required for global schedules; a workspace schedule defaults to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_: Option<String>,
    /// Title of the runs; defaults to the schedule's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Pipes instead of a pty.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_tty: bool,
    #[serde(default)]
    pub missed: Missed,
    #[serde(default)]
    pub overlap: Overlap,
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
}

pub const GLOBAL_SCOPE: &str = "global";

/// What a reload found.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ReloadReport {
    pub loaded: usize,
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    /// `name: what was wrong`, for entries that did not load.
    pub errors: Vec<String>,
}
