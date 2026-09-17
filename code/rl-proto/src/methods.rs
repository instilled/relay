//! Typed method definitions. One struct per method; the name, params and
//! result live together so client and server cannot drift.

use crate::decision::{Decision, DecisionOption};
use crate::types::{AttachMode, Checkout, Event, Mark, Run, RunKind, RunState, Signal};
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
/// nothing writes the title afterwards, so a rename is not overwritten.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspacePath {
    pub path: PathBuf,
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

/// Unregisters a folder; its schedules stop.
pub struct WorkspaceRemove;
impl Method for WorkspaceRemove {
    const NAME: &'static str = "workspace.remove";
    type Params = WorkspacePath;
    type Result = bool;
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
    pub summarise: Option<ResolvedAssistant>,
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
    /// `interpret` or `summarise`.
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SummaryParams {
    pub run: String,
    /// Only this stretch, back from now: a duration (`2h`). None, the whole run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// Make a new one even when the one kept is up to date.
    #[serde(default)]
    pub again: bool,
    /// A duration; a minute when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

/// A recap of a run, live or ended, over `since` or the whole of it: the one
/// kept while the run has not moved on (unless `again`); else the agent's own
/// recap from its transcript (Claude), its turns as facts (Codex), or
/// `assistants.summarise` reading the stretch. Refused for an assistant's own
/// run.
pub struct SummaryMake;
impl Method for SummaryMake {
    const NAME: &'static str = "summary.make";
    type Params = SummaryParams;
    type Result = crate::types::Summary;
}

/// The summary kept for a run, if any. Never asks.
pub struct SummaryGet;
impl Method for SummaryGet {
    const NAME: &'static str = "summary.get";
    type Params = RunId;
    type Result = Option<crate::types::Summary>;
}
