//! A lead: one thing worth your attention, from one source. The one object of
//! the report — projected from Relay's own state, or posted by a flow.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;

/// What ranks a lead. The only thing that does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    /// Cannot proceed without you. Pinned to the top.
    NeedsYou,
    /// Happening now, may change.
    Live,
    /// Done, here for the record.
    Happened,
}

impl Band {
    pub fn rank(self) -> u8 {
        match self {
            Band::NeedsYou => 0,
            Band::Live => 1,
            Band::Happened => 2,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Band::NeedsYou => "needs_you",
            Band::Live => "live",
            Band::Happened => "happened",
        }
    }
}

impl FromStr for Band {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "needs_you" | "needs-you" => Ok(Band::NeedsYou),
            "live" => Ok(Band::Live),
            "happened" => Ok(Band::Happened),
            other => Err(format!(
                "unknown band {other:?} — use one of: needs_you, live, happened"
            )),
        }
    }
}

/// Where a lead stands. User leads are `Open` until cleared; a dismiss is a
/// user action from the pane (step 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeadStatus {
    Open,
    Resolved,
    Dismissed,
}

/// A single thing worth your attention, from one source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lead {
    /// Who raised it — a flow name, an `rl lead` source, or `relay` for built-ins.
    pub source: String,
    /// The source's own id for this lead, stable across re-emits.
    pub key: String,
    pub band: Band,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// A project/folder this is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<PathBuf>,
    /// How to act on it — a command to run or a thing to open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    pub status: LeadStatus,
    /// Created / last updated.
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
    /// A finished run's frozen facts — set only on built-in run leads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<crate::RunFacts>,
    /// A run lead's queue item, set while it waits to be reviewed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest: Option<crate::queue::LeadRest>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn band_parses_and_ranks() {
        assert_eq!(Band::from_str("needs_you").unwrap(), Band::NeedsYou);
        assert_eq!(Band::from_str("live").unwrap(), Band::Live);
        assert_eq!(Band::from_str("happened").unwrap(), Band::Happened);
        assert!(Band::NeedsYou.rank() < Band::Live.rank());
        assert!(Band::Live.rank() < Band::Happened.rank());
    }

    #[test]
    fn band_rejects_unknown() {
        let err = Band::from_str("urgent").unwrap_err();
        assert!(err.contains("needs_you"), "error names the valid bands: {err}");
    }

    #[test]
    fn lead_round_trips_json() {
        let l = Lead {
            source: "feedback".into(),
            key: "notion:triage".into(),
            band: Band::NeedsYou,
            title: "5 feedback items to triage".into(),
            summary: None,
            scope: None,
            action: Some("claude -p 'triage the feedback queue'".into()),
            status: LeadStatus::Open,
            at: Timestamp::now(),
            expires_at: None,
            facts: None,
            rest: None,
        };
        let s = serde_json::to_string(&l).unwrap();
        let back: Lead = serde_json::from_str(&s).unwrap();
        assert_eq!(l, back);
        assert!(s.contains("\"needs_you\""), "band serializes snake_case");
    }
}
