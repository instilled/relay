//! The list's rows, from its three sources: sessions Relay holds
//! (`sessions/`), agents' own sessions their hooks told of (`state/`), and
//! the harnesses' own background sessions (`Harness::list`). One `Row` each;
//! the list never asks which source a row came from.

use crate::rows::{registry, state_words};
use crate::store::sessions::{Held, Status};
use crate::store::state::{HookState, Said};
use jiff::Timestamp;
use rl_core::state::{Confidence, RunKind, RunState};
#[cfg(not(test))]
use rl_detect::registry::find_bin;
use rl_session::exit::Exit;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;

/// Where a row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Held,
    Hooked,
    Native,
}

/// What enter does with a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attach {
    /// Attach to the Relay-held session of this name.
    Held(String),
    /// Run this, in the terminal, to attach to the harness's own session.
    Argv(Vec<String>),
    /// Run this to carry the conversation on, then attach: in the
    /// harness's own background (`background`), else held by Relay.
    CarryOn {
        argv: Vec<String>,
        background: bool,
    },
    None,
}

pub use rl_detect::harness::{CarryOn, Choice, Harness, NativeRow, NativeState, Pr};

/// What lists a harness's own background sessions. Asked from a background
/// task while the list is open, so `list` may block (it runs the harness's
/// CLI); an error keeps what it said last.
pub trait NativeLister: Send + Sync {
    /// The harness whose sessions it lists: `claude`.
    fn harness(&self) -> &str;
    fn list(&self) -> anyhow::Result<Vec<NativeRow>>;
    /// The same, for no longer than `wait`.
    fn list_within(&self, wait: std::time::Duration) -> anyhow::Result<Vec<NativeRow>> {
        let _ = wait;
        self.list()
    }
}

/// A harness, as a lister.
struct Listing(&'static dyn Harness);

impl NativeLister for Listing {
    fn harness(&self) -> &str {
        self.0.name()
    }

    fn list(&self) -> anyhow::Result<Vec<NativeRow>> {
        self.0.list()
    }

    fn list_within(&self, wait: std::time::Duration) -> anyhow::Result<Vec<NativeRow>> {
        self.0.list_within(wait)
    }
}

/// Every harness the profiles name, found on this machine or not. Made
/// once.
#[cfg(not(test))]
pub fn harnesses() -> &'static [Box<dyn Harness>] {
    static ALL: OnceLock<Vec<Box<dyn Harness>>> = OnceLock::new();
    ALL.get_or_init(|| rl_detect::harness::harnesses(registry(), find_bin))
}

/// The tests never run a real harness, but a stand-in: Codex's daemon a
/// copy of `fixtures/stand-in-codex-daemon` (`codex_stand_in`), every other
/// `fixtures/stand-in-harness`, and the listing of each with a background
/// is theirs (`StandIn`).
#[cfg(test)]
pub fn harnesses() -> &'static [Box<dyn Harness>] {
    static ALL: OnceLock<Vec<Box<dyn Harness>>> = OnceLock::new();
    ALL.get_or_init(|| {
        let bin = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-harness"));
        let codex = codex_stand_in();
        let registry = registry();
        rl_detect::harness::harnesses(registry, |bins| {
            Some(if bins.iter().any(|b| b == "codex") {
                codex.clone()
            } else {
                bin.clone()
            })
        })
        .into_iter()
        .map(|h| {
            if !h.lists() {
                return h;
            }
            let claude = registry
                .profile(h.name())
                .filter(|p| p.name == "claude")
                .and_then(|p| rl_detect::claude_native::Claude::of(p, bin.clone()));
            Box::new(StandIn { inner: h, claude }) as Box<dyn Harness>
        })
        .collect()
    })
}

/// Where the tests' stand-ins are copied, beside the test binary in the
/// target folder: one folder per test run, by its pid.
#[cfg(test)]
const STAND_INS: &str = "stand-ins";

/// The copy of `fixtures/stand-in-codex-daemon` the tests' Codex runs, in a
/// folder of this test run's own (`<target>/debug/deps/stand-ins/<pid>`),
/// where it keeps what it was sent. Made fresh by each run, which first
/// sweeps away the folders of runs that have finished: the target folder
/// holds at most one per run going on.
#[cfg(test)]
pub(crate) fn codex_stand_in() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let base = exe.parent().unwrap().join(STAND_INS);
        sweep_stand_ins(&base);
        let dir = base.join(std::process::id().to_string());
        // A pid used again: what an earlier run left is not this one's.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("codex");
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-codex-daemon"),
            &bin,
        )
        .unwrap();
        bin
    })
    .clone()
}

/// Takes away the folders under `base` of test runs that have finished: a
/// folder named by a pid no process has.
#[cfg(test)]
fn sweep_stand_ins(base: &std::path::Path) {
    for e in std::fs::read_dir(base).into_iter().flatten().flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else {
            continue;
        };
        let gone = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) == Err(nix::errno::Errno::ESRCH);
        if gone {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// A harness with a background run on the stand-in: what Claude starts in
/// its background (`0a1b2c3d`, in the folder asked) is listed as working
/// there from then on, by every such harness's listing. All else is the
/// harness's own.
#[cfg(test)]
struct StandIn {
    inner: Box<dyn Harness>,
    /// Claude's harness, where it is Claude's: what it starts with.
    claude: Option<rl_detect::claude_native::Claude>,
}

#[cfg(test)]
static STARTED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
impl StandIn {
    /// Runs `argv` in Claude's background, as Claude's harness does, its
    /// listing this one's.
    fn begin(&self, argv: &[String], cwd: &std::path::Path, before: &HashSet<String>) -> anyhow::Result<String> {
        STARTED.lock().unwrap().push(cwd.to_path_buf());
        rl_detect::claude_native::begin(
            |w| self.list_within(w),
            argv,
            cwd,
            before,
            rl_detect::claude_native::LISTED_WAIT,
        )
    }
}

#[cfg(test)]
impl Harness for StandIn {
    fn profile(&self) -> &rl_detect::Profile {
        self.inner.profile()
    }

    fn bin(&self) -> &str {
        self.inner.bin()
    }

    fn found(&self) -> bool {
        self.inner.found()
    }

    fn lists(&self) -> bool {
        self.inner.lists()
    }

    fn list_within(&self, _: std::time::Duration) -> anyhow::Result<Vec<NativeRow>> {
        let started = STARTED.lock().unwrap();
        Ok(started
            .iter()
            .map(|cwd| NativeRow {
                harness: self.name().into(),
                id: "0a1b2c3d".into(),
                session_id: "s-0a1b2c3d".into(),
                name: "started".into(),
                cwd: cwd.clone(),
                state: NativeState {
                    words: "working".into(),
                    state: RunState::Working,
                },
                summary: None,
                started: None,
                ended: None,
                at: None,
                pr: None,
                color: None,
            })
            .collect())
    }

    fn start(
        &self,
        cwd: &std::path::Path,
        name: Option<&str>,
        prompt: Option<&str>,
        before: &HashSet<String>,
    ) -> anyhow::Result<rl_detect::harness::Started> {
        match self.claude.as_ref().and_then(|c| c.start_argv(name, prompt)) {
            Some(argv) => self.begin(&argv, cwd, before).map(rl_detect::harness::Started::Id),
            None => self.inner.start(cwd, name, prompt, before),
        }
    }

    fn never_held(&self) -> bool {
        self.inner.never_held()
    }

    fn held_argv(
        &self,
        home: &rl_core::Home,
        argv: Vec<String>,
        rl: &dyn Fn() -> anyhow::Result<PathBuf>,
    ) -> anyhow::Result<Vec<String>> {
        self.inner.held_argv(home, argv, rl)
    }

    fn attach_argv(&self, id: &str) -> Option<Vec<String>> {
        self.inner.attach_argv(id)
    }

    fn carry_on(&self, session_id: &str) -> Option<CarryOn> {
        self.inner.carry_on(session_id)
    }

    fn carry_on_there(
        &self,
        argv: &[String],
        cwd: &std::path::Path,
        before: &HashSet<String>,
    ) -> anyhow::Result<String> {
        self.begin(argv, cwd, before)
    }

    fn stops(&self, row: &NativeRow) -> bool {
        self.inner.stops(row)
    }

    fn stop(&self, id: &str, cwd: &std::path::Path) -> anyhow::Result<()> {
        self.inner.stop(id, cwd)
    }

    fn stop_choice(&self) -> Choice {
        self.inner.stop_choice()
    }

    fn close_choice(&self, row: &NativeRow) -> Option<Choice> {
        self.inner.close_choice(row)
    }

    fn close(&self, id: &str, cwd: &std::path::Path) -> anyhow::Result<()> {
        self.inner.close(id, cwd)
    }

    fn ping_argv(&self, id: &str, text: &str) -> Option<Vec<String>> {
        self.inner.ping_argv(id, text)
    }

    fn renames(&self, row: &NativeRow, name: &str) -> bool {
        self.inner.renames(row, name)
    }

    fn rename(&self, row: &NativeRow, name: &str, locks: &std::path::Path) -> anyhow::Result<()> {
        self.inner.rename(row, name, locks)
    }

    fn wake(&self, row: &NativeRow, line: &str, locks: &std::path::Path) -> Option<rl_detect::harness::Woke> {
        self.inner.wake(row, line, locks)
    }

    fn user_hooks(&self) -> Option<&rl_detect::sessions::UserHooks> {
        self.inner.user_hooks()
    }

    fn has_mcp(&self) -> bool {
        self.inner.has_mcp()
    }

    fn mcp(
        &self,
        env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    ) -> anyhow::Result<Option<rl_detect::mcp_entry::Place>> {
        self.inner.mcp(env)
    }

    fn caller_id_var(&self) -> Option<&'static str> {
        self.inner.caller_id_var()
    }

    fn mail_by_hooks(&self) -> bool {
        self.inner.mail_by_hooks()
    }

    fn wake_advice(&self, name: &str) -> Option<String> {
        self.inner.wake_advice(name)
    }
}

/// The harness of the profile `name`.
pub fn harness(name: &str) -> Option<&'static dyn Harness> {
    harnesses().iter().find(|h| h.name() == name).map(|h| h.as_ref())
}

/// The listers the list asks: the harnesses on this machine with a
/// background of their own.
pub fn native_listers() -> Vec<Box<dyn NativeLister>> {
    harnesses()
        .iter()
        .filter(|h| h.lists() && h.found())
        .map(|h| Box::new(Listing(h.as_ref())) as Box<dyn NativeLister>)
        .collect()
}

/// What the listers said when asked all at once (`list_all`): their rows,
/// in the listers' order, and the harnesses that answered.
#[derive(Debug, Default)]
pub struct Listed {
    pub rows: Vec<NativeRow>,
    pub answered: BTreeSet<String>,
}

/// Asks `listers` at once, each on a thread of its own, and only for
/// `wait`: one that fails or has not answered by then is `warn`ed of and
/// its sessions left out, the others' kept. A thread still listing is left
/// to finish on its own (its harness's listing is bounded too).
pub fn list_all(
    listers: impl IntoIterator<Item = std::sync::Arc<dyn NativeLister>>,
    wait: std::time::Duration,
    warn: &mut dyn FnMut(String),
) -> Listed {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut asked: Vec<String> = Vec::new();
    for (i, l) in listers.into_iter().enumerate() {
        asked.push(l.harness().to_string());
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send((i, l.list_within(wait)));
        });
    }
    drop(tx);
    let deadline = std::time::Instant::now() + wait;
    let mut said: Vec<Option<anyhow::Result<Vec<NativeRow>>>> = asked.iter().map(|_| None).collect();
    let mut heard = 0;
    while heard < asked.len() {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let Ok((i, s)) = rx.recv_timeout(left) else {
            break;
        };
        said[i] = Some(s);
        heard += 1;
    }
    let mut listed = Listed::default();
    for (harness, s) in asked.into_iter().zip(said) {
        match s {
            Some(Ok(rows)) => {
                listed.rows.extend(rows);
                listed.answered.insert(harness);
            }
            Some(Err(e)) => warn(format!("{harness} did not list its sessions: {e:#}")),
            None => warn(format!(
                "{harness} did not answer in {}s; its sessions are left out",
                wait.as_secs_f32()
            )),
        }
    }
    listed
}

/// One row on the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `r:<name>` held, `o:<agent>:<session id>` hooked, `n:<harness>:<id>` native.
    pub key: String,
    pub source: Source,
    pub harness: String,
    pub name: String,
    pub cwd: PathBuf,
    pub state: RunState,
    /// The row's state word.
    pub words: String,
    pub summary: Option<String>,
    pub started: Option<Timestamp>,
    pub ended: Option<Timestamp>,
    /// When it came to stand as it does: what says whether a hidden row has
    /// come to need you since.
    pub state_at: Option<Timestamp>,
    pub pr: Option<Pr>,
    /// The colour its harness draws it in, by name (`pink`); none for most.
    pub color: Option<String>,
    pub attach: Attach,
    /// The id its harness stops it by (`Harness::stop`), where it is a
    /// harness's own session its harness can stop as it stands.
    pub stop: Option<String>,
    /// How `^x` twice offers that stop (`s stop · keeps the conversation`),
    /// where there is one.
    pub stop_choice: Option<Choice>,
    /// What `^x` twice offers to close it for good, or off its harness's
    /// lists (`Harness::close_choice`): a harness's own session only.
    pub close: Option<Choice>,
    pub kind: RunKind,
    /// The root of the repository its folder is in, as read when the rows
    /// were (`git::toplevel`).
    pub repo: Option<PathBuf>,
}

/// Every row, held first, then hooked, then native. A session never stands
/// twice: a hooked one that a native row or a Relay-held session already
/// is, nor a native one Relay holds (a harness's own session a held one's
/// hooks have said is its conversation). `answered`: the harnesses that
/// have listed their sessions at least once — until then, a hooked row of one
/// may be among them, and is not carried on. Each folder's repository is
/// looked for once.
pub fn rows(held: &[Held], hooked: &[HookState], native: &[NativeRow], answered: &BTreeSet<String>) -> Vec<Row> {
    let held_ids: HashSet<&str> = held
        .iter()
        .filter_map(|h| h.hook.as_ref().map(|k| k.session_id.as_str()))
        .collect();
    let native: Vec<&NativeRow> = native
        .iter()
        .filter(|n| !held_ids.contains(n.session_id.as_str()))
        .collect();
    let taken: HashSet<&str> = held_ids
        .iter()
        .copied()
        .chain(native.iter().map(|n| n.session_id.as_str()))
        .collect();
    let mut rows: Vec<Row> = held
        .iter()
        .map(held_row)
        .chain(
            hooked
                .iter()
                .filter(|h| !taken.contains(h.session_id.as_str()))
                .map(|h| hooked_row(h, answered)),
        )
        .chain(native.into_iter().map(native_row))
        .collect();
    let mut repos: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    for r in &mut rows {
        r.repo = repos
            .entry(r.cwd.clone())
            .or_insert_with(|| crate::git::project(&r.cwd))
            .clone();
    }
    rows
}

/// How an ended session ended: stopped when a hang-up or kill took it (the
/// signal is named, or the shell under it died by one and said `128 + n`),
/// finished on 0, failed on any other code. The one place this is decided.
pub fn ended_state(e: &Exit) -> RunState {
    // SIGHUP, SIGINT, SIGKILL, SIGTERM: what stopping a session sends.
    let stopped_by_code = matches!(e.code, 129 | 130 | 137 | 143);
    if e.signal.is_some() || stopped_by_code {
        RunState::Stopped
    } else if e.code == 0 {
        RunState::Finished
    } else {
        RunState::Failed
    }
}

fn held_row(h: &Held) -> Row {
    let m = &h.meta;
    let (state, confidence, at) = match &h.status {
        Status::Live => match h.current() {
            Some(c) => (c.state, c.confidence, c.at),
            None => (RunState::Starting, Confidence::Exact, m.started),
        },
        Status::Ended(e) => (h.ended_state(e), Confidence::Exact, e.at),
        Status::Lost => (RunState::Stale, Confidence::Exact, m.started),
    };
    let live_shell =
        m.kind == RunKind::Shell && !state.is_terminal() && !matches!(state, RunState::NeedsInput | RunState::Blocked);
    let words = match h.status {
        Status::Lost => "lost".to_string(),
        _ if live_shell => String::new(),
        _ => state_words(state, confidence),
    };
    let summary = h
        .hook
        .as_ref()
        .and_then(|k| k.detail.clone())
        .or_else(|| h.state.as_ref().and_then(|s| s.detail.clone()))
        .or_else(|| h.state.as_ref().and_then(|s| s.last_line.clone()));
    Row {
        key: format!("r:{}", m.name),
        source: Source::Held,
        harness: m.harness.clone(),
        name: m.name.clone(),
        cwd: m.cwd.clone(),
        state,
        words,
        summary,
        started: Some(m.started),
        ended: match &h.status {
            Status::Ended(e) => Some(e.at),
            _ => None,
        },
        state_at: Some(at),
        pr: None,
        color: None,
        attach: Attach::Held(m.name.clone()),
        stop: None,
        stop_choice: None,
        close: None,
        kind: m.kind,
        repo: None,
    }
}

/// What carries on a conversation open in another terminal, as its harness
/// says (`Harness::carry_on`): in the harness's own background where it has
/// one, else a session Relay holds — or, for a harness Relay never holds
/// (Codex: the conversation is a thread on its daemon), its own attach. Only
/// one at rest — one busy or waiting there is still in use — and, in a
/// background, only once the harness has said what runs there (`answered`):
/// carried on while it already runs there, it would run twice.
fn carry_on(h: &HookState, answered: &BTreeSet<String>) -> Attach {
    if h.state != Said::Idle {
        return Attach::None;
    }
    match harness(&h.agent).and_then(|x| x.carry_on(&h.session_id)) {
        Some(CarryOn::Attach(argv)) => Attach::Argv(argv),
        Some(CarryOn::Background(_)) if !answered.contains(&h.agent) => Attach::None,
        Some(CarryOn::Background(argv)) => Attach::CarryOn { argv, background: true },
        Some(CarryOn::Held(argv)) => Attach::CarryOn {
            argv,
            background: false,
        },
        None => Attach::None,
    }
}

fn hooked_row(h: &HookState, answered: &BTreeSet<String>) -> Row {
    let state = match h.state {
        Said::Idle => RunState::Idle,
        Said::Busy => RunState::Working,
        Said::Waiting => RunState::NeedsInput,
    };
    Row {
        key: format!("o:{}:{}", h.agent, h.session_id),
        source: Source::Hooked,
        harness: h.agent.clone(),
        name: format!("session {}", h.session_id.chars().take(8).collect::<String>()),
        cwd: h.cwd.clone(),
        state,
        words: state_words(state, Confidence::Exact),
        summary: h.detail.clone(),
        started: Some(h.started),
        ended: None,
        state_at: Some(h.at),
        pr: None,
        color: None,
        attach: carry_on(h, answered),
        stop: None,
        stop_choice: None,
        close: None,
        kind: RunKind::Agent,
        repo: None,
    }
}

/// The id its harness knows a native row by: what its key says after
/// `n:<harness>:`. None for any other row.
pub fn native_id(r: &Row) -> Option<&str> {
    if r.source != Source::Native {
        return None;
    }
    r.key
        .strip_prefix("n:")?
        .strip_prefix(r.harness.as_str())?
        .strip_prefix(':')
}

fn native_row(n: &NativeRow) -> Row {
    Row {
        key: format!("n:{}:{}", n.harness, n.id),
        source: Source::Native,
        harness: n.harness.clone(),
        name: n.name.clone(),
        cwd: n.cwd.clone(),
        state: n.state.state,
        words: n.state.words.clone(),
        summary: n.summary.clone(),
        started: n.started,
        ended: n.ended,
        state_at: n.at,
        pr: n.pr.clone(),
        color: n.color.clone(),
        attach: harness(&n.harness)
            .and_then(|h| h.attach_argv(&n.id))
            .map_or(Attach::None, Attach::Argv),
        stop: harness(&n.harness).filter(|h| h.stops(n)).map(|_| n.id.clone()),
        stop_choice: harness(&n.harness).filter(|h| h.stops(n)).map(|h| h.stop_choice()),
        close: harness(&n.harness).and_then(|h| h.close_choice(n)),
        kind: RunKind::Agent,
        repo: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sessions::Meta;
    use rl_session::exit::Exit;
    use rl_session::state::StateFile;

    fn at(s: i64) -> Timestamp {
        Timestamp::from_second(s).unwrap()
    }

    /// The rows, every harness having answered.
    fn rows(held: &[Held], hooked: &[HookState], native: &[NativeRow]) -> Vec<Row> {
        let all = harnesses().iter().map(|h| h.name().to_string()).collect();
        super::rows(held, hooked, native, &all)
    }

    fn held(name: &str, kind: RunKind, status: Status) -> Held {
        Held {
            meta: Meta {
                name: name.into(),
                harness: if kind == RunKind::Shell { "sh" } else { "gemini" }.into(),
                kind,
                cwd: "/w/api".into(),
                argv: vec!["x".into()],
                started: at(100),
                socket: "/w/sock".into(),
                holder_pid: None,
            },
            status,
            state: None,
            hook: None,
            dir: "/h/sessions/x".into(),
        }
    }

    fn hook(agent: &str, sid: &str, state: Said, detail: Option<&str>) -> HookState {
        HookState {
            agent: agent.into(),
            session_id: sid.into(),
            cwd: "/w/api".into(),
            state,
            detail: detail.map(Into::into),
            pid: None,
            transcript_path: None,
            started: at(150),
            at: at(200),
        }
    }

    fn native(sid: &str) -> NativeRow {
        NativeRow {
            harness: "claude".into(),
            id: "ab12cd34".into(),
            session_id: sid.into(),
            name: "fix the login".into(),
            cwd: "/w/api".into(),
            state: NativeState {
                words: "needs input".into(),
                state: RunState::NeedsInput,
            },
            summary: Some("asks which way".into()),
            started: Some(at(50)),
            ended: None,
            at: Some(at(60)),
            pr: Some(Pr {
                number: 12,
                href: "https://x/pull/12".into(),
            }),
            color: None,
        }
    }

    fn seen(detail: Option<&str>, last: Option<&str>) -> StateFile {
        StateFile {
            state: RunState::Working,
            confidence: Confidence::Likely,
            detail: detail.map(Into::into),
            last_line: last.map(Into::into),
            foreground: None,
            at: at(150),
        }
    }

    #[test]
    fn a_native_row_hides_the_hooked_row_with_the_same_session_id() {
        let hooked = [
            hook("claude", "s-1", Said::Busy, None),
            hook("claude", "s-2", Said::Idle, None),
        ];
        let rows = rows(&[], &hooked, &[native("s-1")]);
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["o:claude:s-2", "n:claude:ab12cd34"]);
        let n = &rows[1];
        assert_eq!((n.source, n.words.as_str()), (Source::Native, "needs input"));
        assert_eq!(n.state, RunState::NeedsInput);
        assert_eq!(n.pr.as_ref().map(|p| p.number), Some(12));
    }

    #[test]
    fn a_relay_held_sessions_hook_word_is_not_a_row_of_its_own() {
        let mut h = held("api-gemini", RunKind::Agent, Status::Live);
        h.hook = Some(hook("gemini", "g-1", Said::Waiting, Some("permission prompt")));
        let mut by_id = held("api-claude", RunKind::Agent, Status::Live);
        by_id.hook = Some(hook("claude", "c-1", Said::Busy, None));
        let hooked = [
            hook("gemini", "g-1", Said::Waiting, None),
            hook("claude", "c-1", Said::Busy, None),
        ];
        let rows = rows(&[h, by_id], &hooked, &[]);
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["r:api-gemini", "r:api-claude"]);
        assert_eq!(rows[0].words, "needs you");
        assert_eq!(rows[0].attach, Attach::Held("api-gemini".into()));
    }

    #[test]
    fn a_held_summary_is_the_hooks_else_the_holders_detail_else_its_last_line() {
        let summary = |hook_detail: Option<&str>, detail: Option<&str>, last: Option<&str>| {
            let mut h = held("a", RunKind::Agent, Status::Live);
            h.state = Some(seen(detail, last));
            h.hook = hook_detail.map(|d| HookState {
                at: at(10),
                ..hook("gemini", "g", Said::Busy, Some(d))
            });
            rows(&[h], &[], &[]).remove(0).summary
        };
        assert_eq!(summary(Some("hook"), Some("seen"), Some("$ ")).as_deref(), Some("hook"));
        assert_eq!(summary(None, Some("seen"), Some("$ ")).as_deref(), Some("seen"));
        assert_eq!(summary(None, None, Some("$ ")).as_deref(), Some("$ "));
        assert_eq!(summary(None, None, None), None);
    }

    #[test]
    fn a_held_row_says_how_it_stands() {
        let words = |h: Held| {
            let r = rows(&[h], &[], &[]).remove(0);
            (r.state, r.words)
        };
        let mut live = held("a", RunKind::Agent, Status::Live);
        assert_eq!(words(live.clone()), (RunState::Starting, "starting".into()));
        live.state = Some(seen(None, None));
        assert_eq!(words(live), (RunState::Working, "working~".into()));
        let ended = |code, signal: Option<&str>| {
            Status::Ended(Exit {
                code,
                signal: signal.map(Into::into),
                at: at(300),
            })
        };
        assert_eq!(
            words(held("a", RunKind::Agent, ended(0, None))),
            (RunState::Finished, "done".into())
        );
        assert_eq!(
            words(held("a", RunKind::Agent, ended(1, None))),
            (RunState::Failed, "failed".into())
        );
        assert_eq!(
            words(held("a", RunKind::Agent, ended(129, Some("SIGHUP")))),
            (RunState::Stopped, "stopped".into())
        );
        assert_eq!(
            words(held("a", RunKind::Agent, Status::Lost)),
            (RunState::Stale, "lost".into())
        );
        // A shell at rest says nothing: its icon says what it is.
        let mut sh = held("sh", RunKind::Shell, Status::Live);
        sh.state = Some(StateFile {
            state: RunState::Idle,
            ..seen(None, None)
        });
        assert_eq!(words(sh), (RunState::Idle, String::new()));
    }

    #[test]
    fn a_hooked_row_carries_its_conversation_on() {
        let r = rows(&[], &[hook("claude", "0123456789", Said::Idle, Some("done"))], &[]).remove(0);
        assert_eq!(r.name, "session 01234567");
        assert_eq!((r.state, r.words.as_str()), (RunState::Idle, "idle"));
        assert_eq!(r.summary.as_deref(), Some("done"));
        // Claude has a background of its own: it carries on there.
        let Attach::CarryOn { argv, background } = &r.attach else {
            panic!("{:?}", r.attach);
        };
        assert!(background);
        assert_eq!(argv[1..], ["--resume", "0123456789", "--bg"]);
        // Gemini has none: Relay holds it.
        let r = rows(&[], &[hook("gemini", "g-1", Said::Idle, None)], &[]).remove(0);
        let Attach::CarryOn { argv, background } = &r.attach else {
            panic!("{:?}", r.attach);
        };
        assert!(!background);
        assert_eq!(argv[1..], ["--resume", "g-1"]);
        // Codex's conversation is a thread on its daemon: resumed there,
        // never held.
        let r = rows(&[], &[hook("codex", "t-1", Said::Idle, None)], &[]).remove(0);
        let Attach::Argv(argv) = &r.attach else {
            panic!("{:?}", r.attach);
        };
        assert_eq!(argv[1..], ["resume", "t-1"]);
        // One busy or waiting in its own terminal is still in use there.
        for said in [Said::Busy, Said::Waiting] {
            let r = rows(&[], &[hook("claude", "c", said, None)], &[]).remove(0);
            assert_eq!(r.attach, Attach::None, "{said:?}");
        }
        let r = rows(&[], &[hook("nobody", "x", Said::Idle, None)], &[]).remove(0);
        assert_eq!(r.attach, Attach::None, "no profile, no way to carry it on");
    }

    #[test]
    fn a_hooked_row_is_not_carried_on_before_its_harness_has_said_what_runs_in_its_background() {
        let hooked = [
            hook("claude", "c-1", Said::Idle, None),
            hook("gemini", "g-1", Said::Idle, None),
        ];
        let none = BTreeSet::new();
        let r = super::rows(&[], &hooked, &[], &none);
        assert_eq!(r[0].attach, Attach::None, "claude may run it already");
        assert!(
            matches!(r[1].attach, Attach::CarryOn { background: false, .. }),
            "gemini has no background"
        );
        let claude: BTreeSet<String> = ["claude".to_string()].into();
        let r = super::rows(&[], &hooked, &[], &claude);
        assert!(matches!(r[0].attach, Attach::CarryOn { background: true, .. }));
    }

    #[test]
    fn a_native_session_still_going_stops_through_its_harness() {
        let r = rows(&[], &[], &[native("s-1")]).remove(0);
        assert_eq!(r.stop.as_deref(), Some("ab12cd34"), "claude stop <id>");
        let mut done = native("s-1");
        done.state = NativeState {
            words: "completed".into(),
            state: RunState::Finished,
        };
        assert_eq!(rows(&[], &[], &[done]).remove(0).stop, None, "nothing left to stop");
        let mut codex = native("t-1");
        codex.harness = "codex".into();
        assert_eq!(
            rows(&[], &[], &[codex.clone()]).remove(0).stop.as_deref(),
            Some("ab12cd34"),
            "a codex turn waiting on you is interrupted"
        );
        codex.state = NativeState {
            words: "ready".into(),
            state: RunState::Idle,
        };
        assert_eq!(rows(&[], &[], &[codex]).remove(0).stop, None, "nothing running to stop");
        let h = held("a", RunKind::Agent, Status::Live);
        assert_eq!(rows(&[h], &[], &[]).remove(0).stop, None, "Relay's own stop it");
    }

    #[test]
    fn a_native_session_offers_its_harnesss_stop_while_it_goes_and_its_close_always() {
        let offers = |n: NativeRow| {
            let r = rows(&[], &[], &[n]).remove(0);
            (r.stop_choice.map(|c| c.offer()), r.close.map(|c| c.offer()))
        };
        let live = native("s-1");
        assert_eq!(
            offers(live.clone()),
            (
                Some("s stop · keeps the conversation".into()),
                Some("r remove · deletes it and its worktree".into())
            )
        );
        let mut done = live.clone();
        done.state = NativeState {
            words: "completed".into(),
            state: RunState::Finished,
        };
        assert_eq!(
            offers(done),
            (None, Some("r remove · deletes it and its worktree".into())),
            "ended: only the close"
        );
        let mut codex = live;
        codex.harness = "codex".into();
        codex.state = NativeState {
            words: "resting".into(),
            state: RunState::Idle,
        };
        assert_eq!(
            offers(codex),
            (
                None,
                Some("a archive · off the list, codex unarchive brings it back".into())
            )
        );
        let h = held("a", RunKind::Agent, Status::Live);
        let r = rows(&[h], &[], &[]).remove(0);
        assert_eq!((r.stop_choice, r.close), (None, None), "Relay's own: as ever");
    }

    #[test]
    fn each_row_knows_its_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let mut in_repo = hook("gemini", "g-1", Said::Busy, None);
        in_repo.cwd = repo.join("src");
        let mut plain = hook("gemini", "g-2", Said::Busy, None);
        plain.cwd = tmp.path().to_path_buf();
        let r = rows(&[], &[in_repo, plain], &[]);
        assert_eq!(r[0].repo.as_deref(), Some(repo.as_path()));
        assert_eq!(r[1].repo, None);
    }

    #[test]
    fn a_row_in_a_worktree_stands_under_its_main_checkout_unless_you_named_the_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap().join("repo");
        let entry = repo.join(".git/worktrees/foo");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("commondir"), "../..\n").unwrap();
        let wt = repo.join(".claude/worktrees/foo");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", entry.display())).unwrap();
        let mut in_wt = hook("gemini", "g-1", Said::Busy, None);
        in_wt.cwd = wt.clone();
        let r = rows(&[], &[in_wt], &[]);
        assert_eq!(r[0].repo.as_deref(), Some(repo.as_path()));
        let none = std::collections::BTreeMap::new();
        assert_eq!(crate::rows::project_of(&r[0], &crate::rows::Names::new(&none)), repo);
        // A folder you named still wins.
        let named = std::collections::BTreeMap::from([(format!("g:{}", wt.display()), "foo".to_string())]);
        assert_eq!(crate::rows::project_of(&r[0], &crate::rows::Names::new(&named)), wt);
    }

    #[test]
    fn a_native_row_attaches_through_its_harness() {
        let r = rows(&[], &[], &[native("s-1")]).remove(0);
        let Attach::Argv(argv) = &r.attach else {
            panic!("{:?}", r.attach);
        };
        assert_eq!(argv[1..], ["attach", "ab12cd34"]);
        let mut codex = native("01a11d20-8cab-7061-885d-f86e8e313f11");
        codex.harness = "codex".into();
        codex.id = codex.session_id.clone();
        let Attach::Argv(argv) = rows(&[], &[], &[codex]).remove(0).attach else {
            panic!("codex attaches its own");
        };
        assert_eq!(argv[1..], ["resume", "01a11d20-8cab-7061-885d-f86e8e313f11"]);
        let mut nobody = native("s-3");
        nobody.harness = "nobody".into();
        assert_eq!(rows(&[], &[], &[nobody]).remove(0).attach, Attach::None, "no harness");
    }

    #[test]
    fn a_codex_thread_relay_holds_stands_once_as_held() {
        let mut h = held("api-codex", RunKind::Agent, Status::Live);
        h.meta.harness = "codex".into();
        h.hook = Some(hook("codex", "t-held", Said::Busy, None));
        let mut held_thread = native("t-held");
        held_thread.harness = "codex".into();
        let mut own = native("t-own");
        own.harness = "codex".into();
        own.id = "t-own".into();
        let hooked = [hook("codex", "t-own", Said::Busy, None)];
        let rows = rows(&[h], &hooked, &[held_thread, own]);
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["r:api-codex", "n:codex:t-own"]);
    }

    #[test]
    fn claudes_listing_and_its_hooks_name_one_session_alike() {
        // As recorded: carried on into another conversation, which its
        // hooks tell of.
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/claude-native");
        let listed = std::fs::read_to_string(fixtures.join("agents.json")).unwrap();
        let native = rl_detect::claude_native::rows(&listed, Some(&fixtures.join("jobs"))).unwrap();
        let hooked = [
            hook("claude", "2132ce0a-647c-43a1-9c71-fa758bc20c12", Said::Waiting, None),
            hook("claude", "0f0f0f0f-0000-4000-8000-000000000000", Said::Busy, None),
        ];
        let rows = rows(&[], &hooked, &native);
        let hooked: Vec<&str> = rows
            .iter()
            .filter(|r| r.source == Source::Hooked)
            .map(|r| r.key.as_str())
            .collect();
        assert_eq!(
            hooked,
            ["o:claude:0f0f0f0f-0000-4000-8000-000000000000"],
            "the background one stands once, as Claude lists it"
        );
        let r = rows.iter().find(|r| r.key == "n:claude:4a088e61").unwrap();
        assert_eq!((r.words.as_str(), r.state), ("needs input", RunState::NeedsInput));
    }

    #[test]
    fn a_session_asked_to_stop_reads_stopped_however_it_exits() {
        let tmp = tempfile::tempdir().unwrap();
        let ended = |code| {
            let mut h = held(
                "a",
                RunKind::Shell,
                Status::Ended(Exit {
                    code,
                    signal: None,
                    at: at(300),
                }),
            );
            h.dir = tmp.path().to_path_buf();
            rows(&[h], &[], &[]).remove(0).state
        };
        assert_eq!(ended(1), RunState::Failed, "no stop asked: as its code says");
        crate::store::sessions::mark_stopped(tmp.path()).unwrap();
        assert!(!tmp.path().join("stopped.tmp").exists(), "written whole");
        assert_eq!(ended(1), RunState::Stopped, "zsh says 1 on a hang-up");
        assert_eq!(ended(0), RunState::Stopped);
    }

    #[test]
    fn how_an_ended_session_ended_is_decided_in_one_place() {
        let exit = |code, signal: Option<&str>| Exit {
            code,
            signal: signal.map(Into::into),
            at: at(300),
        };
        assert_eq!(ended_state(&exit(0, None)), RunState::Finished);
        assert_eq!(ended_state(&exit(1, None)), RunState::Failed);
        assert_eq!(ended_state(&exit(2, None)), RunState::Failed);
        for s in ["SIGHUP", "SIGTERM", "SIGKILL", "SIGINT"] {
            assert_eq!(ended_state(&exit(129, Some(s))), RunState::Stopped, "{s}");
        }
        // A shell that took the hang-up and exited by it says only the code.
        assert_eq!(ended_state(&exit(129, None)), RunState::Stopped);
        assert!(!wants_you_state(ended_state(&exit(129, Some("SIGHUP")))));
        assert_eq!(ended_state(&exit(129, Some("SIGHUP"))).to_string(), "stopped");
    }

    fn wants_you_state(s: RunState) -> bool {
        crate::rows::wants(s)
    }

    #[test]
    fn age_runs_from_the_start_and_stops_when_it_ends() {
        let mut ended = held(
            "a",
            RunKind::Agent,
            Status::Ended(Exit {
                code: 0,
                signal: None,
                at: at(160),
            }),
        );
        ended.meta.started = at(100);
        let r = rows(&[ended], &[], &[]).remove(0);
        assert_eq!((r.started, r.ended), (Some(at(100)), Some(at(160))));
        // A hooked row runs from its first word, not its last.
        let mut first = hook("claude", "s", Said::Busy, None);
        first.started = at(120);
        assert_eq!(rows(&[], &[first], &[]).remove(0).started, Some(at(120)));
    }

    #[test]
    fn the_codex_stand_in_lives_under_the_target_folder_and_a_finished_runs_is_swept() {
        let bin = codex_stand_in();
        let deps = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        let mine = bin.parent().unwrap();
        assert_eq!(mine, deps.join(STAND_INS).join(std::process::id().to_string()));
        assert!(bin.is_file());
        // A run that has finished left its folder: the next run's sweep
        // takes it, and keeps a live one's.
        let base = tempfile::tempdir().unwrap();
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let dead = gone.id();
        gone.wait().unwrap();
        for pid in [dead, std::process::id()] {
            std::fs::create_dir_all(base.path().join(pid.to_string())).unwrap();
        }
        std::fs::create_dir_all(base.path().join("not-a-run")).unwrap();
        sweep_stand_ins(base.path());
        assert!(!base.path().join(dead.to_string()).exists());
        assert!(base.path().join(std::process::id().to_string()).exists());
        assert!(base.path().join("not-a-run").exists(), "only a run's");
    }
}
