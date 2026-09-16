//! Decisions: a run's question to a human, with the next steps already
//! written down. Raising one does not stop the run — it outlives it — and
//! choosing an option starts the runs that option carries, with
//! `cause = decision:<id>` and the same `flow`, so the trail from schedule to
//! question to follow-up is one chain.

use crate::action::action_struct;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    /// Waiting for a human.
    Open,
    /// A human (or a default) chose.
    Taken,
    /// Answered with "not this".
    Dismissed,
    /// Ran out of time with no default to fall back on.
    Expired,
}

impl DecisionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Taken => "taken",
            Self::Dismissed => "dismissed",
            Self::Expired => "expired",
        }
    }
    pub fn is_open(self) -> bool {
        self == Self::Open
    }
}

impl std::str::FromStr for DecisionStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "open" => Self::Open,
            "taken" => Self::Taken,
            "dismissed" => Self::Dismissed,
            "expired" => Self::Expired,
            other => return Err(format!("unknown decision status: {other}")),
        })
    }
}

impl std::fmt::Display for DecisionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

action_struct! {
    /// One way to answer. `key` is what you type, `label` is what you read,
    /// and its `action()` is what happens when it is chosen — nothing, for
    /// an option that is only an answer.
    #[serde(deny_unknown_fields)]
    struct DecisionOption {
        before: {
            pub key: String,
            #[serde(default)]
            pub label: String,
            /// Taken on its own when the decision expires.
            #[serde(default, skip_serializing_if = "std::ops::Not::not")]
            pub default: bool,
        }
        after: {}
    }
}

impl DecisionOption {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            ..Default::default()
        }
    }

    /// The label if there is one, else the key.
    pub fn text(&self) -> &str {
        if self.label.is_empty() { &self.key } else { &self.label }
    }
}

/// A question waiting on a human.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    pub workspace: PathBuf,
    /// The run that raised it, if it was a run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// The chain it belongs to; the raising run's flow, or its own id.
    pub flow: String,
    /// Asking the same thing again replaces the open one with this key
    /// instead of piling up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub options: Vec<DecisionOption>,
    /// Several options may be chosen at once.
    #[serde(default)]
    pub multi: bool,
    pub status: DecisionStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<Timestamp>,
    /// Option keys, once taken.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen: Vec<String>,
    /// Runs those options started.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcome_runs: Vec<String>,
    /// Why it ended that way: a human's note, or `expired`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Decision {
    pub fn option(&self, key: &str) -> Option<&DecisionOption> {
        self.options.iter().find(|o| o.key == key)
    }

    /// The option a decision falls back on when it expires.
    pub fn default_option(&self) -> Option<&DecisionOption> {
        self.options.iter().find(|o| o.default)
    }

    /// `fix, ignore` — the keys, for one-line lists.
    pub fn keys(&self) -> String {
        self.options
            .iter()
            .map(|o| o.key.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_option_carries_its_action_inline() {
        let json = serde_json::json!({
            "key": "fix",
            "label": "Open a PR",
            "run": "echo hi",
        });
        let o: DecisionOption = serde_json::from_value(json).unwrap();
        assert_eq!(o.key, "fix");
        assert_eq!(o.action().run, Some(crate::action::Task::Shell("echo hi".into())));
        assert!(!o.action().is_empty());
        assert!(DecisionOption::new("ignore", "Ignore").action().is_empty());
    }

    #[test]
    fn status_round_trips() {
        assert_eq!(serde_json::to_string(&DecisionStatus::Taken).unwrap(), "\"taken\"");
        assert_eq!("expired".parse::<DecisionStatus>().unwrap(), DecisionStatus::Expired);
        assert!("nope".parse::<DecisionStatus>().is_err());
    }
}
