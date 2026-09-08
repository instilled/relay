//! Typed method definitions. One struct per method; the name, params and
//! result live together so client and server cannot drift.

use crate::types::{Event, Mark, Run, RunKind, RunState};
use jiff::Timestamp;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
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

/// Starts a run under a fresh session holder. Returns once the holder is
/// ready and the run is in the ledger.
pub struct RunStart;
impl Method for RunStart {
    const NAME: &'static str = "run.start";
    type Params = RunSpec;
    type Result = Run;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSpec {
    pub argv: Vec<String>,
    /// `command` unless the caller says otherwise (`--agent` → `agent`).
    #[serde(default = "RunKind::default_command")]
    pub kind: RunKind,
    pub cwd: PathBuf,
    /// Defaults to the argv joined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// A pty (default) or plain pipes.
    #[serde(default = "default_true")]
    pub tty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
    /// Extra environment for the child.
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

fn default_true() -> bool {
    true
}

impl RunKind {
    fn default_command() -> Self {
        RunKind::Command
    }
}

/// Asks a live run to stop: SIGTERM, or SIGKILL with `force`. The run ends in
/// `stopped`. Returns the run as it is right after the signal is sent.
pub struct RunStop;
impl Method for RunStop {
    const NAME: &'static str = "run.stop";
    type Params = RunStopParams;
    type Result = Run;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStopParams {
    pub id: String,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunId {
    pub id: String,
}

/// A CLI's own signal about a run, as delivered by `rl hook <event>`. The
/// payload is the hook's JSON, passed through. Returns the run afterwards;
/// events that say nothing about state leave it untouched.
pub struct RunHook;
impl Method for RunHook {
    const NAME: &'static str = "run.hook";
    type Params = RunHookParams;
    type Result = Run;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunHookParams {
    pub id: String,
    /// Which CLI's vocabulary `event` and `payload` use. Only `claude` today.
    #[serde(default = "default_dialect")]
    pub dialect: String,
    pub event: String,
    #[serde(default)]
    pub payload: Value,
}

fn default_dialect() -> String {
    "claude".into()
}

/// A human's word on a run's state, for CLIs with no signals at all.
pub struct RunMark;
impl Method for RunMark {
    const NAME: &'static str = "run.mark";
    type Params = RunMarkParams;
    type Result = Run;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMarkParams {
    pub id: String,
    pub mark: Mark,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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
