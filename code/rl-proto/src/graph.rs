//! The wiring graph: everything that starts work, what it runs, and what sets
//! what off, across every project at once.
//!
//! A reaction hears events from every folder, whatever file it lives in, so
//! the network is one graph, not one per project; a node carries its folder
//! as a fact about it, never as a box around it. Events are not nodes but the
//! labels on the edges between the things that raise them and the reactions
//! that hear them.
//!
//! What the files say (a schedule runs this, a reaction asks that, an answer
//! runs the other) is always there, before anything has fired. What only the
//! ledger knows — which command raised which event — is drawn from what
//! happened in the window, with a count.

use crate::rule::RuleStatus;
use crate::types::RunState;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    /// The window the counts are over.
    pub since: Timestamp,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Graph {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// What a node is, which is also how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A clock: `schedules.yaml`.
    Schedule,
    /// An ear: `reactions.yaml`.
    Reaction,
    /// A reaction's question, put to you instead of running something.
    Ask,
    /// A shell line or command.
    Shell,
    /// An agent CLI.
    Agent,
    /// An event kind a reaction listens for that nothing in the window raised.
    Event,
    /// Events raised from outside any run — `rl emit` in your own terminal, a
    /// hook — in one folder.
    Outside,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Stable across calls, so a cursor can stay on it.
    pub id: String,
    pub kind: NodeKind,
    /// A rule's name, a command as written, an event kind.
    pub name: String,
    /// The folder it belongs to; none for the global file and what is not in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    /// The rule it is, or comes from: `schedule:<scope>/<name>`, the key the
    /// rules commands take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// The rule's own state, for rules and what they run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RuleStatus>,
    /// `cron 0 7 * * *`, `on run.failed`, the question's title.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// When a schedule fires next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at: Option<Timestamp>,
    /// Runs it stands for, in the window.
    #[serde(default, skip_serializing_if = "Tally::is_empty")]
    pub runs: Tally,
    /// A question's ids still waiting for you, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting: Vec<String>,
}

/// How a node's runs went in the window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tally {
    pub total: u64,
    pub failed: u64,
    /// Not ended yet.
    pub going: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<RunState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_at: Option<Timestamp>,
}

impl Tally {
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// The event kinds it carries (`run.failed`), or an answer's key.
    pub label: String,
    /// How many times, in the window.
    pub count: u64,
    /// A file says so, rather than only the ledger.
    pub declared: bool,
}
