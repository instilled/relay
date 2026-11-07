//! The state machine: `(current, signal) → transition`. Every source — PTY
//! heuristics, hooks, a human's `rl mark` — produces a [`Signal`]; this is
//! the only place that decides whether it changes anything.
//!
//! The rules, in order:
//! 1. Terminal states never change here (exits are recorded by the supervisor).
//! 2. `stale` only ever replaces `working`/`starting`.
//! 3. A PTY guess never overrides an exact state from another source; hooks
//!    and humans do not get second-guessed by a timer.
//! 4. Same state again: keep the row, upgrade source/confidence if the new
//!    signal is more certain, otherwise nothing.
//! 5. Anything else is a transition.

use rl_proto::types::{Confidence, Run, RunState, Source, event_kind};
use serde_json::{Value, json};

pub use rl_core::state::Signal;

/// What the ledger says right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Current {
    pub state: RunState,
    pub source: Source,
    pub confidence: Confidence,
}

impl From<&Run> for Current {
    fn from(r: &Run) -> Self {
        Self {
            state: r.state,
            source: r.source,
            confidence: r.confidence,
        }
    }
}

/// A change to record: new state (or a confidence upgrade of the same one),
/// where it came from, and the event it deserves, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: RunState,
    pub state: RunState,
    pub source: Source,
    pub confidence: Confidence,
    pub detail: Option<String>,
}

impl Transition {
    pub fn changed(&self) -> bool {
        self.from != self.state
    }

    /// Attention states and their release always make an event; so does
    /// `stale`. Other changes only when they are better than a guess, so a
    /// PTY-only run does not flood the ledger with `working?`/`idle?` flaps.
    pub fn event_kind(&self) -> Option<&'static str> {
        if !self.changed() {
            return None;
        }
        Some(match self.state {
            RunState::NeedsInput => event_kind::RUN_NEEDS_INPUT,
            RunState::Blocked => event_kind::RUN_BLOCKED,
            RunState::Stale => event_kind::RUN_STATE,
            RunState::Working | RunState::Idle if self.from.needs_attention() || self.from == RunState::Stale => {
                event_kind::RUN_RESUMED
            }
            _ if self.confidence == Confidence::Guess => return None,
            _ => event_kind::RUN_STATE,
        })
    }

    pub fn summary(&self) -> String {
        let detail = self.detail.as_deref().map(str::trim).filter(|d| !d.is_empty());
        match (self.state, detail) {
            (RunState::NeedsInput, Some(d)) => format!("needs input: {d}"),
            (RunState::NeedsInput, None) => "needs input".into(),
            (RunState::Blocked, Some(d)) => format!("blocked: {d}"),
            (RunState::Blocked, None) => "blocked".into(),
            (RunState::Stale, Some(d)) => format!("stale: {d}"),
            (RunState::Stale, None) => "stale".into(),
            (s, _) if self.from.needs_attention() || self.from == RunState::Stale => {
                format!("resumed ({s} after {})", self.from)
            }
            (s, Some(d)) => format!("{s}: {d}"),
            (s, None) => s.to_string(),
        }
    }

    pub fn data(&self) -> Value {
        json!({
            "state": self.state,
            "from": self.from,
            "source": self.source,
            "confidence": self.confidence,
            "detail": self.detail,
        })
    }
}

pub fn step(cur: Current, sig: &Signal) -> Option<Transition> {
    if cur.state.is_terminal() {
        return None;
    }
    if sig.state == RunState::Stale && !matches!(cur.state, RunState::Working | RunState::Starting) {
        return None;
    }
    let exact_elsewhere = cur.source != Source::Pty && cur.confidence == Confidence::Exact;
    if sig.source == Source::Pty && exact_elsewhere && sig.state != RunState::Stale {
        return None;
    }
    if sig.state == cur.state && sig.confidence <= cur.confidence {
        return None;
    }
    Some(Transition {
        from: cur.state,
        state: sig.state,
        source: sig.source,
        confidence: sig.confidence,
        detail: sig.detail.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cur(state: RunState, source: Source, confidence: Confidence) -> Current {
        Current {
            state,
            source,
            confidence,
        }
    }
    const PTY_GUESS_WORKING: Current = Current {
        state: RunState::Working,
        source: Source::Pty,
        confidence: Confidence::Guess,
    };

    #[test]
    fn guesses_move_guesses() {
        let t = step(PTY_GUESS_WORKING, &Signal::pty(RunState::Idle, Confidence::Guess, None)).unwrap();
        assert!(t.changed());
        assert_eq!(t.event_kind(), None, "guess flaps make no events");
        let t = step(
            cur(RunState::Idle, Source::Pty, Confidence::Guess),
            &Signal::pty(RunState::Idle, Confidence::Likely, Some("> ".into())),
        )
        .unwrap();
        assert!(!t.changed(), "same state, better confidence: upgrade only");
        assert_eq!(t.confidence, Confidence::Likely);
        assert!(
            step(
                cur(RunState::Idle, Source::Pty, Confidence::Likely),
                &Signal::pty(RunState::Idle, Confidence::Guess, None)
            )
            .is_none()
        );
    }

    #[test]
    fn exact_beats_pty_and_pty_cannot_undo_it() {
        let hook = Signal::hook(RunState::NeedsInput, Some("Bash: rm -rf".into()));
        let t = step(PTY_GUESS_WORKING, &hook).unwrap();
        assert_eq!(t.event_kind(), Some(event_kind::RUN_NEEDS_INPUT));
        assert_eq!(t.summary(), "needs input: Bash: rm -rf");
        let now = cur(RunState::NeedsInput, Source::Hook, Confidence::Exact);
        assert!(step(now, &Signal::pty(RunState::Working, Confidence::Guess, None)).is_none());
        assert!(step(now, &Signal::pty(RunState::Idle, Confidence::Likely, None)).is_none());
        // Another exact source may.
        let t = step(now, &Signal::manual(RunState::Working, None)).unwrap();
        assert_eq!(t.event_kind(), Some(event_kind::RUN_RESUMED));
        assert_eq!(t.summary(), "resumed (working after needs_input)");
        let t = step(now, &Signal::hook(RunState::Idle, None)).unwrap();
        assert_eq!(t.event_kind(), Some(event_kind::RUN_RESUMED));
    }

    #[test]
    fn stale_only_replaces_working() {
        let stale = Signal::pty(RunState::Stale, Confidence::Likely, Some("no output for 10m".into()));
        let t = step(cur(RunState::Working, Source::Hook, Confidence::Exact), &stale).unwrap();
        assert_eq!(t.event_kind(), Some(event_kind::RUN_STATE));
        assert_eq!(t.summary(), "stale: no output for 10m");
        assert!(step(cur(RunState::Idle, Source::Pty, Confidence::Guess), &stale).is_none());
        assert!(step(cur(RunState::NeedsInput, Source::Hook, Confidence::Exact), &stale).is_none());
        // Life after stale is a resume, even as a guess.
        let t = step(
            cur(RunState::Stale, Source::Pty, Confidence::Likely),
            &Signal::pty(RunState::Working, Confidence::Guess, None),
        )
        .unwrap();
        assert_eq!(t.event_kind(), Some(event_kind::RUN_RESUMED));
    }

    #[test]
    fn terminal_is_final() {
        for s in [RunState::Finished, RunState::Failed, RunState::Stopped] {
            assert!(
                step(
                    cur(s, Source::Pty, Confidence::Exact),
                    &Signal::manual(RunState::Blocked, None)
                )
                .is_none()
            );
        }
    }

    #[test]
    fn likely_pty_changes_make_events() {
        let t = step(
            PTY_GUESS_WORKING,
            &Signal::pty(RunState::Idle, Confidence::Likely, Some("$ ".into())),
        )
        .unwrap();
        assert_eq!(t.event_kind(), Some(event_kind::RUN_STATE));
        assert_eq!(t.summary(), "idle: $");
        assert_eq!(t.data()["from"], "working");
    }
}
