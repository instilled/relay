//! A flow file's shape: `flow.md`'s frontmatter in a flow's folder; and a
//! flow, its firings and what did not load, as they cross the wire.
//!
//! ```yaml
//! when: every 30m          # or `on: run.failed`, or neither (by hand only)
//! steps:
//!   probe:
//!     run: new-log-errors.sh
//!   triage:
//!     after: probe
//!     if: probe.out
//!     run: { agent: claude }
//!   review:
//!     after: triage
//!     ask: "{triage.out}"
//!     options:
//!       apply: { run: { agent: claude } }
//!       ignore: {}
//!     default: ignore
//!     expires: 4h
//! ```
//!
//! Steps and options are maps whose order matters, so they are read into
//! lists of pairs in the order they are written ([`ordered`]).

mod outcome;
pub use outcome::{FiringRef, FlowOutcome, Outcome};

use crate::action::{Action, action_struct};
use crate::methods::Method;
use crate::types::str_enum;
use jiff::Timestamp;
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::path::PathBuf;

/// How long a flow waits before an event may fire it again, unless it says
/// otherwise.
pub const DEFAULT_THROTTLE: &str = "10s";

/// What else has to be true of the run an event is about. Every field given
/// must match; a flow with none takes every event of its kinds.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    /// The run carries this tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Its cause starts with this: `flow:`, or `flow:nightly`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// The CLI it is running: `claude`, `codex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    /// The event's group key, exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Fields of the event's `data` that must match — a value, or any of a
    /// list: `where: { data: { severity: [ERROR, WARNING] } }`. Compared as
    /// text, so quote numbers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, Match>,
}

/// A `where.data` value: one string, or any of a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Match {
    One(String),
    Any(Vec<String>),
}

impl Match {
    /// Whether an event's field (as text) satisfies this.
    pub fn matches(&self, value: &str) -> bool {
        match self {
            Match::One(s) => s == value,
            Match::Any(xs) => xs.iter().any(|x| x == value),
        }
    }
}

/// What to do about a time that passed while asleep or down. By default it is made up, once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Missed {
    Skip,
    #[default]
    CatchUpOnce,
}

/// What to do when the previous firing is still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Overlap {
    #[default]
    Skip,
    Queue,
    Kill,
}

/// Where a flow's firing goes in the catch-up besides its project's line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CatchUpPlace {
    /// Its latest firing since you left heads the catch-up, above Relay's own bullets.
    Top,
}

/// What a flow file says. How a flow starts is `when` (a time) or `on` (an
/// event kind); with neither it runs only when asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowSpec {
    /// The workspace the steps run in, a path; `~` allowed.
    #[serde(default, rename = "in", skip_serializing_if = "Option::is_none")]
    pub in_: Option<PathBuf>,
    /// A time to fire at: `every 30m`, or a cron line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Event kinds to fire on, one or a list.
    #[serde(
        default,
        deserialize_with = "crate::flow::kinds",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub on: Vec<String>,
    /// What else has to be true of the run an event is about.
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter>,
    /// What to do about a time that passed while asleep or down.
    #[serde(default)]
    pub missed: Missed,
    /// What to do when the last firing is still going.
    #[serde(default)]
    pub overlap: Overlap,
    /// The least time between two firings: `10m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throttle: Option<String>,
    /// `top`: its latest firing since you left heads the catch-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catch_up: Option<CatchUpPlace>,
    /// The steps, in the order they are written.
    #[serde(with = "crate::flow::ordered")]
    pub steps: Vec<(String, StepSpec)>,
}

action_struct! {
    #[serde(deny_unknown_fields)]
    struct StepSpec {
        before: {
            /// The steps that must finish first.
            #[serde(default, deserialize_with = "crate::flow::one_or_many", skip_serializing_if = "Vec::is_empty")]
            pub after: Vec<String>,
            /// Runs only if this holds of an earlier step: `probe.out`.
            #[serde(default, rename = "if", skip_serializing_if = "Option::is_none")]
            pub if_: Option<String>,
            /// Where it runs, instead of the flow's workspace.
            #[serde(default, rename = "in", skip_serializing_if = "Option::is_none")]
            pub in_: Option<PathBuf>,
            /// A question for the person, in place of (or before) a run.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub ask: Option<String>,
            /// The answers, each with what it starts, in the order written.
            #[serde(default, with = "crate::flow::ordered", skip_serializing_if = "Vec::is_empty")]
            pub options: Vec<(String, Action)>,
            /// The answer taken when it expires.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub default: Option<String>,
            /// How long the question waits: `4h`.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub expires: Option<String>,
            /// Questions with the same group are asked as one.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub group: Option<String>,
            /// More than one answer may be chosen.
            #[serde(default, skip_serializing_if = "std::ops::Not::not")]
            pub multi: bool,
        }
        after: {}
    }
}

/// A map read into pairs, in the order it is written. A key given twice is
/// an error, which a plain map would swallow.
pub mod ordered {
    use super::*;

    pub fn serialize<S: Serializer, T: Serialize>(pairs: &[(String, T)], s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(pairs.len()))?;
        for (k, v) in pairs {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<(String, T)>, D::Error> {
        struct Pairs<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for Pairs<T> {
            type Value = Vec<(String, T)>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a map")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut pairs: Vec<(String, T)> = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if pairs.iter().any(|(k, _)| *k == key) {
                        return Err(de::Error::custom(format!("step `{key}` is named twice")));
                    }
                    pairs.push((key, map.next_value()?));
                }
                Ok(pairs)
            }
        }

        d.deserialize_map(Pairs(PhantomData))
    }
}

/// `after: probe` and `after: [probe, triage]` both mean a list.
pub fn one_or_many<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

/// `on: run.failed` and `on: [run.failed, run.blocked]` both mean a list,
/// which is never empty and never holds a blank.
pub fn kinds<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let kinds = one_or_many(d)?;
    if kinds.is_empty() || kinds.iter().any(|k| k.trim().is_empty()) {
        return Err(de::Error::custom("`on` needs an event kind, or a list of them"));
    }
    Ok(kinds)
}

/// Where a step of a firing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// Not reached yet.
    Pending,
    /// Its run is going: the step's own, or the option chosen at its question.
    Running,
    /// Its question is open.
    Waiting,
    /// Its run was lost while `rld` was down; resuming picks it up.
    Lost,
    Done,
    /// Not run: its `if:` was false, its question went unanswered, or a step before it did not finish well.
    Skipped,
    Failed,
    /// Its run was stopped by hand, which stops the firing.
    Stopped,
}

/// Where a firing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FiringStatus {
    Running,
    /// On a question, or a lost step.
    Waiting,
    Done,
    Failed,
    /// Stopped by hand.
    Stopped,
}

str_enum!(StepStatus {
    Pending => "pending", Running => "running", Waiting => "waiting", Lost => "lost",
    Done => "done", Skipped => "skipped", Failed => "failed", Stopped => "stopped",
});
str_enum!(FiringStatus {
    Running => "running", Waiting => "waiting", Done => "done", Failed => "failed", Stopped => "stopped",
});

/// A flow as `rld` has it: the folder as last loaded, and what became of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flow {
    pub name: String,
    /// The folder's, less `runs/`: what a yes is given to.
    pub hash: String,
    /// `rl_flow::def::FlowDef` as JSON; the client reads what it shows.
    pub def: serde_json::Value,
    /// The folder.
    pub source: PathBuf,
    /// Its `in`, resolved: `~` expanded and the path made canonical, as the
    /// projects are registered — where its row stands on the panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    pub paused: bool,
    pub approved_hash: Option<String>,
    pub last_fired_at: Option<Timestamp>,
    pub next_at: Option<Timestamp>,
    pub removed_at: Option<Timestamp>,
}

/// Where a flow stands, in one word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlowStatus {
    /// A version of the folder nobody has said yes to.
    NeedsApproval,
    Paused,
    On,
    Removed,
}

impl fmt::Display for FlowStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FlowStatus::NeedsApproval => "needs approval",
            FlowStatus::Paused => "paused",
            FlowStatus::On => "on",
            FlowStatus::Removed => "removed",
        })
    }
}

impl Flow {
    /// Whether the folder as it is now has a yes.
    pub fn approved(&self) -> bool {
        self.approved_hash.as_deref() == Some(self.hash.as_str())
    }

    /// Whether its latest firing since you left heads the catch-up (`catch_up: top`).
    pub fn heads_catch_up(&self) -> bool {
        self.def["catch_up"] == "top"
    }

    pub fn status(&self) -> FlowStatus {
        if self.removed_at.is_some() {
            FlowStatus::Removed
        } else if !self.approved() {
            FlowStatus::NeedsApproval
        } else if self.paused {
            FlowStatus::Paused
        } else {
            FlowStatus::On
        }
    }

    /// What starts it: `every 30m`, `30 7 * * 1-5`, `on run.failed`, `by hand`.
    pub fn when(&self) -> String {
        let trigger = &self.def["trigger"];
        let mut said = Vec::new();
        if let Some(secs) = trigger["when"]["Every"].as_u64() {
            said.push(format!("every {}", every(secs)));
        }
        if let Some(cron) = trigger["when"]["Cron"].as_str() {
            said.push(cron.to_string());
        }
        let on: Vec<&str> = trigger["on"]
            .as_array()
            .map(|kinds| kinds.iter().filter_map(|k| k.as_str()).collect())
            .unwrap_or_default();
        if !on.is_empty() {
            said.push(format!("on {}", on.join(", ")));
        }
        if said.is_empty() {
            return "by hand".to_string();
        }
        said.join(" or ")
    }
}

/// `30m`, `1h`, `1h30m`, `90s` as written in a `when:`, from seconds.
fn every(secs: u64) -> String {
    let parts = [
        (secs / 86_400, "d"),
        (secs / 3600 % 24, "h"),
        (secs / 60 % 60, "m"),
        (secs % 60, "s"),
    ];
    parts
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, unit)| format!("{n}{unit}"))
        .collect()
}

/// One run of a flow, and where each of its steps is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Firing {
    /// Also its step runs' chain.
    pub id: String,
    pub flow: String,
    /// `when`, `on` or `hand`.
    pub trigger: String,
    /// The event that fired it, for `on`.
    pub event: Option<i64>,
    pub status: FiringStatus,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    /// In the order the flow writes them.
    pub steps: Vec<StepState>,
    /// The first line of what it came to (`Outcome`), as `rld` sends it; not kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// One step of a firing: where it is, the run it started, the question it raised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepState {
    pub name: String,
    pub status: StepStatus,
    pub run: Option<String>,
    pub decision: Option<String>,
}

/// A flow folder that did not load at the last reload: where, and why. It is
/// left out while the rest load, and stays said until it is fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotLoaded {
    pub flow: String,
    /// `<folder>/flow.md`, or the folder when it could not be read.
    pub file: PathBuf,
    /// From 1, when it is known.
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub message: String,
}

impl NotLoaded {
    /// `flow.md:3:5`, `flow.md:3`, or just the file's name.
    pub fn at(&self) -> String {
        let file = self
            .file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.file.to_string_lossy().into_owned());
        match (self.line, self.column) {
            (Some(l), Some(c)) => format!("{file}:{l}:{c}"),
            (Some(l), None) => format!("{file}:{l}"),
            _ => file,
        }
    }
}

/// `prod-logs · flow.md:3 · in: ~/x is not a project Relay knows`.
impl fmt::Display for NotLoaded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} · {} · {}", self.flow, self.at(), self.message)
    }
}

/// What a reload of `~/.relay/flows` found and changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reloaded {
    /// How many flows loaded.
    pub loaded: usize,
    pub added: Vec<String>,
    /// Changed folders: each waits for a yes again.
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub not_loaded: Vec<NotLoaded>,
}

// ---- flow.* methods ------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlowFilter {
    /// Removed flows too.
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowName {
    pub name: String,
}

/// A yes to one version of a flow's folder: the hash of the version the one saying it was shown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowApproval {
    pub name: String,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FiringFilter {
    pub flow: String,
    /// Newest first; default 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Every flow in `~/.relay/flows` as last loaded, by name.
pub struct FlowList;
impl Method for FlowList {
    const NAME: &'static str = "flow.list";
    type Params = FlowFilter;
    type Result = Vec<Flow>;
}

/// The flows that did not load at the last reload, each with its file and line.
pub struct FlowErrors;
impl Method for FlowErrors {
    const NAME: &'static str = "flow.errors";
    type Params = ();
    type Result = Vec<NotLoaded>;
}

/// Reads `~/.relay/flows` again.
pub struct FlowReload;
impl Method for FlowReload {
    const NAME: &'static str = "flow.reload";
    type Params = ();
    type Result = Reloaded;
}

/// Says yes to the folder as it was shown; the flow may fire from then on. Refused when the
/// folder as last loaded is not that version any more.
pub struct FlowApprove;
impl Method for FlowApprove {
    const NAME: &'static str = "flow.approve";
    type Params = FlowApproval;
    type Result = Flow;
}

pub struct FlowPause;
impl Method for FlowPause {
    const NAME: &'static str = "flow.pause";
    type Params = FlowName;
    type Result = Flow;
}

pub struct FlowResume;
impl Method for FlowResume {
    const NAME: &'static str = "flow.resume";
    type Params = FlowName;
    type Result = Flow;
}

/// Fires a flow now, by hand. Refused unless it has a yes.
pub struct FlowRun;
impl Method for FlowRun {
    const NAME: &'static str = "flow.run";
    type Params = FlowName;
    type Result = Firing;
}

/// A flow's last firings, newest first.
pub struct FlowFirings;
impl Method for FlowFirings {
    const NAME: &'static str = "flow.firings";
    type Params = FiringFilter;
    type Result = Vec<Firing>;
}

/// What a step asks of its firing's state: by the run it is called in (`$RELAY_RUN`), and a name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateName {
    pub run: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateValues {
    pub run: String,
    /// `(key, value)`; a key given twice is its last.
    pub values: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateFile {
    pub run: String,
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Sets values of the step asking, for the steps after it. Refused outside a running step.
pub struct FlowStateSet;
impl Method for FlowStateSet {
    const NAME: &'static str = "flow.state.set";
    type Params = StateValues;
    type Result = ();
}

/// A value, an out, a choice or a note a step of the firing left: `probe.signature`; a bare key
/// is the asking step's own. `None`: not set.
pub struct FlowStateGet;
impl Method for FlowStateGet {
    const NAME: &'static str = "flow.state.get";
    type Params = StateName;
    type Result = Option<String>;
}

/// Writes a file in the firing's folder, whole or not at all.
pub struct FlowStateWrite;
impl Method for FlowStateWrite {
    const NAME: &'static str = "flow.state.write";
    type Params = StateFile;
    type Result = ();
}

/// A file in the firing's folder, or what a step printed (`probe.out`).
pub struct FlowStateRead;
impl Method for FlowStateRead {
    const NAME: &'static str = "flow.state.read";
    type Params = StateName;
    type Result = Vec<u8>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_data_reads_a_value_or_a_list() {
        let f: Filter =
            serde_json::from_str(r#"{"data": {"severity": ["ERROR", "WARNING"], "service": "api"}}"#).unwrap();
        assert!(f.data["severity"].matches("ERROR"));
        assert!(f.data["severity"].matches("WARNING"));
        assert!(!f.data["severity"].matches("INFO"));
        assert!(f.data["service"].matches("api"));
        assert!(!f.data["service"].matches("web"));
    }

    #[test]
    fn steps_keep_the_order_they_are_written_in() {
        // Fed as text: `json!` would sort the keys before they were read.
        let s: FlowSpec = serde_json::from_str(
            r#"{
                "when": "every 30m",
                "steps": {
                    "probe": { "run": "new-log-errors.sh" },
                    "triage": { "after": "probe", "if": "probe.out", "run": { "agent": "claude" } },
                    "review": { "after": ["triage"], "ask": "{triage.out}",
                                "options": { "apply": { "run": { "agent": "claude" } }, "ignore": {} },
                                "default": "ignore", "expires": "4h" }
                }
            }"#,
        )
        .unwrap();
        let names: Vec<_> = s.steps.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["probe", "triage", "review"]);
        let review = &s.steps[2].1;
        assert_eq!(review.after, ["triage"]);
        assert_eq!(
            review.options.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            ["apply", "ignore"]
        );
        assert!(review.options[1].1.is_empty());
    }

    #[test]
    fn an_unknown_key_or_a_step_named_twice_is_refused() {
        let e = serde_json::from_value::<FlowSpec>(serde_json::json!({ "steps": {}, "cron": "x" }));
        assert!(e.unwrap_err().to_string().contains("cron"));
        // json! cannot repeat a key, so the text is fed as it is.
        let e = serde_json::from_str::<FlowSpec>(r#"{"steps":{"a":{"run":"x"},"a":{"run":"y"}}}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("named twice"), "{e}");
    }

    #[test]
    fn a_flow_writes_back_as_it_was_written() {
        let text = r#"{
            "in": "~/work/api",
            "on": ["run.failed", "run.blocked"],
            "where": { "tag": "nightly" },
            "throttle": "10m",
            "steps": {
                "b": { "run": ["echo", "b"] },
                "a": { "after": ["b"], "run": { "agent": "claude" } }
            }
        }"#;
        let s: FlowSpec = serde_json::from_str(text).unwrap();
        assert_eq!(s.in_, Some(PathBuf::from("~/work/api")));
        assert_eq!(s.on, ["run.failed", "run.blocked"]);
        let out = serde_json::to_string(&s).unwrap();
        assert!(out.find("\"b\":{").unwrap() < out.find("\"a\":{").unwrap(), "{out}");
        assert_eq!(serde_json::from_str::<FlowSpec>(&out).unwrap(), s);
    }

    #[test]
    fn on_takes_one_kind_or_many_but_never_none() {
        let on = |v: serde_json::Value| serde_json::from_value::<FlowSpec>(serde_json::json!({ "on": v, "steps": {} }));
        assert_eq!(on(serde_json::json!("run.failed")).unwrap().on, ["run.failed"]);
        assert_eq!(on(serde_json::json!(["a", "b"])).unwrap().on, ["a", "b"]);
        assert!(on(serde_json::json!([])).is_err());
        assert!(on(serde_json::json!(" ")).is_err());
    }

    #[test]
    fn statuses_read_back_as_they_are_written() {
        use StepStatus as S;
        for s in [
            S::Pending,
            S::Running,
            S::Waiting,
            S::Lost,
            S::Done,
            S::Skipped,
            S::Failed,
            S::Stopped,
        ] {
            assert_eq!(s.as_str().parse::<StepStatus>(), Ok(s));
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
        }
        use FiringStatus as F;
        for f in [F::Running, F::Waiting, F::Done, F::Failed, F::Stopped] {
            assert_eq!(f.as_str().parse::<FiringStatus>(), Ok(f));
            assert_eq!(serde_json::to_value(f).unwrap(), f.as_str());
        }
        assert!("ended".parse::<FiringStatus>().is_err());
    }

    fn flow(trigger: serde_json::Value) -> Flow {
        Flow {
            name: "pulse".into(),
            hash: "h2".into(),
            def: serde_json::json!({ "trigger": trigger }),
            source: "/h/flows/pulse".into(),
            folder: None,
            paused: false,
            approved_hash: Some("h1".into()),
            last_fired_at: None,
            next_at: None,
            removed_at: None,
        }
    }

    #[test]
    fn a_flow_is_on_once_its_folder_as_it_is_has_a_yes() {
        let mut f = flow(serde_json::json!({ "when": null, "on": [] }));
        assert_eq!(
            f.status(),
            FlowStatus::NeedsApproval,
            "the folder changed since the yes"
        );
        f.approved_hash = Some("h2".into());
        assert_eq!(f.status(), FlowStatus::On);
        f.paused = true;
        assert_eq!(f.status(), FlowStatus::Paused);
        f.removed_at = Some(Timestamp::UNIX_EPOCH);
        assert_eq!(f.status(), FlowStatus::Removed);
        assert_eq!(FlowStatus::NeedsApproval.to_string(), "needs approval");
        assert_eq!(
            serde_json::to_value(FlowStatus::NeedsApproval).unwrap(),
            "needs-approval"
        );
    }

    #[test]
    fn a_flow_says_what_starts_it() {
        let when = |t: serde_json::Value| flow(t).when();
        assert_eq!(
            when(serde_json::json!({ "when": { "Every": 1800 }, "on": [] })),
            "every 30m"
        );
        assert_eq!(
            when(serde_json::json!({ "when": { "Every": 5400 }, "on": [] })),
            "every 1h30m"
        );
        assert_eq!(
            when(serde_json::json!({ "when": { "Cron": "30 7 * * 1-5" }, "on": [] })),
            "30 7 * * 1-5"
        );
        assert_eq!(
            when(serde_json::json!({ "when": null, "on": ["run.failed", "logs.*"] })),
            "on run.failed, logs.*"
        );
        assert_eq!(
            when(serde_json::json!({ "when": { "Every": 60 }, "on": ["run.failed"] })),
            "every 1m or on run.failed"
        );
        assert_eq!(when(serde_json::json!({ "when": null, "on": [] })), "by hand");
    }

    #[test]
    fn what_did_not_load_says_where_and_why() {
        let mut n = NotLoaded {
            flow: "prod-logs".into(),
            file: "/h/flows/prod-logs/flow.md".into(),
            line: Some(3),
            column: None,
            message: "in: ~/x is not a project Relay knows".into(),
        };
        assert_eq!(
            n.to_string(),
            "prod-logs · flow.md:3 · in: ~/x is not a project Relay knows"
        );
        n.column = Some(5);
        assert_eq!(n.at(), "flow.md:3:5");
        n.line = None;
        assert_eq!(n.at(), "flow.md");
    }

    #[test]
    fn catch_up_top_is_read_and_nothing_else_is() {
        let s: FlowSpec = serde_json::from_str(r#"{"catch_up": "top", "steps": {}}"#).unwrap();
        assert_eq!(s.catch_up, Some(CatchUpPlace::Top));
        let none: FlowSpec = serde_json::from_str(r#"{"steps": {}}"#).unwrap();
        assert_eq!(none.catch_up, None);
        let e = serde_json::from_str::<FlowSpec>(r#"{"catch_up": "bottom", "steps": {}}"#).unwrap_err();
        assert!(e.to_string().contains("top"), "{e}");

        let mut f = flow(serde_json::json!({ "when": null, "on": [] }));
        assert!(!f.heads_catch_up());
        f.def["catch_up"] = serde_json::json!("top");
        assert!(f.heads_catch_up());
    }
}
