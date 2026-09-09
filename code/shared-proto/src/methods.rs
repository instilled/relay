//! Typed method definitions. One struct per method; the name, params and
//! result live together so client and server cannot drift.

use crate::types::{AttachMode, Event, Mark, Run, RunKind, RunState};
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
    /// Only runs with this cause, e.g. `schedule:nightly`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
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
    /// The child's environment: extras on top of the daemon's own when
    /// `inherit_env` is true, the whole environment when it is false. `rl run`
    /// sends its shell's environment, so the run sees what the shell saw.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default = "default_true")]
    pub inherit_env: bool,
    /// Why this run starts; `manual` unless the daemon's own machinery says.
    #[serde(default)]
    pub cause: crate::types::Cause,
    /// The chain to join; a new one when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
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

/// Attaches this connection to a run's terminal. After the `ok` reply the
/// connection stops speaking JSON-RPC: it carries `rl-session` frames both
/// ways — `Output`, `Note`, `ReplayEnd`, `Exit` to the viewer; `Input` and
/// `Resize` from the joiner (a watcher's are ignored). Scrollback comes first
/// (the journal's tail), then the holder's replay from exactly where the
/// journal stops, then live output. Closing the connection detaches. Several
/// watchers, one joiner: a second `join` is refused unless `take` is set,
/// which turns the previous joiner into a watcher. An ended run replays its
/// journal and sends `Exit` straight away.
pub struct RunAttach;
impl Method for RunAttach {
    const NAME: &'static str = "run.attach";
    type Params = AttachParams;
    type Result = AttachInfo;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachParams {
    pub id: String,
    pub mode: AttachMode,
    /// Who is looking (`user@host`); shown to other viewers, recorded as presence.
    pub client: String,
    /// Bytes of journaled scrollback to send first. Default 256 KiB.
    #[serde(default = "default_scrollback")]
    pub scrollback: u64,
    /// Join even if someone else has: they become a watcher.
    #[serde(default)]
    pub take: bool,
}

fn default_scrollback() -> u64 {
    256 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachInfo {
    pub run: Run,
    /// The pty's size as the holder has it (0×0 for an ended run).
    pub cols: u16,
    pub rows: u16,
    pub tty: bool,
    /// Whether a holder is attached; `false` means only the journal follows.
    pub live: bool,
    /// The current joiner, if any (possibly the caller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joiner: Option<String>,
    /// Output bytes in the journal (where the scrollback ends).
    pub journaled: u64,
    /// Where the holder's replay starts; more than `journaled` means a gap.
    pub replay_from: u64,
}

/// The user has looked at these runs: their current news stops being news.
pub struct RunSeen;
impl Method for RunSeen {
    const NAME: &'static str = "run.seen";
    type Params = RunIds;
    type Result = usize;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunIds {
    pub ids: Vec<String>,
}

// ---- digest.* ---------------------------------------------------------------

/// Every run that had an event at or after `since` (default: everything),
/// plus every run still waiting on a human, each with a one-line summary of
/// where it stands, whether that is news, and who is joined. The fleet and
/// `rl since` are two views of this.
pub struct DigestGet;
impl Method for DigestGet {
    const NAME: &'static str = "digest.get";
    type Params = DigestParams;
    type Result = Digest;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DigestParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Digest {
    pub since: Timestamp,
    pub items: Vec<DigestItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestItem {
    pub run: Run,
    /// Where it stands, in one line: the question, the exit, the last thing said.
    pub summary: String,
    /// When that last happened.
    pub at: Timestamp,
    /// News the user has not looked at (attention states and finishes only).
    pub unseen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joined_by: Option<String>,
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

// ---- schedule.* -------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduleFilter {
    /// Include schedules removed from their file.
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleName {
    pub name: String,
}

pub struct ScheduleList;
impl Method for ScheduleList {
    const NAME: &'static str = "schedule.list";
    type Params = ScheduleFilter;
    type Result = Vec<crate::schedule::Schedule>;
}

/// Fires a schedule now, whatever the clock or its policies say.
pub struct ScheduleRun;
impl Method for ScheduleRun {
    const NAME: &'static str = "schedule.run";
    type Params = ScheduleName;
    type Result = crate::types::Run;
}

pub struct SchedulePause;
impl Method for SchedulePause {
    const NAME: &'static str = "schedule.pause";
    type Params = ScheduleName;
    type Result = crate::schedule::Schedule;
}

pub struct ScheduleResume;
impl Method for ScheduleResume {
    const NAME: &'static str = "schedule.resume";
    type Params = ScheduleName;
    type Result = crate::schedule::Schedule;
}

/// Re-reads `schedules.yaml`.
pub struct ScheduleReload;
impl Method for ScheduleReload {
    const NAME: &'static str = "schedule.reload";
    type Params = ();
    type Result = crate::schedule::ReloadReport;
}
