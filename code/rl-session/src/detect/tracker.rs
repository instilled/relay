//! The holder's running answer to "what state is this run in?": the PTY
//! heuristics folded into one [`StateFile`], with a dirty flag so a writer
//! flushes once per change rather than once per read.

use super::heuristics::{Heuristics, Patterns, Timing};
use crate::state::StateFile;
use jiff::Timestamp;
use rl_core::state::{Confidence, RunKind, RunState, Signal};

pub const LAST_LINE_MAX: usize = 200;

pub struct Tracker {
    heuristics: Heuristics,
    current: StateFile,
    dirty: bool,
}

impl Tracker {
    /// Starts `Starting`/`Guess`, dirty, so the first flush writes a file.
    pub fn new(patterns: Patterns, timing: Timing, kind: RunKind, now: Timestamp) -> Self {
        Self {
            heuristics: Heuristics::new(patterns, timing, kind, now),
            current: StateFile {
                state: RunState::Starting,
                confidence: Confidence::Guess,
                detail: None,
                last_line: None,
                foreground: None,
                at: now,
            },
            dirty: true,
        }
    }

    pub fn output(&mut self, raw: &[u8], now: Timestamp) {
        let signal = self.heuristics.output(raw, now);
        self.apply(signal, now);
        let last_line = self.heuristics.last_line(LAST_LINE_MAX);
        if last_line != self.current.last_line {
            self.current.last_line = last_line;
            self.touch(now);
        }
    }

    pub fn tick(&mut self, now: Timestamp) {
        if let Some(signal) = self.heuristics.tick(now) {
            self.apply(signal, now);
        }
    }

    pub fn foreground(&mut self, shell_owns_foreground: bool) {
        self.heuristics.set_foreground(shell_owns_foreground);
        if self.current.foreground != Some(shell_owns_foreground) {
            self.current.foreground = Some(shell_owns_foreground);
            self.dirty = true;
        }
    }

    /// The state to flush: `Some` once per change, then `None` until the next.
    pub fn take_dirty(&mut self) -> Option<StateFile> {
        std::mem::take(&mut self.dirty).then(|| self.current.clone())
    }

    /// `Stale` replaces only `Working` or `Starting`; anything else sets state.
    fn apply(&mut self, signal: Signal, now: Timestamp) {
        if signal.state == RunState::Stale && !matches!(self.current.state, RunState::Working | RunState::Starting) {
            return;
        }
        let changed = signal.state != self.current.state
            || signal.confidence != self.current.confidence
            || signal.detail != self.current.detail;
        if changed {
            self.current.state = signal.state;
            self.current.confidence = signal.confidence;
            self.current.detail = signal.detail;
            self.touch(now);
        }
    }

    fn touch(&mut self, now: Timestamp) {
        self.current.at = now;
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use std::time::Duration;

    fn t(secs: f64) -> Timestamp {
        Timestamp::from_millisecond((secs * 1000.0) as i64).unwrap()
    }

    fn tracker_with(needs_input: &str, idle_ms: u64, stale_ms: u64, kind: RunKind) -> Tracker {
        let patterns = Patterns {
            needs_input: vec![Regex::new(needs_input).unwrap()],
            idle: vec![],
        };
        let timing = Timing {
            idle_after: Duration::from_millis(idle_ms),
            stale_after: Duration::from_millis(stale_ms),
        };
        Tracker::new(patterns, timing, kind, t(0.0))
    }

    #[test]
    fn a_question_after_quiet_reads_needs_input() {
        let mut tr = tracker_with(r"\[y/N\]", 100, 10_000, RunKind::Agent);
        tr.output(b"Continue? [y/N] ", t(0.0));
        tr.tick(t(0.2));
        let s = tr.take_dirty().unwrap();
        assert_eq!(s.state, RunState::NeedsInput);
        assert_eq!(s.detail.as_deref(), Some("Continue? [y/N]"));
        assert_eq!(s.last_line.as_deref(), Some("Continue? [y/N]"));
        assert!(tr.take_dirty().is_none());
    }

    #[test]
    fn stale_never_replaces_needs_input() {
        let mut tr = tracker_with(r"\[y/N\]", 100, 500, RunKind::Agent);
        tr.output(b"Continue? [y/N] ", t(0.0));
        tr.tick(t(0.2));
        tr.take_dirty();
        tr.tick(t(1.0));
        assert!(tr.take_dirty().is_none());
    }

    #[test]
    fn dirty_output_coalesces_into_one_flush() {
        let mut tr = tracker_with(r"\[y/N\]", 100, 10_000, RunKind::Agent);
        for i in 0..100 {
            tr.output(format!("frame {i}\r").as_bytes(), t(0.0));
        }
        assert!(tr.take_dirty().is_some());
        assert!(tr.take_dirty().is_none());
    }
}
