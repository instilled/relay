//! The list's rows, from its three sources: sessions Relay holds
//! (`sessions/`), agents' own sessions their hooks told of (`state/`), and
//! the harnesses' own background sessions (their adapters). One `Row` each;
//! the list never asks which source a row came from.

use crate::rows::{registry, state_words};
use crate::store::sessions::{Held, Status};
use crate::store::state::{HookState, Said};
use jiff::Timestamp;
use rl_core::state::{Confidence, RunKind, RunState};
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

pub use rl_detect::adapter::{Adapter, NativeRow, NativeState, Pr};

/// What lists a harness's own background sessions: its adapter. Asked from a
/// background task while the list is open, so `list` may block (it runs the
/// harness's CLI); an error keeps what it said last.
pub trait NativeLister: Send + Sync {
    /// The harness whose sessions it lists: `claude`.
    fn harness(&self) -> &str;
    fn list(&self) -> anyhow::Result<Vec<NativeRow>>;
}

/// An adapter, as a lister.
struct Listing(&'static dyn Adapter);

impl NativeLister for Listing {
    fn harness(&self) -> &str {
        self.0.harness()
    }

    fn list(&self) -> anyhow::Result<Vec<NativeRow>> {
        self.0.list()
    }
}

/// Every adapter the profiles name, and whether its harness is on this
/// machine. Made once.
#[cfg(not(test))]
fn known() -> &'static [(Box<dyn Adapter>, bool)] {
    static KNOWN: OnceLock<Vec<(Box<dyn Adapter>, bool)>> = OnceLock::new();
    KNOWN.get_or_init(|| rl_detect::adapter::adapters(registry(), find_bin))
}

/// The tests never run a real harness, but a stand-in: Codex's daemon a
/// copy of `fixtures/stand-in-codex-daemon` (`codex_stand_in`), every other
/// `fixtures/stand-in-harness`, and the listing is theirs (`StandIn`).
#[cfg(test)]
fn known() -> &'static [(Box<dyn Adapter>, bool)] {
    static KNOWN: OnceLock<Vec<(Box<dyn Adapter>, bool)>> = OnceLock::new();
    KNOWN.get_or_init(|| {
        let bin = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-harness"));
        let codex = codex_stand_in();
        let registry = registry();
        rl_detect::adapter::adapters(registry, |bins| {
            Some(if bins.iter().any(|b| b == "codex") {
                codex.clone()
            } else {
                bin.clone()
            })
        })
        .into_iter()
        .map(|(a, found)| {
            let claude = registry
                .profile(a.harness())
                .filter(|p| p.name == "claude")
                .and_then(|p| rl_detect::claude_native::Claude::of(p, bin.clone()));
            (Box::new(StandIn { inner: a, claude }) as Box<dyn Adapter>, found)
        })
        .collect()
    })
}

/// The copy of `fixtures/stand-in-codex-daemon` the tests' Codex runs, in a
/// folder of this test run's own, where it keeps what it was sent.
#[cfg(test)]
pub(crate) fn codex_stand_in() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap().keep();
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

/// A harness's adapter run on the stand-in: what Claude starts in its
/// background (`0a1b2c3d`, in the folder asked) is listed as working there
/// from then on, by every harness's listing.
#[cfg(test)]
struct StandIn {
    inner: Box<dyn Adapter>,
    /// Claude's adapter, where it is Claude's: what it starts with.
    claude: Option<rl_detect::claude_native::Claude>,
}

#[cfg(test)]
static STARTED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
impl StandIn {
    /// Runs `argv` in Claude's background, as Claude's adapter does, its
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
impl Adapter for StandIn {
    fn harness(&self) -> &str {
        self.inner.harness()
    }

    fn list_within(&self, _: std::time::Duration) -> anyhow::Result<Vec<NativeRow>> {
        let started = STARTED.lock().unwrap();
        Ok(started
            .iter()
            .map(|cwd| NativeRow {
                harness: self.harness().into(),
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

    fn attach_argv(&self, id: &str) -> Vec<String> {
        self.inner.attach_argv(id)
    }

    fn start(
        &self,
        cwd: &std::path::Path,
        name: Option<&str>,
        prompt: Option<&str>,
        before: &HashSet<String>,
    ) -> Option<anyhow::Result<rl_detect::adapter::Started>> {
        match &self.claude {
            Some(c) => {
                let argv = c.start_argv(name, prompt)?;
                Some(self.begin(&argv, cwd, before).map(rl_detect::adapter::Started::Id))
            }
            None => self.inner.start(cwd, name, prompt, before),
        }
    }

    fn never_held(&self) -> bool {
        self.inner.never_held()
    }

    fn carry_on_argv(&self, session_id: &str) -> Option<Vec<String>> {
        self.inner.carry_on_argv(session_id)
    }

    fn carry_on(&self, argv: &[String], cwd: &std::path::Path, before: &HashSet<String>) -> anyhow::Result<String> {
        self.begin(argv, cwd, before)
    }

    fn stops(&self, row: &NativeRow) -> bool {
        self.inner.stops(row)
    }

    fn stop(&self, id: &str, cwd: &std::path::Path) -> anyhow::Result<()> {
        self.inner.stop(id, cwd)
    }

    fn ping_argv(&self, id: &str, text: &str) -> Option<Vec<String>> {
        self.inner.ping_argv(id, text)
    }
}

/// The adapter of `harness`'s own background sessions, if it has one.
pub fn adapter(harness: &str) -> Option<&'static dyn Adapter> {
    known()
        .iter()
        .find(|(a, _)| a.harness() == harness)
        .map(|(a, _)| a.as_ref())
}

/// The listers the list asks: the adapters of the harnesses on this machine.
pub fn native_listers() -> Vec<Box<dyn NativeLister>> {
    known()
        .iter()
        .filter(|(_, found)| *found)
        .map(|(a, _)| Box::new(Listing(a.as_ref())) as Box<dyn NativeLister>)
        .collect()
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
    /// The id its harness stops it by (`Adapter::stop`), where it is a
    /// harness's own session its harness can stop as it stands.
    pub stop: Option<String>,
    pub kind: RunKind,
    /// The root of the repository its folder is in, as read when the rows
    /// were (`git::toplevel`).
    pub repo: Option<PathBuf>,
}

/// Every row, held first, then hooked, then native. A session never stands
/// twice: a hooked one that a native row or a Relay-held session already
/// is, nor a native one Relay holds (a harness's own session a held one
/// has said is its conversation). `answered`: the harnesses whose adapter
/// has listed its sessions at least once — until then, a hooked row of one
/// may be among them, and is not carried on. Each folder's repository is
/// looked for once.
pub fn rows(held: &[Held], hooked: &[HookState], native: &[NativeRow], answered: &BTreeSet<String>) -> Vec<Row> {
    let held_ids: HashSet<&str> = held
        .iter()
        .flat_map(|h| {
            [
                h.hook.as_ref().map(|k| k.session_id.as_str()),
                h.meta.native_id.as_deref(),
            ]
        })
        .flatten()
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
        kind: m.kind,
        repo: None,
    }
}

/// What carries on a conversation open in another terminal: the harness's
/// own background where it has one, else a session Relay holds — or, for a
/// harness Relay never holds (Codex: the conversation is a thread on its
/// daemon), its own attach. Only one at rest — one busy or waiting there is
/// still in use — and, where the harness has a background, only once it has
/// said what runs there (`answered`): carried on while it already runs
/// there, it would run twice.
fn carry_on(h: &HookState, answered: &BTreeSet<String>) -> Attach {
    if h.state != Said::Idle {
        return Attach::None;
    }
    if let Some(a) = adapter(&h.agent).filter(|a| a.never_held()) {
        return Attach::Argv(a.attach_argv(&h.session_id));
    }
    if let Some(a) = adapter(&h.agent)
        && let Some(argv) = a.carry_on_argv(&h.session_id)
    {
        if !answered.contains(a.harness()) {
            return Attach::None;
        }
        return Attach::CarryOn { argv, background: true };
    }
    registry()
        .profile(&h.agent)
        .and_then(|p| {
            let bin = find_bin(&p.bins)
                .map(|b| b.to_string_lossy().into_owned())
                .or_else(|| p.bins.first().cloned())?;
            p.resume_argv(bin, &h.session_id)
        })
        .map_or(Attach::None, |argv| Attach::CarryOn {
            argv,
            background: false,
        })
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
        kind: RunKind::Agent,
        repo: None,
    }
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
        attach: adapter(&n.harness).map_or(Attach::None, |a| Attach::Argv(a.attach_argv(&n.id))),
        stop: adapter(&n.harness).filter(|a| a.stops(n)).map(|_| n.id.clone()),
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

    /// The rows, every adapter having answered.
    fn rows(held: &[Held], hooked: &[HookState], native: &[NativeRow]) -> Vec<Row> {
        let all = known().iter().map(|(a, _)| a.harness().to_string()).collect();
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
                native_id: None,
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
        by_id.meta.native_id = Some("c-1".into());
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
            panic!("codex has an adapter");
        };
        assert_eq!(argv[1..], ["resume", "01a11d20-8cab-7061-885d-f86e8e313f11"]);
        let mut nobody = native("s-3");
        nobody.harness = "nobody".into();
        assert_eq!(rows(&[], &[], &[nobody]).remove(0).attach, Attach::None, "no adapter");
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
}
