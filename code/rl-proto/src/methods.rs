//! Typed method definitions. One struct per method; the name, params and
//! result live together so client and server cannot drift.

use crate::decision::{Decision, DecisionOption};
use crate::types::{AttachMode, Checkout, Event, Mark, Run, RunKind, RunState, Signal, SummaryTarget};
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
    /// Include runs the user is done with (`run.archive`).
    #[serde(default)]
    pub archived: bool,
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
    /// Whether the child has input: what is typed at it (default), or none at
    /// all — end of input at once, for a headless run that must not wait on
    /// it. A pty always has input.
    #[serde(default = "default_true")]
    pub input: bool,
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

/// Feed bytes to a run's terminal, as if typed — for a warm project shell a
/// command line is written straight to its stdin without a full join.
pub struct RunInput;
impl Method for RunInput {
    const NAME: &'static str = "run.input";
    type Params = RunInputParams;
    type Result = ();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunInputParams {
    pub id: String,
    /// The bytes to write; the caller adds any trailing newline.
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunId {
    pub id: String,
}

/// Emit a custom event into the ledger, from a command or plugin. A reaction
/// picks it up like any built-in event. Inside a run, `RELAY_RUN` attributes
/// it to that run's workspace and flow; outside one, the caller names the
/// folder. The kind is the caller's own dotted name (`log.issue`); the
/// daemon's own kinds (`run.*`, `decision.*`, …) are refused.
pub struct EventEmit;
impl Method for EventEmit {
    const NAME: &'static str = "event.emit";
    type Params = EventEmitParams;
    type Result = ();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEmitParams {
    pub kind: String,
    /// The run it belongs to; `RELAY_RUN` when emitting inside one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// The folder it is about, when no run says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// Override the group key (default: the run's, else the folder path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// A one-line summary (default: the kind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Arbitrary structured payload, reachable in a reaction as `{data.<key>}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
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
    /// Which of the client's terminals this is (its process, say): two
    /// terminals of one person are two seats, and only one has the keyboard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
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

/// Gives a run a new title, in place. A run is titled when it starts and
/// nothing writes the title afterwards, so a rename is not overwritten. An
/// empty title undoes the rename: the run goes back to the name Relay gave
/// it — what it started as, or what its first prompt named it.
pub struct RunRename;
impl Method for RunRename {
    const NAME: &'static str = "run.rename";
    type Params = RunRenameParams;
    type Result = crate::types::Run;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRenameParams {
    pub id: String,
    pub title: String,
}

/// Done with these runs: out of the panel and `rl since`, kept in the
/// ledger. Only ended runs are; the result is how many were.
pub struct RunArchive;
impl Method for RunArchive {
    const NAME: &'static str = "run.archive";
    type Params = RunArchiveParams;
    type Result = usize;
}

/// Picks ended runs back up, each as a new run in its folder under its
/// title: an agent's conversation where its CLI can resume one (`claude
/// --resume`, `codex resume`), a shell as a fresh shell, a command run again.
/// The run picked up leaves the list. One outcome per run asked about.
pub struct RunResume;
impl Method for RunResume {
    const NAME: &'static str = "run.resume";
    type Params = RunResumeParams;
    type Result = Vec<Resumed>;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunResumeParams {
    #[serde(default)]
    pub ids: Vec<String>,
    /// Instead: every run lost in a restart that is still on the list (see
    /// `Digest::lost`) — agents and shells, and plain commands only when
    /// `commands` says so, since running one again can do things twice.
    #[serde(default)]
    pub lost: bool,
    #[serde(default)]
    pub commands: bool,
    /// The environment the new runs start in, as for `run.start`; empty means
    /// the daemon's own.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
}

/// `Resumed::why_not` for a command `lost` left out because `commands` was
/// not set: running one again can do what it did twice.
pub const RESUME_LEFT_OUT: &str = "a command is run again only when asked";

/// What became of one run asked to resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resumed {
    /// The run asked about.
    pub from: String,
    pub title: String,
    /// The run that carries it on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<crate::types::Run>,
    /// Why there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_not: Option<String>,
}

/// Keeps a run at the top of the panel, or lets it go back among the rest.
/// Answers with the run as it now stands.
pub struct RunPin;
impl Method for RunPin {
    const NAME: &'static str = "run.pin";
    type Params = RunPinParams;
    type Result = crate::types::Run;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPinParams {
    pub id: String,
    pub pinned: bool,
}

/// Takes a run off the panel's list, or brings it back: it stays what it is,
/// going or ended, and shows only in the `hidden` lens — unless it comes to
/// need you after it was hidden. Hiding again moves the time on, so what came
/// through goes back. Answers with the run as it now stands.
pub struct RunHide;
impl Method for RunHide {
    const NAME: &'static str = "run.hide";
    type Params = RunHideParams;
    type Result = crate::types::Run;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunHideParams {
    pub id: String,
    pub hidden: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunArchiveParams {
    #[serde(default)]
    pub ids: Vec<String>,
    /// Instead: everything ended that has been looked at.
    #[serde(default)]
    pub all_seen: bool,
}

// ---- graph.* ----------------------------------------------------------------

/// The wiring graph over every project: rules, what they run, the questions
/// they ask, and what set what off since `since` (default: the last 7 days).
pub struct GraphGet;
impl Method for GraphGet {
    const NAME: &'static str = "graph.get";
    type Params = GraphParams;
    type Result = crate::graph::Graph;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
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

/// Takes a whole project, or a job, off the panel's list — or brings it back
/// — by its row's key: `g:<folder>` a project's header (`g:everywhere` the
/// global rules' runs outside every project), `j:<kind>:<name>@<group>` a
/// job. Everything under it is hidden with it, runs that come later too;
/// nothing is stopped or removed. Answers with everything hidden so.
pub struct DigestHide;
impl Method for DigestHide {
    const NAME: &'static str = "digest.hide";
    type Params = DigestHideParams;
    type Result = Vec<Hidden>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestHideParams {
    pub key: String,
    pub hidden: bool,
}

/// A project or a job taken off the list, and since when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hidden {
    pub key: String,
    pub at: Timestamp,
}

impl Hidden {
    /// Whether `key` names something `digest.hide` can hide.
    pub fn valid_key(key: &str) -> bool {
        key.strip_prefix("g:")
            .or_else(|| key.strip_prefix("j:"))
            .is_some_and(|k| !k.is_empty())
    }
}

/// Keeps a project's rows in the order given — moved by hand on the panel
/// (shift-↑/↓) — or, with no rows, lets it sort itself again (`/sort`). The
/// project is its header's key (`g:<folder>`, `g:everywhere`); the rows are
/// run (`r:<id>`) and job (`j:<kind>:<name>@<group>`) keys, first on top.
/// The order given replaces the one kept, so what has left the list drops
/// out. Answers with every project's order.
pub struct DigestOrder;
impl Method for DigestOrder {
    const NAME: &'static str = "digest.order";
    type Params = Order;
    type Result = Vec<Order>;
}

/// A project's rows in the order you put them, first on top.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    /// The project's header key: `g:<folder>`, or `g:everywhere`.
    pub group: String,
    /// Run and job keys, `r:<id>` and `j:<kind>:<name>@<group>`.
    #[serde(default)]
    pub rows: Vec<String>,
}

impl Order {
    /// Whether `key` names a project an order can be kept for.
    pub fn valid_group(key: &str) -> bool {
        key.strip_prefix("g:").is_some_and(|k| !k.is_empty())
    }

    /// Whether `key` names a row that can be put in order: a run or a job.
    pub fn valid_row(key: &str) -> bool {
        key.strip_prefix("r:")
            .or_else(|| key.strip_prefix("j:"))
            .is_some_and(|k| !k.is_empty())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Digest {
    pub since: Timestamp,
    pub items: Vec<DigestItem>,
    /// Everything still waiting on an answer, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<Decision>,
    /// The git checkout of every folder the list shows that is in one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkouts: Vec<Checkout>,
    /// Runs lost in a restart and still on the list, oldest first: what
    /// `run.resume` with `lost` would pick up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lost: Vec<String>,
    /// The projects and jobs taken off the list (`digest.hide`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<Hidden>,
    /// The projects whose rows you put in order (`digest.order`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<Order>,
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
    /// What the run raised itself (`rl emit`), by kind, in the order they
    /// first came.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emitted: Vec<Signal>,
    /// The folder a live run last said it was in (OSC 7) — a shell's, at its
    /// prompt. Unknown until it says so; forgotten when the run ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<std::path::PathBuf>,
    /// A Claude or Codex run's latest recap, its first line cut for a row —
    /// the agent's own, kept as it is written, or one asked for since. The
    /// whole of it is `summary.get`'s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recap: Option<String>,
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
    /// Oldest first, unless `newest_first`; default 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// The newest `limit` rather than the oldest, newest first — in the
    /// order they were written (by id), so a page ends where the next begins.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub newest_first: bool,
    /// Only events written before this one: the page past one already read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<i64>,
    /// Only events written after this one: what came since one already read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
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
    /// `global` or a workspace path; needed when the name is in several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
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

// ---- workspace.* ------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceUpParams {
    pub path: PathBuf,
    /// Approve every pending schedule version in it.
    #[serde(default)]
    pub approve: bool,
}

/// Registers a folder and loads its `.relay/`.
pub struct WorkspaceUp;
impl Method for WorkspaceUp {
    const NAME: &'static str = "workspace.up";
    type Params = WorkspaceUpParams;
    type Result = crate::schedule::UpReport;
}

pub struct WorkspaceList;
impl Method for WorkspaceList {
    const NAME: &'static str = "workspace.list";
    type Params = ();
    type Result = Vec<crate::schedule::Workspace>;
}

/// Unregisters a folder: its rules stop, are marked removed and forget their
/// approvals, and its open questions are dismissed. The folder need not exist
/// any more, nor have been registered — a folder the list shows only for its
/// runs is removed by what the flags do to those. Asked to, it also stops
/// every run still going there, archives what has ended, and moves the rule
/// files of its `.relay/` into the Relay home's `removed/`.
pub struct WorkspaceRemove;
impl Method for WorkspaceRemove {
    const NAME: &'static str = "workspace.remove";
    type Params = WorkspaceRemoveParams;
    type Result = WorkspaceRemoved;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceRemoveParams {
    pub path: PathBuf,
    /// Stop every run in the folder still going — shells, agents, commands —
    /// and wait (a while, not for ever) for them to end.
    #[serde(default)]
    pub stop_runs: bool,
    /// Archive the folder's ended runs, those just stopped included.
    #[serde(default)]
    pub archive_runs: bool,
    /// Move `.relay/schedules.yaml` and `reactions.yaml` to
    /// `<home>/removed/<name>-<when>/`. Refused, before anything is done,
    /// when `.relay` is a symlink or is the Relay home.
    #[serde(default)]
    pub move_rules: bool,
}

/// What removing a folder did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceRemoved {
    /// The folder as the daemon took it.
    pub path: PathBuf,
    /// It was registered, and no longer is.
    pub removed: bool,
    /// Runs asked to stop that have ended.
    pub stopped: usize,
    /// Runs asked to stop that were still going when the wait ran out, or
    /// that no holder of this daemon's could be told to stop.
    #[serde(default)]
    pub still_running: usize,
    pub archived: usize,
    /// Open questions from the folder, dismissed.
    #[serde(default)]
    pub dismissed: usize,
    /// Where the rule files went, when they were moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_moved_to: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRenameParams {
    pub path: PathBuf,
    pub name: String,
}

/// Gives a registered folder a new display name, in place.
pub struct WorkspaceRename;
impl Method for WorkspaceRename {
    const NAME: &'static str = "workspace.rename";
    type Params = WorkspaceRenameParams;
    type Result = crate::schedule::Workspace;
}

// ---- decision.* -------------------------------------------------------------

/// Raises a question and returns it. With a `group`, an open decision with
/// that key is replaced in place — the same id, the new wording — so a
/// schedule that asks nightly leaves one line, not thirty.
pub struct DecisionRaise;
impl Method for DecisionRaise {
    const NAME: &'static str = "decision.raise";
    type Params = DecisionSpec;
    type Result = Decision;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecisionSpec {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub options: Vec<DecisionOption>,
    /// Several options may be chosen at once.
    #[serde(default)]
    pub multi: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The run asking; `RELAY_RUN` when `rl ask` runs inside one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Where its options run, when no run says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// How long it stands (`4h`, `2d`). Then the default option is taken, or
    /// it expires unanswered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}

pub struct DecisionList;
impl Method for DecisionList {
    const NAME: &'static str = "decision.list";
    type Params = DecisionFilter;
    type Result = Vec<Decision>;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecisionFilter {
    /// Include the ones already answered.
    #[serde(default)]
    pub all: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,
    /// Newest first; default 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

pub struct DecisionGet;
impl Method for DecisionGet {
    const NAME: &'static str = "decision.get";
    type Params = DecisionId;
    type Result = Decision;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionId {
    pub id: String,
}

/// Chooses. Every chosen option that carries an action starts a run
/// (`cause = decision:<id>`, the decision's flow); the ids come back on the
/// decision. Answering twice is refused.
pub struct DecisionTake;
impl Method for DecisionTake {
    const NAME: &'static str = "decision.take";
    type Params = DecisionTakeParams;
    type Result = Decision;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionTakeParams {
    pub id: String,
    /// Option keys. More than one only when the decision is `multi`.
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Answers "not this": nothing runs, the question stops waiting.
pub struct DecisionDismiss;
impl Method for DecisionDismiss {
    const NAME: &'static str = "decision.dismiss";
    type Params = DecisionNote;
    type Result = Decision;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionNote {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

// ---- rule.* -----------------------------------------------------------------

/// A rule by name. `scope` settles it when the name exists in several,
/// `kind` when a schedule and a reaction share it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<crate::rule::RuleKind>,
}

/// Every schedule and reaction as one list: what starts work without you.
pub struct RuleList;
impl Method for RuleList {
    const NAME: &'static str = "rule.list";
    type Params = ScheduleFilter;
    type Result = Vec<crate::rule::Rule>;
}

/// What did not load at the last reload of each rules file, the global ones
/// and every folder's: it stays said until the file is fixed and reloaded.
pub struct RuleErrors;
impl Method for RuleErrors {
    const NAME: &'static str = "rule.errors";
    type Params = ();
    type Result = Vec<crate::rule::NotLoaded>;
}

/// Says yes to the current version of one rule; it may fire from now on.
pub struct RuleApprove;
impl Method for RuleApprove {
    const NAME: &'static str = "rule.approve";
    type Params = RuleRef;
    type Result = crate::rule::Rule;
}

// ---- reaction.* -------------------------------------------------------------

pub struct ReactionList;
impl Method for ReactionList {
    const NAME: &'static str = "reaction.list";
    type Params = ScheduleFilter;
    type Result = Vec<crate::reaction::Reaction>;
}

pub struct ReactionPause;
impl Method for ReactionPause {
    const NAME: &'static str = "reaction.pause";
    type Params = ScheduleName;
    type Result = crate::reaction::Reaction;
}

pub struct ReactionResume;
impl Method for ReactionResume {
    const NAME: &'static str = "reaction.resume";
    type Params = ScheduleName;
    type Result = crate::reaction::Reaction;
}

/// Re-reads every `reactions.yaml`.
pub struct ReactionReload;
impl Method for ReactionReload {
    const NAME: &'static str = "reaction.reload";
    type Params = ();
    type Result = crate::schedule::ReloadReport;
}

/// What every rule would do with one event, without doing any of it.
pub struct ReactionTest;
impl Method for ReactionTest {
    const NAME: &'static str = "reaction.test";
    type Params = EventId;
    type Result = Vec<ReactionTrial>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventId {
    pub event: i64,
}

/// One rule against one event: whether it fires, and why not when it does not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionTrial {
    pub reaction: String,
    pub scope: String,
    pub fires: bool,
    pub why: String,
    /// What it would run, or the question it would ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

// ---- agent.* / config.* ------------------------------------------------------

/// An agent CLI the registry knows, and whether it is on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    /// The binaries the profile names.
    pub bins: Vec<String>,
    /// The first of them found on `PATH`, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<PathBuf>,
    /// Arguments that make it run one prompt and exit.
    #[serde(default)]
    pub headless: Vec<String>,
    /// How to name a model, if the profile says.
    #[serde(default)]
    pub model_flag: Vec<String>,
    /// The quick, cheap model the profile suggests for summaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_model: Option<String>,
    /// Whether the panel offers it: named in the config's `agents`, or that
    /// list is empty.
    #[serde(default = "yes")]
    pub offered: bool,
}

fn yes() -> bool {
    true
}

impl AgentInfo {
    pub fn available(&self) -> bool {
        self.bin.is_some()
    }
}

/// Every agent the registry knows, with what is available: detected, not
/// configured.
pub struct AgentList;
impl Method for AgentList {
    const NAME: &'static str = "agent.list";
    type Params = ();
    type Result = Vec<AgentInfo>;
}

/// An assistant as it would be invoked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAssistant {
    pub agent: String,
    /// `config` when the file names it, `default` for the first available agent.
    pub source: String,
    /// The binary, headless arguments, the model flag and the extra
    /// arguments, in that order; the prompt goes last.
    pub argv: Vec<String>,
    pub available: bool,
}

/// `config.yaml` as it stands, re-read from disk, with each assistant resolved
/// against the registry and what is on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigReport {
    pub file: PathBuf,
    pub present: bool,
    pub config: crate::config::Config,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpret: Option<ResolvedAssistant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<ResolvedAssistant>,
    /// Why the file, or an assistant in it, cannot be used as written.
    #[serde(default)]
    pub errors: Vec<String>,
}

pub struct ConfigGet;
impl Method for ConfigGet {
    const NAME: &'static str = "config.get";
    type Params = ();
    type Result = ConfigReport;
}

// ---- import.* ----------------------------------------------------------------

/// A session Claude runs on its own (`claude --bg`, or open in a terminal),
/// as `claude agents --json` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeSession {
    /// The conversation: what `claude --resume` takes.
    pub session_id: String,
    /// A background session's short id (`claude stop <id>`); none when it
    /// is open in a terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub cwd: PathBuf,
    /// `background` or `interactive`.
    pub kind: String,
    /// `busy` or `idle`.
    pub status: String,
    /// A background session's `working` or `done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// The Relay run that carries it on, once moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Not yet mentioned when the panel started.
    #[serde(default)]
    pub new: bool,
}

impl ClaudeSession {
    /// Open in someone's terminal: that terminal has it, not us.
    pub fn in_a_terminal(&self) -> bool {
        self.id.is_none()
    }

    /// In the middle of a turn: moving it now would cut the turn short.
    pub fn working(&self) -> bool {
        self.state.as_deref() == Some("working") || (self.state.is_none() && self.status == "busy")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportListParams {
    /// Remember these as mentioned, so the next start's hint is only about
    /// sessions that are new since.
    #[serde(default)]
    pub mark: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    pub sessions: Vec<ClaudeSession>,
    /// Why there is no list: no `claude` here, or it would not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Claude's own sessions, and which of them Relay carries on already.
pub struct ImportList;
impl Method for ImportList {
    const NAME: &'static str = "import.list";
    type Params = ImportListParams;
    type Result = ImportReport;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportMoveParams {
    pub session_id: String,
    /// The environment the resumed session starts with: the caller's.
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// Moves a Claude session into Relay: Claude's background process is
/// stopped (the conversation stays), and `claude --resume` carries it on as
/// a Relay run in the same folder, which becomes a workspace.
pub struct ImportMove;
impl Method for ImportMove {
    const NAME: &'static str = "import.move";
    type Params = ImportMoveParams;
    type Result = crate::types::Run;
}

// ---- setup.* -----------------------------------------------------------------

/// The setup guide's steps, counted: a release that adds one raises it, and
/// the panel shows the guide again for what is new.
pub const SETUP_STEPS: u32 = 3;

/// Where setup stands, and what it starts from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupState {
    /// How many steps were gone through last time; 0 when never.
    pub seen: u32,
    pub config: ConfigReport,
    pub agents: Vec<AgentInfo>,
}

pub struct SetupGet;
impl Method for SetupGet {
    const NAME: &'static str = "setup.get";
    type Params = ();
    type Result = SetupState;
}

/// What setup settled. A field left out leaves that part of `config.yaml`
/// as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSave {
    /// Steps gone through, kept so the guide is not shown again for them.
    pub seen: u32,
    /// The `agents` the panel offers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<String>>,
    /// Who summarises and drafts: both `assistants` jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant: Option<crate::config::Assistant>,
}

pub struct SetupDone;
impl Method for SetupDone {
    const NAME: &'static str = "setup.save";
    type Params = SetupSave;
    type Result = ConfigReport;
}

// ---- gc.* ------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GcParams {
    /// A duration; the config's `retention.journal` when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_than: Option<String>,
    /// Only count.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReport {
    /// The duration used.
    pub older_than: String,
    /// `asked` or `config`.
    pub from: String,
    pub dry_run: bool,
    /// Runs whose journal went (or would).
    pub runs: usize,
    /// Journal bytes and file bytes, together.
    pub bytes: u64,
    /// Log and sidecar files removed (or that would be).
    pub files: usize,
}

/// Drops the journal of runs ended longer ago than retention says.
pub struct GcRun;
impl Method for GcRun {
    const NAME: &'static str = "gc.run";
    type Params = GcParams;
    type Result = GcReport;
}

// ---- assistant.* -------------------------------------------------------------

/// A job for `assistants.<job>`: run headless with `prompt` as its last
/// argument, waited for, its output handed back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantParams {
    /// `interpret` or `summary`.
    pub job: String,
    pub prompt: String,
    pub cwd: PathBuf,
    /// The run's title in the list.
    pub title: String,
    /// A duration; a minute when left out. Past it the run is stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantOutput {
    /// The run that answered; in the list and the ledger like any other.
    pub run: String,
    pub agent: String,
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
}

pub struct AssistantRun;
impl Method for AssistantRun {
    const NAME: &'static str = "assistant.run";
    type Params = AssistantParams;
    type Result = AssistantOutput;
}

// ---- summary.* --------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryParams {
    pub target: SummaryTarget,
    /// Read the logs, not the recaps.
    #[serde(default)]
    pub deep: bool,
    /// Where the stretch begins. None: since you last looked (a run's
    /// seen_at, when it has done anything since, else the whole run; a job's
    /// oldest unseen firing on, else the last day); required for a project and
    /// all — the client knows when you were last here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Timestamp>,
    /// Make a new one even when the one kept is up to date.
    #[serde(default)]
    pub again: bool,
    /// Only say how many assistant calls it would take (`Summary.calls`, text
    /// empty).
    #[serde(default)]
    pub dry: bool,
    /// A duration; a minute when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

/// What happened in a run, a job's runs, a project's or everything's, over
/// the stretch from `from`, brief or deep: the one kept while it has not moved
/// on (unless `again`); else a run's own recap where it has one, or
/// `assistants.summary` reading the stretch. Refused for an assistant's own
/// run.
pub struct SummaryMake;
impl Method for SummaryMake {
    const NAME: &'static str = "summary.make";
    type Params = SummaryParams;
    type Result = crate::types::Summary;
}

/// The summary kept for a target, if any. Never asks. Of the params, the
/// target, `deep` and `from` say which; the rest are ignored.
pub struct SummaryGet;
impl Method for SummaryGet {
    const NAME: &'static str = "summary.get";
    type Params = SummaryParams;
    type Result = Option<crate::types::Summary>;
}
