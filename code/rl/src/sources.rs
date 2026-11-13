//! The list's rows, from its three sources: sessions Relay holds
//! (`sessions/`), agents' own sessions their hooks told of (`state/`), and
//! the harnesses' own background sessions (`Harness::sessions`). One `Row`
//! each, with the `Session` its harness's intents are asked of: what enter
//! does with it, and what stops and removes it, are its harness's to say.

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

/// What enter does with a row, as its harness says (`Harness::attach`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attach {
    /// Attach to the Relay-held session of this name.
    Held(String),
    /// Run this, in the terminal, to attach to the harness's own session.
    Argv(Vec<String>),
    /// Start this to carry the conversation on (`Harness::start`), then
    /// attach to what it began.
    CarryOn(Start),
    None,
}

pub use rl_detect::harness::{Harness, NativeRow, NativeState, Owner, Pr, Session, Start};

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
        &self.0.info().name
    }

    fn list(&self) -> anyhow::Result<Vec<NativeRow>> {
        self.list_within(rl_detect::harness::LIST_WAIT)
    }

    fn list_within(&self, wait: std::time::Duration) -> anyhow::Result<Vec<NativeRow>> {
        self.0.sessions(wait).rows()
    }
}

/// Every harness the profiles name, found on this machine or not, Relay's
/// own sessions stopped and removed by its store (`cmd::Store`). Made once.
#[cfg(not(test))]
pub fn harnesses() -> &'static [Box<dyn Harness>] {
    static ALL: OnceLock<Vec<Box<dyn Harness>>> = OnceLock::new();
    ALL.get_or_init(|| {
        rl_detect::harness::set_holder(std::sync::Arc::new(crate::cmd::Store));
        rl_detect::harness::harnesses(registry(), find_bin)
    })
}

/// The tests never run a real harness, but a stand-in: Codex's daemon a
/// copy of `fixtures/stand-in-codex-daemon` (`codex_stand_in`), every other
/// `fixtures/stand-in-harness`, and the listing of each with a background
/// is theirs (`StandIn`).
#[cfg(test)]
pub fn harnesses() -> &'static [Box<dyn Harness>] {
    static ALL: OnceLock<Vec<Box<dyn Harness>>> = OnceLock::new();
    ALL.get_or_init(|| {
        rl_detect::harness::set_holder(std::sync::Arc::new(crate::cmd::Store));
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
            let lists = registry
                .profile(&h.info().name)
                .and_then(|p| p.sessions.as_ref())
                .is_some_and(|s| s.list.is_some());
            if !lists {
                return h;
            }
            let claude = registry
                .profile(&h.info().name)
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
            |_| Ok(self.listed()),
            argv,
            cwd,
            before,
            rl_detect::claude_native::LISTED_WAIT,
        )
    }

    /// What it lists: what Claude started, working, in each folder.
    fn listed(&self) -> Vec<NativeRow> {
        let started = STARTED.lock().unwrap();
        started
            .iter()
            .map(|cwd| NativeRow {
                harness: self.inner.info().name.clone(),
                id: "0a1b2c3d".into(),
                elsewhere: false,
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
            .collect()
    }
}

#[cfg(test)]
impl Harness for StandIn {
    fn info(&self) -> &rl_detect::harness::Info {
        self.inner.info()
    }

    fn sessions(&self, _: std::time::Duration) -> rl_detect::harness::Listed {
        rl_detect::harness::Listed::Rows(self.listed())
    }

    /// Claude's start, run on the stand-in and listed by it; the rest the
    /// harness's own.
    fn start(&self, s: &rl_detect::harness::Start) -> anyhow::Result<rl_detect::harness::Started> {
        let argv = s.words.is_none().then(|| {
            self.claude
                .as_ref()
                .and_then(|c| c.bg_argv(s.resume.as_deref(), s.name.as_deref(), s.prompt.as_deref()))
        });
        match argv.flatten() {
            Some(argv) => self
                .begin(&argv, &s.cwd, &HashSet::new())
                .map(rl_detect::harness::Started::Background),
            None => Harness::start(self.inner.as_ref(), s),
        }
    }

    fn attach(&self, s: &rl_detect::harness::Session) -> rl_detect::harness::Attach {
        self.inner.attach(s)
    }

    fn actions(&self, s: &rl_detect::harness::Session) -> rl_detect::harness::Actions {
        self.inner.actions(s)
    }

    fn stop(&self, s: &rl_detect::harness::Session) -> anyhow::Result<()> {
        Harness::stop(self.inner.as_ref(), s)
    }

    fn remove(&self, s: &rl_detect::harness::Session) -> anyhow::Result<()> {
        Harness::remove(self.inner.as_ref(), s)
    }

    fn rename(&self, s: &rl_detect::harness::Session, name: &str) -> rl_detect::harness::Renamed {
        Harness::rename(self.inner.as_ref(), s, name)
    }

    fn notify(&self, s: &rl_detect::harness::Session, mail: &rl_detect::harness::Mail) -> rl_detect::harness::Notified {
        self.inner.notify(s, mail)
    }

    fn setup(&self, env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> anyhow::Result<Vec<rl_detect::setup::Part>> {
        self.inner.setup(env)
    }

    fn caller(&self, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
        self.inner.caller(env)
    }

    fn check(&self) -> rl_detect::harness::Compat {
        self.inner.check()
    }
}

/// The harness of the profile `name`.
pub fn harness(name: &str) -> Option<&'static dyn Harness> {
    harnesses().iter().find(|h| h.info().name == name).map(|h| h.as_ref())
}

/// The harness of a session's: its profile's, else the generic one (a
/// command Relay holds that no profile names).
pub fn harness_of(name: &str) -> &'static dyn Harness {
    harness(name)
        .or_else(|| harness(rl_detect::registry::GENERIC))
        .expect("the generic profile's harness")
}

/// The listers the list asks: the harnesses on this machine (one Relay
/// holds lists none of its own).
pub fn native_listers() -> Vec<Box<dyn NativeLister>> {
    harnesses()
        .iter()
        .filter(|h| h.info().found)
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
    /// The id its harness stops it by, where it is a harness's own session
    /// its harness can stop as it stands (`Harness::actions`).
    pub stop: Option<String>,
    /// Whether its harness deletes it for good (`Harness::actions`): a
    /// harness's own session; one Relay holds is asked about by `^x`.
    pub removes: bool,
    /// What its harness's intents are asked of.
    pub session: Session,
    pub kind: RunKind,
    /// The root of the repository its folder is in, as read when the rows
    /// were (`git::toplevel`).
    pub repo: Option<PathBuf>,
}

/// Every row, held first, then those open in other terminals, then the
/// harnesses' own. A session never stands twice: a hooked one that a native
/// row or a Relay-held session already is, nor a native one Relay holds (a
/// harness's own session a held one's hooks have said is its conversation).
/// One its harness lists as open elsewhere and its hooks tell of too is one
/// row, by its conversation (`o:<harness>:<id>`), with what each knows best
/// (`elsewhere_row`). `answered`: the harnesses that have listed their
/// sessions at least once — until then, a hooked row of one may be among
/// them, and is not carried on. Each folder's repository is looked for once.
pub fn rows(held: &[Held], hooked: &[HookState], native: &[NativeRow], answered: &BTreeSet<String>) -> Vec<Row> {
    let held_ids: HashSet<&str> = held
        .iter()
        .filter_map(|h| h.hook.as_ref().map(|k| k.session_id.as_str()))
        .collect();
    let (elsewhere, native): (Vec<&NativeRow>, Vec<&NativeRow>) = native
        .iter()
        .filter(|n| !held_ids.contains(n.session_id.as_str()))
        .partition(|n| n.elsewhere);
    let taken: HashSet<&str> = held_ids
        .iter()
        .copied()
        .chain(native.iter().map(|n| n.session_id.as_str()))
        .collect();
    let listed = |harness: &str, sid: &str| {
        elsewhere
            .iter()
            .copied()
            .find(|n| n.harness == harness && n.session_id == sid)
    };
    let hooked: Vec<&HookState> = hooked
        .iter()
        .filter(|h| !taken.contains(h.session_id.as_str()))
        .collect();
    let told = |n: &NativeRow| {
        hooked
            .iter()
            .any(|h| h.agent == n.harness && h.session_id == n.session_id)
    };
    let mut rows: Vec<Row> = held
        .iter()
        .map(held_row)
        .chain(
            hooked
                .iter()
                .map(|h| elsewhere_row(Some(h), listed(&h.agent, &h.session_id), answered)),
        )
        .chain(
            elsewhere
                .iter()
                .filter(|n| !taken.contains(n.session_id.as_str()) && !told(n))
                .map(|n| elsewhere_row(None, Some(n), answered)),
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
    let session = Session {
        harness: m.harness.clone(),
        owner: Owner::Relay {
            live: h.status == Status::Live,
            dir: h.dir.clone(),
        },
        id: m.name.clone(),
        session_id: h.hook.as_ref().map(|k| k.session_id.clone()).unwrap_or_default(),
        cwd: m.cwd.clone(),
        state,
        words: words.clone(),
        name: m.name.clone(),
        relay_name: None,
        attached: false,
    };
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
        attach: attach(&session),
        stop: None,
        removes: false,
        kind: m.kind,
        repo: None,
        session,
    }
}

/// What enter does with `s`, as its harness says.
fn attach(s: &Session) -> Attach {
    match harness_of(&s.harness).attach(s) {
        rl_detect::harness::Attach::Held => Attach::Held(s.id.clone()),
        rl_detect::harness::Attach::Argv(argv) => Attach::Argv(argv),
        rl_detect::harness::Attach::CarryOn(start) => Attach::CarryOn(start),
        rl_detect::harness::Attach::No => Attach::None,
    }
}

/// A conversation open in another terminal is carried on as its harness
/// says (`Harness::attach`): in the harness's own background where it has
/// one, else a session Relay holds — or, for a harness Relay never holds
/// (Codex: the conversation is a thread on its daemon), its own attach. Only
/// one at rest — one busy or waiting there is still in use — and, in a
/// background, only once the harness has said what runs there (`answered`,
/// `Owner::Elsewhere::listed`): carried on while it already runs there, it
/// would run twice.
///
/// What its hooks told of it (`hook`), what its harness lists of it as open
/// elsewhere (`listed`), or both: one row, keyed by its conversation either
/// way, so a name, a hide or a pin kept for it stays with it. The name is
/// its harness's, where it gives one (else `session <id>`); how it stands,
/// its summary and its times are the hooks', where they told — they say
/// more (one waiting on you) — else its harness's.
fn elsewhere_row(hook: Option<&HookState>, listed: Option<&NativeRow>, answered: &BTreeSet<String>) -> Row {
    let (harness, sid, cwd, state, words) = match (hook, listed) {
        (Some(h), _) => {
            let state = match h.state {
                Said::Idle => RunState::Idle,
                Said::Busy => RunState::Working,
                Said::Waiting => RunState::NeedsInput,
            };
            let words = state_words(state, Confidence::Exact);
            (&h.agent, &h.session_id, &h.cwd, state, words)
        }
        (None, Some(n)) => (&n.harness, &n.session_id, &n.cwd, n.state.state, n.state.words.clone()),
        (None, None) => unreachable!("a row of what its hooks told or its harness listed"),
    };
    let name = listed
        .map(|n| n.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("session {}", sid.chars().take(8).collect::<String>()));
    let session = Session {
        harness: harness.clone(),
        owner: Owner::Elsewhere {
            listed: answered.contains(harness),
        },
        id: sid.clone(),
        session_id: sid.clone(),
        cwd: cwd.clone(),
        state,
        words: words.clone(),
        name: name.clone(),
        relay_name: None,
        attached: false,
    };
    Row {
        key: format!("o:{harness}:{sid}"),
        source: Source::Hooked,
        harness: harness.clone(),
        name,
        cwd: cwd.clone(),
        state,
        words,
        summary: hook
            .and_then(|h| h.detail.clone())
            .or_else(|| listed.and_then(|n| n.summary.clone())),
        started: hook.map(|h| h.started).or_else(|| listed.and_then(|n| n.started)),
        ended: None,
        state_at: hook.map(|h| h.at).or_else(|| listed.and_then(|n| n.at)),
        pr: listed.and_then(|n| n.pr.clone()),
        color: listed.and_then(|n| n.color.clone()),
        attach: attach(&session),
        stop: None,
        removes: false,
        kind: RunKind::Agent,
        repo: None,
        session,
    }
}

/// The key the list keeps a harness's listed session by: `n:<harness>:<id>`
/// for one of its own, `o:<harness>:<conversation>` for one open elsewhere
/// (the key its hooks' row has always had).
pub fn key_of(n: &NativeRow) -> String {
    if n.elsewhere {
        format!("o:{}:{}", n.harness, n.session_id)
    } else {
        format!("n:{}:{}", n.harness, n.id)
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
    let session = Session::own(n);
    let can = harness_of(&n.harness).actions(&session);
    Row {
        key: key_of(n),
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
        attach: attach(&session),
        stop: can.stop.then(|| n.id.clone()),
        removes: can.remove,
        kind: RunKind::Agent,
        repo: None,
        session,
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
        let all = harnesses().iter().map(|h| h.info().name.clone()).collect();
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
            elsewhere: false,
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
        let Attach::CarryOn(start) = &r.attach else {
            panic!("{:?}", r.attach);
        };
        assert_eq!(start.resume.as_deref(), Some("0123456789"));
        // Gemini has none: Relay holds it.
        let r = rows(&[], &[hook("gemini", "g-1", Said::Idle, None)], &[]).remove(0);
        let Attach::CarryOn(start) = &r.attach else {
            panic!("{:?}", r.attach);
        };
        assert_eq!(
            harness("gemini").unwrap().start(start).unwrap(),
            rl_detect::harness::Started::Held(vec!["--resume".into(), "g-1".into()]),
            "held, its profile's resume"
        );
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
        assert!(matches!(r[1].attach, Attach::CarryOn(_)), "gemini has no background");
        let claude: BTreeSet<String> = ["claude".to_string()].into();
        let r = super::rows(&[], &hooked, &[], &claude);
        assert!(matches!(r[0].attach, Attach::CarryOn(_)));
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
    fn a_native_session_is_removed_by_its_harness_running_or_not() {
        let removes = |n: NativeRow| rows(&[], &[], &[n]).remove(0).removes;
        let live = native("s-1");
        assert!(removes(live.clone()), "claude rm <id>");
        let mut done = live.clone();
        done.state = NativeState {
            words: "completed".into(),
            state: RunState::Finished,
        };
        assert!(removes(done), "ended too");
        let mut codex = live;
        codex.harness = "codex".into();
        codex.state = NativeState {
            words: "resting".into(),
            state: RunState::Idle,
        };
        assert!(removes(codex), "thread/delete");
        let h = held("a", RunKind::Agent, Status::Live);
        assert!(!rows(&[h], &[], &[]).remove(0).removes, "Relay removes its own");
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
        let t = rows.iter().find(|r| r.key == hooked[0]).unwrap();
        assert_eq!(t.name, "in a terminal", "the one in a terminal, by Claude's name");
    }

    /// One Claude lists as open in a terminal of its own.
    fn in_terminal(sid: &str, name: &str) -> NativeRow {
        NativeRow {
            id: sid.into(),
            elsewhere: true,
            name: name.into(),
            state: NativeState {
                words: "idle".into(),
                state: RunState::Idle,
            },
            summary: None,
            started: Some(at(40)),
            at: None,
            pr: None,
            ..native(sid)
        }
    }

    #[test]
    fn one_open_elsewhere_that_its_harness_lists_and_its_hooks_tell_of_is_one_row_by_its_conversation() {
        let hooked = [
            hook("claude", "t-1", Said::Waiting, Some("asks which port")),
            hook("claude", "t-2", Said::Busy, None),
        ];
        let listed = [
            in_terminal("t-1", "os-dispatch-ea"),
            in_terminal("t-2", ""),
            in_terminal("t-3", "alone"),
            native("s-bg"),
        ];
        let rows = rows(&[], &hooked, &listed);
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            ["o:claude:t-1", "o:claude:t-2", "o:claude:t-3", "n:claude:ab12cd34"],
            "each once, by the key its hooks' row has always had"
        );
        let both = &rows[0];
        assert_eq!(both.name, "os-dispatch-ea", "Claude's name");
        assert_eq!(
            (both.state, both.summary.as_deref(), both.started),
            (RunState::NeedsInput, Some("asks which port"), Some(at(150))),
            "what its hooks know"
        );
        assert_eq!(both.source, Source::Hooked);
        assert!(matches!(both.session.owner, Owner::Elsewhere { listed: true }));
        assert_eq!(
            (both.stop.as_ref(), both.removes),
            (None, false),
            "not the list's to stop"
        );
        assert_eq!(rows[1].name, "session t-2", "no name: as ever");
        let alone = &rows[2];
        assert_eq!(
            (alone.name.as_str(), alone.state, alone.words.as_str(), alone.started),
            ("alone", RunState::Idle, "idle", Some(at(40))),
            "as Claude says"
        );
        let Attach::CarryOn(start) = &alone.attach else {
            panic!("at rest: carried on, as one its hooks tell of: {:?}", alone.attach);
        };
        assert_eq!(start.resume.as_deref(), Some("t-3"));
        assert_eq!(native_id(alone), None, "no id of its harness's own");
        assert_eq!(key_of(&listed[2]), "o:claude:t-3");
        assert_eq!(key_of(&listed[3]), "n:claude:ab12cd34");
        // One Relay holds is its held row only.
        let mut h = held("api-claude", RunKind::Agent, Status::Live);
        h.hook = Some(hook("claude", "t-1", Said::Busy, None));
        let rows = super::rows(&[h], &[], &listed[..1], &BTreeSet::new());
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["r:api-claude"]);
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
