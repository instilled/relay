//! The review queue's wire types: one item per session that came to rest and
//! waits to be looked at, and the facts of what it changed.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Why it is in the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestKind {
    /// Waiting on you.
    Asks,
    /// Done, for you to look over.
    Finished,
}

impl RestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RestKind::Asks => "asks",
            RestKind::Finished => "finished",
        }
    }
}

impl FromStr for RestKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "asks" => Ok(RestKind::Asks),
            "finished" => Ok(RestKind::Finished),
            other => Err(format!("unknown rest kind {other:?} — use one of: asks, finished")),
        }
    }
}

/// How the rest was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum How {
    Permission,
    Reply,
    Screen,
}

impl How {
    pub fn as_str(self) -> &'static str {
        match self {
            How::Permission => "permission",
            How::Reply => "reply",
            How::Screen => "screen",
        }
    }
}

impl FromStr for How {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "permission" => Ok(How::Permission),
            "reply" => Ok(How::Reply),
            "screen" => Ok(How::Screen),
            other => Err(format!("unknown how {other:?} — use one of: permission, reply, screen")),
        }
    }
}

/// A closed set of notes on a queued item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hint {
    Question,
    Permission,
    Stopped,
    Failed,
    Unsure,
}

impl Hint {
    pub fn as_str(self) -> &'static str {
        match self {
            Hint::Question => "question",
            Hint::Permission => "permission",
            Hint::Stopped => "stopped",
            Hint::Failed => "failed",
            Hint::Unsure => "unsure",
        }
    }
}

impl FromStr for Hint {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "question" => Ok(Hint::Question),
            "permission" => Ok(Hint::Permission),
            "stopped" => Ok(Hint::Stopped),
            "failed" => Ok(Hint::Failed),
            "unsure" => Ok(Hint::Unsure),
            other => Err(format!(
                "unknown hint {other:?} — use one of: question, permission, stopped, failed, unsure"
            )),
        }
    }
}

/// One session waiting to be looked at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueItem {
    pub source: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub kind: RestKind,
    pub how: How,
    pub hints: Vec<Hint>,
    /// When it last came to rest.
    pub at: Timestamp,
    /// How many times it has rested since it was last reviewed.
    pub rests: u32,
    /// Where the unreviewed stretch begins.
    pub from: Timestamp,
    /// The repo's HEAD at `from`, if there is a repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_head: Option<String>,
}

/// What a stretch of work changed in its repo.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Changed {
    pub commits: u32,
    pub added: u64,
    pub removed: u64,
    pub subjects: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub pushed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_are_exact() {
        for (v, s) in [(RestKind::Asks, "asks"), (RestKind::Finished, "finished")] {
            assert_eq!(serde_json::to_string(&v).unwrap(), format!("\"{s}\""));
            assert_eq!(serde_json::from_str::<RestKind>(&format!("\"{s}\"")).unwrap(), v);
            assert_eq!(s.parse::<RestKind>().unwrap(), v);
            assert_eq!(v.as_str(), s);
        }
        for (v, s) in [
            (How::Permission, "permission"),
            (How::Reply, "reply"),
            (How::Screen, "screen"),
        ] {
            assert_eq!(serde_json::to_string(&v).unwrap(), format!("\"{s}\""));
            assert_eq!(serde_json::from_str::<How>(&format!("\"{s}\"")).unwrap(), v);
            assert_eq!(s.parse::<How>().unwrap(), v);
            assert_eq!(v.as_str(), s);
        }
        for (v, s) in [
            (Hint::Question, "question"),
            (Hint::Permission, "permission"),
            (Hint::Stopped, "stopped"),
            (Hint::Failed, "failed"),
            (Hint::Unsure, "unsure"),
        ] {
            assert_eq!(serde_json::to_string(&v).unwrap(), format!("\"{s}\""));
            assert_eq!(serde_json::from_str::<Hint>(&format!("\"{s}\"")).unwrap(), v);
            assert_eq!(s.parse::<Hint>().unwrap(), v);
            assert_eq!(v.as_str(), s);
        }
        assert!("nope".parse::<Hint>().is_err());
    }

    #[test]
    fn the_item_serialises_its_reference_as_ref() {
        let item = QueueItem {
            source: "run".into(),
            reference: "r1".into(),
            kind: RestKind::Asks,
            how: How::Reply,
            hints: vec![Hint::Question],
            at: Timestamp::from_second(10).unwrap(),
            rests: 1,
            from: Timestamp::from_second(0).unwrap(),
            from_head: None,
        };
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v["ref"], "r1");
        assert!(v.get("reference").is_none());
        assert_eq!(serde_json::from_value::<QueueItem>(v).unwrap(), item);
    }
}
