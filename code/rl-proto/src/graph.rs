//! The wiring graph: everything that starts work, what it runs, and what sets
//! what off, across every project at once.
//!
//! A flow hears events from every folder, whatever its `in`, so the network
//! is one graph, not one per project; a node carries its folder as a fact
//! about it, never as a box around it. Events are not nodes but the labels on
//! the edges between the things that raise them and the flows that hear them.
//!
//! Every flow is there before anything has fired, and so are its steps,
//! wired as the flow writes them; each `on` nothing
//! raised is a way in. What only the ledger knows — which flow's runs raised
//! which event, which questions a step asked — is drawn from what happened in
//! the window, with a count.

use crate::flow::FlowStatus;
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
    /// A flow: `~/.relay/flows/<name>`.
    Flow,
    /// A flow's ask step, and the questions it put to you.
    Ask,
    /// A shell line or command.
    Shell,
    /// An agent CLI.
    Agent,
    /// An event kind a flow listens for that nothing in the window raised.
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
    /// A flow's name, a step's, an event kind.
    pub name: String,
    /// The folder it belongs to; none for a flow with no `in` and what is not in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    /// The flow's own state, for a flow and its steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<FlowStatus>,
    /// What starts a flow: `every 30m`, `on run.failed`; what a step runs, or the newest question an
    /// ask step raised.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// When a flow fires next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at: Option<Timestamp>,
    /// Runs it stands for, in the window.
    #[serde(default, skip_serializing_if = "Tally::is_empty")]
    pub runs: Tally,
    /// An ask step's questions still waiting for you, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting: Vec<String>,
    /// The flow it is, or is a step of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
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
    /// A flow says so, rather than only the ledger.
    pub declared: bool,
}
