//! Typed method definitions. One struct per method; the name, params and
//! result live together so client and server cannot drift.

use crate::types::{Event, Run, RunState};
use jiff::Timestamp;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::PathBuf;

pub trait Method {
    const NAME: &'static str;
    type Params: Serialize + DeserializeOwned + Send;
    type Result: Serialize + DeserializeOwned + Send;
}

// ---- daemon.* ---------------------------------------------------------------

pub struct DaemonPing;
impl Method for DaemonPing {
    const NAME: &'static str = "daemon.ping";
    type Params = ();
    type Result = DaemonInfo;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub protocol: u32,
    /// Cargo package version of the running `rld`.
    pub version: String,
    pub pid: u32,
    pub started_at: Timestamp,
    pub home: PathBuf,
}

/// Graceful stop. The daemon replies, then exits. Never touches session holders.
pub struct DaemonShutdown;
impl Method for DaemonShutdown {
    const NAME: &'static str = "daemon.shutdown";
    type Params = ();
    type Result = ();
}

// ---- run.* ------------------------------------------------------------------

pub struct RunList;
impl Method for RunList {
    const NAME: &'static str = "run.list";
    type Params = RunFilter;
    type Result = Vec<Run>;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunFilter {
    /// Only these states. Empty means any.
    #[serde(default)]
    pub states: Vec<RunState>,
    /// Drop finished/failed/stopped runs.
    #[serde(default)]
    pub active_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Newest first; default 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

pub struct RunGet;
impl Method for RunGet {
    const NAME: &'static str = "run.get";
    type Params = RunId;
    type Result = Run;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunId {
    pub id: String,
}

// ---- event.* ----------------------------------------------------------------

pub struct EventList;
impl Method for EventList {
    const NAME: &'static str = "event.list";
    type Params = EventFilter;
    type Result = Vec<Event>;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Only these kinds. Empty means any.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Oldest first; default 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

// ---- presence.* -------------------------------------------------------------

/// Records that a client is looking now. Returns when it last did, which is
/// the boundary `rl since` uses by default.
pub struct PresenceTouch;
impl Method for PresenceTouch {
    const NAME: &'static str = "presence.touch";
    type Params = PresenceClient;
    type Result = PresenceInfo;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceClient {
    pub client: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceInfo {
    pub client: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<Timestamp>,
}
