//! Reactions: event → an action, or a question.
//!
//! `reactions.yaml` sits beside `schedules.yaml`, global or per workspace, and
//! is approved the same way. A rule says which events it wants and what to do
//! about them, in the same `run:` grammar everything else uses.
//!
//! ```yaml
//! reactions:
//!   tell-me-when-asked:
//!     on: decision.raised
//!     run: 'osascript -e "display notification \"{summary}\""'
//!   look-at-failures:
//!     on: [run.failed, run.blocked]
//!     where: { tag: nightly }
//!     run: { agent: claude, prompt: "{title} failed. Say whether it is flaky." }
//! ```

use crate::action::action_struct;
use crate::decision::DecisionOption;
use jiff::Timestamp;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

action_struct! {
    /// One entry of `reactions.yaml`, as written. Exactly one of `run` /
    /// `ask`. Without a `cwd`, a workspace's runs in the folder, and a global
    /// one where the event happened: the folder the event's run ran in while
    /// it is there, else the event's workspace, else the Relay home. A
    /// relative `cwd` is inside that same folder, and `{…}` fill it as they
    /// fill `run`.
    #[serde(deny_unknown_fields)]
    struct ReactionSpec {
        before: {
            /// Event kinds, one or a list. A trailing `*` matches a family:
            /// `run.*` takes every event about runs, `*` takes the lot.
            #[serde(deserialize_with = "one_or_many")]
            pub on: Vec<String>,
            /// Narrows it further, against the run the event is about.
            #[serde(rename = "where", default, skip_serializing_if = "Option::is_none")]
            pub filter: Option<Filter>,
            /// Event kinds what it runs raises (`rl emit`), for the graph.
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            pub emits: Vec<String>,
        }
        after: {
            /// Or a question to put to a human instead.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub ask: Option<AskSpec>,
            /// Not again within this long (`10s` by default, `0` to allow every one).
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub throttle: Option<String>,
        }
    }
}

/// How long a rule waits before it may fire again, unless it says otherwise.
pub const DEFAULT_THROTTLE: &str = "10s";

impl ReactionSpec {
    /// `run.failed, run.blocked`, for lists.
    pub fn when(&self) -> String {
        self.on.join(", ")
    }

    /// Whether this rule wants an event of this kind.
    pub fn matches_kind(&self, kind: &str) -> bool {
        self.on.iter().any(|p| match p.strip_suffix('*') {
            Some(prefix) => kind.starts_with(prefix),
            None => p == kind,
        })
    }
}

/// What else has to be true of the run an event is about. Every field given
/// must match; a rule with none takes every event of its kinds.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    /// The run carries this tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Its cause starts with this: `schedule:`, or `schedule:nightly`.
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

/// The question a reaction puts instead of running something.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskSpec {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub options: Vec<DecisionOption>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi: bool,
    /// Asking again replaces the open question with this key. Defaults to the
    /// reaction's own name, so a noisy event leaves one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}

/// A reaction as the daemon tracks it: the spec, its version, and what has
/// happened to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reaction {
    /// `global`, or a workspace path.
    pub scope: String,
    pub name: String,
    /// sha256 of the spec; the version.
    pub hash: String,
    pub source: PathBuf,
    pub spec: ReactionSpec,
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<Timestamp>,
    /// The last run it started, if its action was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<Timestamp>,
    /// The version the user said yes to; it fires only while this is `hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_hash: Option<String>,
}

impl Reaction {
    pub fn approved(&self) -> bool {
        self.approved_hash.as_deref() == Some(&self.hash)
    }

    /// `global`, or the workspace folder's name.
    pub fn scope_label(&self) -> String {
        crate::schedule::scope_label(&self.scope)
    }
}

/// `on: run.failed` and `on: [run.failed, run.blocked]` both mean a list.
fn one_or_many<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    let kinds = match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    };
    if kinds.is_empty() || kinds.iter().any(|k| k.trim().is_empty()) {
        return Err(serde::de::Error::custom("`on` needs an event kind, or a list of them"));
    }
    Ok(kinds)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `on:` as JSON, which is what the wire and the YAML both come down to.
    fn spec(on: &str) -> ReactionSpec {
        let json = format!(r#"{{"on": {on}, "run": "x"}}"#);
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn on_takes_one_kind_or_many() {
        assert_eq!(spec(r#""run.failed""#).on, ["run.failed"]);
        assert_eq!(
            spec(r#"["run.failed", "run.blocked"]"#).on,
            ["run.failed", "run.blocked"]
        );
    }

    #[test]
    fn a_trailing_star_matches_a_family() {
        let s = spec(r#""run.*""#);
        assert!(s.matches_kind("run.failed"));
        assert!(s.matches_kind("run.started"));
        assert!(!s.matches_kind("decision.raised"));
        let s = spec(r#""*""#);
        assert!(s.matches_kind("anything.at.all"));
        let s = spec(r#""run.failed""#);
        assert!(s.matches_kind("run.failed"));
        assert!(!s.matches_kind("run.failed.later"));
    }

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
}
