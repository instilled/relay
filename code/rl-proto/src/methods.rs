//! Typed method definitions. One struct per method; the name, params and
//! result live together so client and server cannot drift.

use crate::decision::{Decision, DecisionOption};
use crate::flow::Firing;
use crate::types::{Checkout, Event, Mark, Run, RunKind, RunState, Signal, SummaryTarget};
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
    /// Only runs with this cause, e.g. `flow:nightly/probe`.
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
    pub chain: Option<String>,
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
/// `stopped`. Returns the run as it is right after the signal is sent. With
/// `archive` it is also archived once it has ended — killed if it has not
/// within a grace — so stopping it and taking it off the list is one ask.
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
    #[serde(default)]
    pub archive: bool,
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

/// Post (or refresh) a lead on the report, from a flow, command or plugin.
/// Keyed by `(source, key)`; the source `relay` is reserved for built-ins.
pub struct LeadPost;
impl Method for LeadPost {
    const NAME: &'static str = "lead.post";
    type Params = LeadPostParams;
    type Result = ();
}

/// Tag a source (replacing any earlier tag); the report groups leads by it.
pub struct TagSet;
impl Method for TagSet {
    const NAME: &'static str = "tag.set";
    type Params = SourceTag;
    type Result = ();
}

/// Remove a source's tag.
pub struct TagClear;
impl Method for TagClear {
    const NAME: &'static str = "tag.clear";
    type Params = TagClearParams;
    type Result = ();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagClearParams {
    pub source: String,
}

/// Withdraw a lead the caller posted earlier.
pub struct LeadClear;
impl Method for LeadClear {
    const NAME: &'static str = "lead.clear";
    type Params = LeadClearParams;
    type Result = ();
}

/// Hide a lead from the report; a re-post by its source will not bring it back.
pub struct LeadDismiss;
impl Method for LeadDismiss {
    const NAME: &'static str = "lead.dismiss";
    type Params = LeadDismissParams;
    type Result = ();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeadDismissParams {
    pub source: String,
    pub key: String,
}

/// You looked at these runs: their items leave the queue, and the next
/// stretch of each begins now, from its repo's HEAD.
pub struct QueueReview;
impl Method for QueueReview {
    const NAME: &'static str = "queue.review";
    type Params = QueueReviewParams;
    type Result = QueueReviewed;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueReviewParams {
    pub runs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueReviewed {
    /// How many were queued.
    pub cleared: u32,
}

/// One run read since you last looked: its log and what changed in git.
pub struct QueueItemGet;
impl Method for QueueItemGet {
    const NAME: &'static str = "queue.item";
    type Params = QueueItemParams;
    type Result = crate::queue::ItemView;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItemParams {
    pub run: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeadPostParams {
    pub source: String,
    pub key: String,
    /// `needs_you`, `live` or `happened` (parsed by the daemon).
    pub band: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeadClearParams {
    pub source: String,
    pub key: String,
}

/// Emit a custom event into the ledger, from a command or plugin. A flow with
/// `on:` picks it up like any built-in event. Inside a run, `RELAY_RUN` attributes
/// it to that run's workspace and chain; outside one, the caller names the
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
    /// Arbitrary structured payload, reachable in a flow as `{event.data.<key>}`.
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
    /// Which CLI's vocabulary `event` and `payload` use: `claude`, `codex`
    /// or `gemini`.
    #[serde(default = "default_dialect")]
    pub dialect: String,
    pub event: String,
    #[serde(default)]
    pub payload: Value,
}

fn default_dialect() -> String {
    "claude".into()
}

/// One of an agent's own sessions, as its user-level hook told it (`rl hook
/// <agent> <Event>` outside any Relay run of that agent's): kept with the
/// rest of the agents' own sessions, keyed `o:<agent>:<session_id>`.
pub struct OutsideHook;
impl Method for OutsideHook {
    const NAME: &'static str = "outside.hook";
    type Params = OutsideHookParams;
    type Result = OutsideHookResult;
}

/// What `rld` answers a hook with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OutsideHookResult {
    /// Whether `rld` knows no process for that session: the hook then finds
    /// the agent's and says it once more, with the pid. An older `rld`
    /// answers with nothing, which reads as false.
    pub wants_pid: bool,
}

impl<'de> Deserialize<'de> for OutsideHookResult {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Said {
            #[serde(default)]
            wants_pid: bool,
        }
        let said = Option::<Said>::deserialize(d)?;
        Ok(Self {
            wants_pid: said.is_some_and(|s| s.wants_pid),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutsideHookParams {
    /// The profile whose hook it is: `claude`, `codex`, `gemini`.
    pub agent: String,
    pub session_id: String,
    pub cwd: PathBuf,
    /// The hook event, in the agent's own words (`PermissionRequest`,
    /// `Notification`, …).
    pub event: String,
    /// What narrows the event: a `Notification`'s type, a permission's
    /// tool, a start's source, an end's reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The agent's process, where the hook found it among its ancestors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// The conversation's file, as the payload names it: a working agent
    /// writes it all the time, so how recently it changed says more than
    /// the last word heard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<PathBuf>,
    /// The Relay run the hook was called under (`RELAY_RUN`), if any: `rld`
    /// lets the word go when that run is this agent's own, whose hooks have
    /// said it already, and keeps it when the agent was only started inside
    /// it (`user_hooks::foreign`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
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
/// `Resize` from the joiner. What comes first
/// depends on the run: a terminal run's screen, drawn afresh by its holder; a
/// pipes run's journal tail, then the holder's replay from exactly where the
/// journal stops. Then live output. Closing the connection detaches. A live
/// run has one viewer, who is its joiner: a second attach is refused unless
/// `take` is set, which detaches the first. An ended run sends its
/// journal's tail, drawn as a screen when it was a terminal run, and `Exit`
/// straight away.
pub struct RunAttach;
impl Method for RunAttach {
    const NAME: &'static str = "run.attach";
    type Params = AttachParams;
    type Result = AttachInfo;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachParams {
    pub id: String,
    /// Who is looking (`user@host`); shown to other viewers, recorded as presence.
    pub client: String,
    /// Bytes of journaled output to draw from for an ended run or a pipes
    /// run; a live terminal run is drawn from the holder's own screen and
    /// history. Default 256 KiB.
    #[serde(default = "default_scrollback")]
    pub scrollback: u64,
    /// Join even if someone else has: they are detached.
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
    /// The pty's size as the holder has it; for an ended run, the last size
    /// the ledger recorded (0×0 if it has none).
    pub cols: u16,
    pub rows: u16,
    pub tty: bool,
    /// Whether a holder is attached; `false` means only the journal follows.
    pub live: bool,
    /// The current joiner, if any (possibly the caller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joiner: Option<String>,
    /// The journal's length in output bytes: where a pipes run's byte
    /// replay picks up.
    pub journaled: u64,
    /// Where the holder's replay starts; more than `journaled` means a gap.
    /// A screen is whole by itself: for one, this is `journaled`.
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

// ---- report.* ---------------------------------------------------------------

pub struct ReportGet;
impl Method for ReportGet {
    const NAME: &'static str = "report.get";
    type Params = ReportParams;
    type Result = Report;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
}

/// The report: leads in three bands. needs-you is pinned; happened is bounded by `since`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub needs_you: Vec<crate::lead::Lead>,
    pub live: Vec<crate::lead::Lead>,
    pub happened: Vec<crate::lead::Lead>,
    /// The source -> tag overlay the TUI groups leads by.
    #[serde(default)]
    pub tags: Vec<SourceTag>,
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
    /// Set by the panel, which keeps the list showing: only then does `rld`
    /// ask `claude agents`. A one-off digest leaves it false and reads what
    /// is kept.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub watching: bool,
}

/// Takes a whole project, or a flow, off the panel's list — or brings it back
/// — by its row's key: `g:<folder>` a project's header (`g:everywhere` the
/// flows' runs outside every project), `f:<name>` a flow,
/// `o:<agent>:<session>` an agent's own session. Everything under it is
/// hidden with it, runs that come later too; nothing is stopped or removed.
/// Answers with everything hidden so.
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

/// A project, a flow or an agent's own session taken off the list,
/// and since when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hidden {
    pub key: String,
    pub at: Timestamp,
}

impl Hidden {
    /// Whether `key` names something `digest.hide` can hide.
    pub fn valid_key(key: &str) -> bool {
        key.strip_prefix("g:")
            .or_else(|| key.strip_prefix("f:"))
            .is_some_and(|k| !k.is_empty())
            || Outside::valid_key(key)
    }
}

/// Keeps a project's rows in the order given — moved by hand on the panel
/// (shift-↑/↓) — or, with no rows, lets them go back to the order they came
/// in (`/sort`). The project is its header's key (`g:<folder>`,
/// `g:everywhere`); the rows are run (`r:<id>`) and flow (`f:<name>`) keys,
/// first on top. Under `Order::PROJECTS` it keeps the projects' own order
/// instead, by their headers' keys (`g:<folder>`; `everywhere` stays last).
/// The order given replaces the one kept, so what has left the list drops
/// out. Answers with every project's order.
pub struct DigestOrder;
impl Method for DigestOrder {
    const NAME: &'static str = "digest.order";
    type Params = Order;
    type Result = Vec<Order>;
}

/// A lead source tagged with a co-worker or team name. Every lead from the
/// source inherits the tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceTag {
    pub source: String,
    pub tag: String,
}

/// A lead the user dismissed from the report, by identity. Hides built-in
/// (computed) leads as well as stored ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dismissal {
    pub source: String,
    pub key: String,
}

/// A project's rows in the order you put them, first on top — or, under
/// `Order::PROJECTS`, the projects themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    /// The project's header key: `g:<folder>`, or `g:everywhere`; or
    /// `Order::PROJECTS`, for the order of the projects.
    pub group: String,
    /// Run, flow and agent session keys, `r:<id>`, `f:<name>` and
    /// `o:<agent>:<session id>`; under `Order::PROJECTS`, the projects'
    /// header keys, `g:<folder>`.
    #[serde(default)]
    pub rows: Vec<String>,
}

impl Order {
    /// The order the projects stand in on the list: not a project of its
    /// own, so nothing a header's key could be.
    pub const PROJECTS: &'static str = "projects";

    /// Whether `key` names what an order can be kept for: a project, or the
    /// projects themselves.
    pub fn valid_group(key: &str) -> bool {
        key == Self::PROJECTS || Self::valid_project(key)
    }

    /// Whether `key` names a project, by its header's key.
    pub fn valid_project(key: &str) -> bool {
        key.strip_prefix("g:").is_some_and(|k| !k.is_empty())
    }

    /// Whether `key` names a row that can be put in order: a run, a flow,
    /// or an agent's own session.
    pub fn valid_row(key: &str) -> bool {
        key.strip_prefix("r:")
            .or_else(|| key.strip_prefix("f:"))
            .is_some_and(|k| !k.is_empty())
            || Outside::valid_key(key)
    }

    /// Whether `key` can stand in this order: a run, a flow or an agent's
    /// own session in a project's, a project in the projects'.
    pub fn fits(&self, key: &str) -> bool {
        if self.group == Self::PROJECTS {
            Self::valid_project(key)
        } else {
            Self::valid_row(key)
        }
    }
}

#[cfg(test)]
mod order_tests {
    use super::Order;

    #[test]
    fn the_projects_keep_an_order_of_their_own_by_their_headers() {
        assert!(Order::valid_group("g:/home/f/relay"));
        assert!(Order::valid_group(Order::PROJECTS));
        assert!(!Order::valid_group("g:"));
        assert!(!Order::valid_group("r:a1"));
        let projects = Order {
            group: Order::PROJECTS.into(),
            rows: vec![],
        };
        assert!(projects.fits("g:/home/f/relay"));
        assert!(!projects.fits("r:a1"));
        let relay = Order {
            group: "g:/home/f/relay".into(),
            rows: vec![],
        };
        assert!(relay.fits("r:a1") && relay.fits("f:nightly") && relay.fits("o:codex:s1"));
        assert!(!relay.fits("g:/home/f/api"));
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
    /// The projects, flows and agents' own sessions taken off the list (`digest.hide`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<Hidden>,
    /// The projects whose rows you put in order (`digest.order`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<Order>,
    /// Each flow's latest firing: where its row says it stands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub firings: Vec<Firing>,
    /// Agents' own sessions as last heard — Claude's as `claude agents
    /// --json` last listed them — less those that are Relay's runs. Filled
    /// only while a panel keeps asking: the first digest after a quiet
    /// minute has none, and empty then does not mean there are none. `rl
    /// since` does not show them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outside: Vec<Outside>,
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

/// When a client last looked, without saying it looks now: for what reads
/// the report without being a look at it (`rl report --json`).
pub struct PresenceGet;
impl Method for PresenceGet {
    const NAME: &'static str = "presence.get";
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

// ---- workspace.* ------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceUpParams {
    pub path: PathBuf,
}

/// Registers a folder, so flows can run in it (`in:`); registered already,
/// it is as it was.
pub struct WorkspaceUp;
impl Method for WorkspaceUp {
    const NAME: &'static str = "workspace.up";
    type Params = WorkspaceUpParams;
    type Result = crate::types::Workspace;
}

pub struct WorkspaceList;
impl Method for WorkspaceList {
    const NAME: &'static str = "workspace.list";
    type Params = ();
    type Result = Vec<crate::types::Workspace>;
}

/// Unregisters a folder: its open questions are dismissed, and flows `in` it
/// no longer load. The folder need not exist any more, nor have been
/// registered — a folder the list shows only for its runs is removed by what
/// the flags do to those. Asked to, it also stops every run still going
/// there, and archives what has ended.
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
    type Result = crate::types::Workspace;
}

// ---- decision.* -------------------------------------------------------------

/// Raises a question and returns it. With a `group`, an open decision with
/// that key is replaced in place — the same id, the new wording — so a
/// flow that asks nightly leaves one line, not thirty.
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
    pub chain: Option<String>,
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
/// (`cause = decision:<id>`, the decision's chain); the ids come back on the
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
    /// `catch_up.after` as it resolves: a duration, or `off`.
    #[serde(default)]
    pub catch_up_after: String,
    /// `catch_up.summary` as it resolves: `auto` or `ask`.
    #[serde(default)]
    pub catch_up_summary: String,
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

/// A session an agent CLI runs on its own, not a Relay run: one of Claude's
/// (`claude --bg`, or open in a terminal, as `claude agents --json` lists
/// it), or Codex's and Gemini's, as their hooks tell. The import screen
/// moves Claude's background ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outside {
    /// The agent profile whose session it is: `claude`, `codex`, `gemini`.
    /// Claude's where not said, as every one was before there were others.
    #[serde(default = "claude_agent")]
    pub agent: String,
    /// The conversation: what the agent's resume takes (`claude --resume`).
    pub session_id: String,
    /// A background session's short id (`claude stop <id>`, `claude attach
    /// <id>`): Claude's only, and none when it is open in a terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub cwd: PathBuf,
    /// `background` or `interactive`; empty when Claude did not say. Only
    /// what to call it: whether it can be attached to is `id`'s to say.
    pub kind: String,
    /// `busy`, `waiting` or `idle` while its process lives; empty when it
    /// does not. Kept as Claude writes it.
    pub status: String,
    /// A background session's `working`, `blocked`, `done`, `failed` or
    /// `stopped`. Kept as Claude writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Its process, while that lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    /// What it waits on while `status` is `waiting`: `permission prompt`,
    /// `input needed`, and the like.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<String>,
    /// The Relay run that carries it on, once moved. On the list, only one
    /// left open in its terminal and at work there again has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Not yet mentioned when the panel started.
    #[serde(default)]
    pub new: bool,
    /// The project folder it stands under, as a run started in its `cwd`
    /// would: the registered folder containing it, else its checkout's
    /// root, else its folder. Settled by `rld` once per session; none where
    /// it was not, and the panel guesses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// When `rld` first saw it stand as it does now: what brings a hidden
    /// one back once it comes to need you.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_at: Option<Timestamp>,
    /// The ended Relay run it went on from: one sent to Claude's background
    /// (`/background`), which carries the run's session id on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<WentFrom>,
}

/// The Relay run a background session went on from: its id and its title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WentFrom {
    pub run: String,
    pub title: String,
}

fn claude_agent() -> String {
    "claude".into()
}

impl Outside {
    /// Its row's key: `o:<agent>:<session id>`, what `/hide` and an order
    /// keep.
    pub fn key(&self) -> String {
        format!("o:{}:{}", self.agent, self.session_id)
    }

    /// What it is called: its name, or — where its agent gave it none —
    /// the start of its session id.
    pub fn title(&self) -> String {
        if self.name.trim().is_empty() {
            format!("session {}", self.session_id.chars().take(8).collect::<String>())
        } else {
            self.name.clone()
        }
    }

    /// Whether `key` is such a key, with an agent and a session.
    pub fn valid_key(key: &str) -> bool {
        key.strip_prefix("o:")
            .and_then(|k| k.split_once(':'))
            .is_some_and(|(agent, session)| !agent.is_empty() && !session.is_empty())
    }

    /// Whether it wants you, as a run would that asks, is blocked or failed:
    /// a `blocked` or `failed` state; or, where its state is none Relay
    /// knows, a `waiting` status. What brings a hidden one back, and what
    /// its `state_at` marks the start of — a change within it (another
    /// `waitingFor`, say) is not news.
    pub fn wants_you(&self) -> bool {
        matches!(
            self.run_state(),
            RunState::Blocked | RunState::Failed | RunState::NeedsInput
        )
    }

    /// Its state as a run's would be: by a background session's `state`
    /// where it is one Relay knows, else by `status` — `busy` working,
    /// `waiting` needing you, anything else idle. The one place these words
    /// are read: the list's state and `wants_you` both come from here.
    pub fn run_state(&self) -> RunState {
        match self.state.as_deref() {
            Some("working") => RunState::Working,
            Some("blocked") => RunState::Blocked,
            Some("done") => RunState::Finished,
            Some("failed") => RunState::Failed,
            Some("stopped") => RunState::Stopped,
            _ => match self.status.as_str() {
                Self::BUSY => RunState::Working,
                Self::WAITING => RunState::NeedsInput,
                _ => RunState::Idle,
            },
        }
    }

    /// The `status` words, as Claude writes them; a session a hook told of
    /// is given the same.
    pub const BUSY: &str = "busy";
    pub const WAITING: &str = "waiting";
    pub const IDLE: &str = "idle";

    /// Open in someone's terminal: that terminal has it, not us.
    pub fn in_a_terminal(&self) -> bool {
        self.id.is_none()
    }

    /// In the middle of a turn: moving it now would cut the turn short.
    pub fn working(&self) -> bool {
        self.run_state() == RunState::Working
    }

    /// At work, or waiting on you mid-turn: one in a terminal so is not
    /// carried on now, and one carried on while left open there shows again.
    pub fn at_work(&self) -> bool {
        matches!(
            self.run_state(),
            RunState::Working | RunState::NeedsInput | RunState::Blocked
        )
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
    pub sessions: Vec<Outside>,
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
    /// Whose session it is: `claude`, `codex`, `gemini`. Claude's where
    /// not said, as every one was before there were others.
    #[serde(default = "claude_agent")]
    pub agent: String,
    pub session_id: String,
    /// The environment the resumed session starts with: the caller's.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// The terminal it starts on, where the caller knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
}

/// Carries one of an agent's own sessions on as a Relay run, in the same
/// folder, which becomes a workspace, as the agent's profile resumes it
/// (`claude --resume`, `codex resume`). A Claude background session is
/// stopped there first (the conversation stays); one open in a terminal is
/// carried on only while idle, and that terminal keeps its own.
pub struct ImportMove;
impl Method for ImportMove {
    const NAME: &'static str = "import.move";
    type Params = ImportMoveParams;
    type Result = crate::types::Run;
}

// ---- setup.* -----------------------------------------------------------------

/// The setup guide's steps, counted: a release that adds one raises it, and
/// the panel shows the guide again for what is new.
pub const SETUP_STEPS: u32 = 4;

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
    /// Firings' folders (`flows/<name>/runs/<firing>/`) removed (or that would be).
    #[serde(default)]
    pub firings: usize,
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
    /// seen_at, when it has done anything since, else the whole run; a flow's
    /// oldest unseen firing on, else the last day); for a project and all, `make`
    /// needs it (the client knows when you were last here), and `get` without it gives the newest kept.
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

/// What happened in a run, a flow's runs, a project's or everything's, over
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

#[cfg(test)]
mod tests {
    #[test]
    fn lead_params_round_trip() {
        use super::LeadPostParams;
        let p = LeadPostParams {
            source: "feedback".into(),
            key: "triage".into(),
            band: "needs_you".into(),
            title: "5 to triage".into(),
            summary: None,
            scope: None,
            action: Some("claude -p 'triage'".into()),
            ttl_secs: Some(1800),
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: LeadPostParams = serde_json::from_str(&s).unwrap();
        assert_eq!(p.source, back.source);
        assert_eq!(back.ttl_secs, Some(1800));
    }

    #[test]
    fn a_hook_answer_of_nothing_or_without_the_word_wants_no_pid() {
        use super::OutsideHookResult;
        let read = |v: serde_json::Value| serde_json::from_value::<OutsideHookResult>(v).unwrap().wants_pid;
        assert!(!read(serde_json::json!(null)));
        assert!(!read(serde_json::json!({})));
        assert!(read(serde_json::json!({ "wants_pid": true })));
    }

    use super::*;

    #[test]
    fn an_agents_own_session_is_keyed_by_its_agent_and_session() {
        for ok in ["o:claude:s1", "o:codex:019a-77"] {
            assert!(Hidden::valid_key(ok) && Order::valid_row(ok), "{ok}");
        }
        for bad in ["c:s1", "o:claude:", "o::s1", "o:claude", "o:"] {
            assert!(!Hidden::valid_key(bad) && !Order::valid_row(bad), "{bad}");
        }
        assert!(Hidden::valid_key("g:/w") && !Hidden::valid_key("r:x"));
        assert!(Order::valid_row("r:x") && !Order::valid_row("g:/w"));
    }

    #[test]
    fn a_session_with_no_agent_said_is_claudes() {
        let s: Outside =
            serde_json::from_str(r#"{"session_id":"s","name":"n","cwd":"/w","kind":"interactive","status":"idle"}"#)
                .unwrap();
        assert_eq!((s.agent.as_str(), s.key()), ("claude", "o:claude:s".to_string()));
        assert_eq!((s.workspace, s.state_at), (None, None));
    }

    #[test]
    fn its_state_words_are_read_in_one_place() {
        let base: Outside =
            serde_json::from_str(r#"{"session_id":"s","name":"n","cwd":"/w","kind":"interactive","status":"idle"}"#)
                .unwrap();
        let at = |state: Option<&str>, status: &str| Outside {
            state: state.map(Into::into),
            status: status.into(),
            ..base.clone()
        };
        for (s, working, at_work) in [
            (at(None, Outside::BUSY), true, true),
            (at(None, Outside::WAITING), false, true),
            (at(None, Outside::IDLE), false, false),
            (at(Some("working"), Outside::IDLE), true, true),
            (at(Some("blocked"), Outside::WAITING), false, true),
            (at(Some("done"), Outside::BUSY), false, false),
            // A state not known yet goes by its status, for both.
            (at(Some("pondering"), Outside::BUSY), true, true),
        ] {
            assert_eq!((s.working(), s.at_work()), (working, at_work), "{s:?}");
            assert_eq!(s.working(), s.run_state() == RunState::Working, "{s:?}");
        }
    }
}
