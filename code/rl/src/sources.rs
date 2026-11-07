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
use std::collections::HashSet;
use std::path::PathBuf;

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
    /// Run this to carry the conversation on, then attach.
    CarryOn(Vec<String>),
    None,
}

/// A pull request a session opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pr {
    pub number: u64,
    pub href: String,
}

/// A harness's own words for how one of its sessions stands, and the
/// `RunState` it maps to — for grouping and the icon only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeState {
    pub words: String,
    pub state: RunState,
}

/// One of a harness's own background sessions, as its adapter lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRow {
    pub harness: String,
    /// The harness's id for it, what attaching takes.
    pub id: String,
    /// Its conversation's id, what hooks tell of.
    pub session_id: String,
    pub name: String,
    pub cwd: PathBuf,
    pub state: NativeState,
    pub summary: Option<String>,
    pub started: Option<Timestamp>,
    pub ended: Option<Timestamp>,
    /// When it came to stand as it does.
    pub at: Option<Timestamp>,
    pub pr: Option<Pr>,
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

/// Every row, held first, then hooked, then native. A hooked session that a
/// native row or a Relay-held session already is never stands twice.
pub fn rows(held: &[Held], hooked: &[HookState], native: &[NativeRow]) -> Vec<Row> {
    let taken: HashSet<&str> = held
        .iter()
        .flat_map(|h| {
            [
                h.hook.as_ref().map(|k| k.session_id.as_str()),
                h.meta.native_id.as_deref(),
            ]
        })
        .flatten()
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
        .chain(native.iter().map(native_row))
        .collect()
}

fn held_row(h: &Held) -> Row {
    let m = &h.meta;
    let (state, confidence, at) = match &h.status {
        Status::Live => match h.current() {
            Some(c) => (c.state, c.confidence, c.at),
            None => (RunState::Starting, Confidence::Exact, m.started),
        },
        Status::Ended(e) => {
            let state = if e.signal.is_some() {
                RunState::Stopped
            } else if e.code == 0 {
                RunState::Finished
            } else {
                RunState::Failed
            };
            (state, Confidence::Exact, e.at)
        }
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

fn hooked_row(h: &HookState) -> Row {
    let state = match h.state {
        Said::Idle => RunState::Idle,
        Said::Busy => RunState::Working,
        Said::Waiting => RunState::NeedsInput,
    };
    let carry_on = registry().profile(&h.agent).and_then(|p| {
        let bin = find_bin(&p.bins)
            .map(|b| b.to_string_lossy().into_owned())
            .or_else(|| p.bins.first().cloned())?;
        p.resume_argv(bin, &h.session_id)
    });
    Row {
        key: format!("o:{}:{}", h.agent, h.session_id),
        source: Source::Hooked,
        harness: h.agent.clone(),
        name: format!("session {}", h.session_id.chars().take(8).collect::<String>()),
        cwd: h.cwd.clone(),
        state,
        words: state_words(state, Confidence::Exact),
        summary: h.detail.clone(),
        started: None,
        ended: None,
        state_at: Some(h.at),
        pr: None,
        attach: carry_on.map_or(Attach::None, Attach::CarryOn),
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
        // The adapter's attach argv is wired in with the adapters.
        attach: Attach::None,
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
        let Attach::CarryOn(argv) = &r.attach else {
            panic!("{:?}", r.attach);
        };
        assert_eq!(argv[1..], ["--resume", "0123456789"]);
        let r = rows(&[], &[hook("nobody", "x", Said::Busy, None)], &[]).remove(0);
        assert_eq!(r.attach, Attach::None, "no profile, no way to carry it on");
    }
}
