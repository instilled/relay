//! `flow.test`: what firing a flow would do now, told without starting anything.

use super::{FiringStatus, Overlap};
use crate::methods::Method;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What firing a flow would do now: whether it may by itself, when, whether an event would fire
/// it, and which steps would run. Nothing is started.
pub struct FlowTest;
impl Method for FlowTest {
    const NAME: &'static str = "flow.test";
    type Params = FlowTestParams;
    type Result = Tested;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowTestParams {
    pub name: String,
    /// An event, by id: whether it would fire the flow, and what the steps would make of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tested {
    pub flow: String,
    /// What starts it, as `Flow::when` says it.
    pub when: String,
    /// The event kinds its `on` takes.
    pub on: Vec<String>,
    /// Why nothing would fire it by itself, if nothing would.
    pub held: Option<Held>,
    /// For `when`: the next three times it would fire.
    pub next: Vec<Timestamp>,
    /// A firing of it still going, which a new one would meet.
    pub going: Option<Going>,
    /// The event asked about, and whether it would fire the flow.
    pub event: Option<EventTest>,
    /// In the order the flow writes them.
    pub steps: Vec<StepTest>,
}

/// Why a flow would fire by itself on nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Held {
    /// The folder as it is has no yes.
    NeedsYes,
    /// Paused: only a hand fires it.
    Paused,
}

/// A firing still going when a new one would start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Going {
    pub firing: String,
    pub status: FiringStatus,
    pub overlap: Overlap,
}

/// Whether an event would fire the flow, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTest {
    pub id: i64,
    pub kind: String,
    pub summary: String,
    pub fires: bool,
    pub why: String,
    /// Over five minutes old: the loop passes over an event that old, as history.
    pub old: bool,
}

/// One step, as a firing started now would have it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepTest {
    pub name: String,
    /// What it does, in a line.
    pub does: String,
    pub runs: Runs,
    /// Where it would run; none when that is where an event happens, and no event was named.
    pub folder: Option<PathBuf>,
}

/// Whether a step would run in a firing started now, as far as can be told before it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runs {
    Yes,
    /// Only if each of these holds when it is reached: its own `if:`, and those of the steps it waits on.
    If(Vec<String>),
    /// It would not, and why.
    No(String),
}
