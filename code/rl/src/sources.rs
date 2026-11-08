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
use std::collections::HashSet;
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
    fn list(&self) -> anyhow::Result<Vec<NativeRow>>;
}

/// An adapter, as a lister.
struct Listing(&'static dyn Adapter);

impl NativeLister for Listing {
    fn list(&self) -> anyhow::Result<Vec<NativeRow>> {
        self.0.list()
    }
}

/// Every adapter the profiles name, and whether its harness is on this
/// machine. Made once.
fn known() -> &'static [(Box<dyn Adapter>, bool)] {
    static KNOWN: OnceLock<Vec<(Box<dyn Adapter>, bool)>> = OnceLock::new();
    KNOWN.get_or_init(|| rl_detect::adapter::adapters(registry(), adapter_bin))
}

/// The harness an adapter runs: the one on this machine.
#[cfg(not(test))]
fn adapter_bin(bins: &[String]) -> Option<PathBuf> {
    find_bin(bins)
}

/// The tests never run a real harness, but a stand-in: a script that writes
/// its arguments to `argv` in the folder it is run in, and says a session id
/// (`0a1b2c3d`) when asked to start one in the background.
#[cfg(test)]
fn adapter_bin(_: &[String]) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    let bin = BIN.get_or_init(|| {
        let dir = tempfile::Builder::new()
            .prefix("rl-test-bins")
            .tempdir()
            .unwrap()
            .keep();
        let bin = dir.join("stand-in");
        let script =
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > argv\ncase \" $* \" in *' --bg '*) echo 'started 0a1b2c3d';; esac\n";
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    });
    Some(bin.clone())
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
    pub attach: Attach,
    pub kind: RunKind,
}

/// Every row, held first, then hooked, then native. A session never stands
/// twice: a hooked one that a native row or a Relay-held session already
/// is, nor a native one Relay holds (a held `codex` runs its thread in
/// Codex's daemon, which lists it).
pub fn rows(held: &[Held], hooked: &[HookState], native: &[NativeRow]) -> Vec<Row> {
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
    held.iter()
        .map(held_row)
        .chain(
            hooked
                .iter()
                .filter(|h| !taken.contains(h.session_id.as_str()))
                .map(hooked_row),
        )
        .chain(native.into_iter().map(native_row))
        .collect()
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
        attach: Attach::Held(m.name.clone()),
        kind: m.kind,
    }
}

/// What carries on a conversation open in another terminal: the harness's
/// own background where it has one, else a session Relay holds. Only one at
/// rest — one busy or waiting there is still in use.
fn carry_on(h: &HookState) -> Attach {
    if h.state != Said::Idle {
        return Attach::None;
    }
    if let Some(argv) = adapter(&h.agent).and_then(|a| a.carry_on_argv(&h.session_id)) {
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

fn hooked_row(h: &HookState) -> Row {
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
        started: Some(h.started.unwrap_or(h.at)),
        ended: None,
        state_at: Some(h.at),
        pr: None,
        attach: carry_on(h),
        kind: RunKind::Agent,
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
        attach: adapter(&n.harness).map_or(Attach::None, |a| Attach::Argv(a.attach_argv(&n.id))),
        kind: RunKind::Agent,
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
            started: None,
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
        // One busy or waiting in its own terminal is still in use there.
        for said in [Said::Busy, Said::Waiting] {
            let r = rows(&[], &[hook("claude", "c", said, None)], &[]).remove(0);
            assert_eq!(r.attach, Attach::None, "{said:?}");
        }
        let r = rows(&[], &[hook("nobody", "x", Said::Idle, None)], &[]).remove(0);
        assert_eq!(r.attach, Attach::None, "no profile, no way to carry it on");
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
        // A hooked row runs from its first word; an old file without one, from its last.
        let mut first = hook("claude", "s", Said::Busy, None);
        first.started = Some(at(150));
        assert_eq!(rows(&[], &[first], &[]).remove(0).started, Some(at(150)));
        let old = hook("claude", "s", Said::Busy, None);
        assert_eq!(rows(&[], &[old], &[]).remove(0).started, Some(at(200)));
    }
}
