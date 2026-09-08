//! The floor: what the byte stream alone says. Output means working; silence
//! long enough means the last lines get read for a prompt or a question;
//! silence much longer while working means stale.
//!
//! Nothing here is ever `exact`. A run whose CLI offers real signals keeps
//! these from touching its state (see [`crate::machine`]).

use crate::Signal;
use crate::ansi;
use crate::registry::Profile;
use jiff::Timestamp;
use regex::Regex;
use relay_core::time::fmt_duration;
use relay_proto::types::{Confidence, RunKind, RunState};
use std::time::Duration;

/// How much cleaned output is kept for reading the last lines.
const TAIL_MAX: usize = 4096;
/// How many of the last non-blank lines may be read for a prompt: the newest
/// line with content, plus any pure decoration (box borders, rules) drawn
/// under it. An older question above a fresh prompt is history, not state.
const LINES_READ: usize = 3;

#[derive(Debug, Clone, Default)]
pub struct Patterns {
    pub needs_input: Vec<Regex>,
    pub idle: Vec<Regex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub idle_after: Duration,
    pub stale_after: Duration,
}

pub struct Heuristics {
    /// Agents and shells sit at prompts; commands do not.
    prompts: bool,
    patterns: Patterns,
    timing: Timing,
    tail: String,
    last_output: Timestamp,
    idle_pending: bool,
    stale_pending: bool,
}

impl Heuristics {
    pub fn new(profile: &Profile, kind: RunKind, now: Timestamp) -> Self {
        Self {
            prompts: kind != RunKind::Command,
            patterns: profile.patterns.clone(),
            timing: profile.timing,
            tail: String::new(),
            last_output: now,
            idle_pending: false,
            stale_pending: true,
        }
    }

    /// Raw bytes from the holder. Output is life: `working`, as a guess for
    /// anything that sits at a prompt (spinners and clocks redraw), likely
    /// for a plain command.
    pub fn output(&mut self, raw: &[u8], now: Timestamp) -> Signal {
        self.tail.push_str(&ansi::strip(raw));
        if self.tail.len() > TAIL_MAX {
            let cut = self.tail.len() - TAIL_MAX;
            let cut = self
                .tail
                .char_indices()
                .map(|(i, _)| i)
                .find(|&i| i >= cut)
                .unwrap_or(0);
            self.tail.drain(..cut);
        }
        self.last_output = now;
        self.idle_pending = true;
        self.stale_pending = true;
        let confidence = if self.prompts {
            Confidence::Guess
        } else {
            Confidence::Likely
        };
        Signal::pty(RunState::Working, confidence, None)
    }

    /// Called periodically. Fires at most once per quiet spell for each of
    /// idle and stale.
    pub fn tick(&mut self, now: Timestamp) -> Option<Signal> {
        let quiet = now.duration_since(self.last_output);
        let quiet = Duration::from_millis(quiet.as_millis().max(0) as u64);
        if self.stale_pending && quiet >= self.timing.stale_after {
            self.stale_pending = false;
            self.idle_pending = false;
            let d = fmt_duration(jiff::SignedDuration::try_from(quiet).unwrap_or_default());
            return Some(Signal::pty(
                RunState::Stale,
                Confidence::Likely,
                Some(format!("no output for {d}")),
            ));
        }
        if self.prompts && self.idle_pending && quiet >= self.timing.idle_after {
            self.idle_pending = false;
            return Some(self.classify());
        }
        None
    }

    /// Reads the last lines: a question beats a prompt beats nothing.
    fn classify(&self) -> Signal {
        let mut lines = Vec::new();
        for line in ansi::last_lines(&self.tail, LINES_READ) {
            lines.push(line);
            if !is_decoration(line) {
                break;
            }
        }
        for line in &lines {
            if self.patterns.needs_input.iter().any(|re| re.is_match(line)) {
                return Signal::pty(RunState::NeedsInput, Confidence::Likely, Some(line.to_string()));
            }
        }
        for line in &lines {
            if self.patterns.idle.iter().any(|re| re.is_match(line)) {
                return Signal::pty(RunState::Idle, Confidence::Likely, Some(line.to_string()));
            }
        }
        Signal::pty(RunState::Idle, Confidence::Guess, None)
    }

    /// Cleaned recent output, for the fleet row's last line.
    pub fn tail(&self) -> &str {
        &self.tail
    }

    pub fn last_line(&self, max: usize) -> Option<String> {
        ansi::last_line(&self.tail, max)
    }
}

/// Box borders, rules, and the like: what TUIs draw under an input box.
fn is_decoration(line: &str) -> bool {
    line.chars()
        .all(|c| c.is_whitespace() || ('\u{2500}'..='\u{259F}').contains(&c) || "-=_|+*~.:'`".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Registry;
    use jiff::SignedDuration;

    fn t(secs: f64) -> Timestamp {
        Timestamp::UNIX_EPOCH + SignedDuration::from_secs_f64(secs)
    }

    #[test]
    fn prompt_then_question_then_output() {
        let reg = Registry::builtin();
        let mut h = Heuristics::new(reg.generic(), RunKind::Agent, t(0.0));
        assert_eq!(h.tick(t(1.0)), None, "nothing seen yet: no idle verdict");

        let s = h.output(b"\x1b[32mready\x1b[0m\n> ", t(1.0));
        assert_eq!((s.state, s.confidence), (RunState::Working, Confidence::Guess));
        assert_eq!(h.tick(t(2.0)), None, "not quiet long enough");
        let s = h.tick(t(4.5)).unwrap();
        assert_eq!((s.state, s.confidence), (RunState::Idle, Confidence::Likely));
        assert_eq!(s.detail.as_deref(), Some(">"));
        assert_eq!(h.tick(t(5.0)), None, "one verdict per quiet spell");

        h.output(b"Do you want to proceed? [y/N] ", t(6.0));
        let s = h.tick(t(9.5)).unwrap();
        assert_eq!((s.state, s.confidence), (RunState::NeedsInput, Confidence::Likely));
        assert_eq!(
            s.detail.as_deref(),
            Some("> Do you want to proceed? [y/N]"),
            "the whole line, as seen"
        );

        h.output(b"y\nlots of text with no prompt\n", t(10.0));
        let s = h.tick(t(14.0)).unwrap();
        assert_eq!((s.state, s.confidence), (RunState::Idle, Confidence::Guess));
        assert_eq!(h.last_line(80).as_deref(), Some("lots of text with no prompt"));
    }

    #[test]
    fn commands_do_not_idle_but_do_go_stale() {
        let reg = Registry::builtin();
        let mut h = Heuristics::new(reg.generic(), RunKind::Command, t(0.0));
        let s = h.output(b"compiling...\n", t(1.0));
        assert_eq!(s.confidence, Confidence::Likely);
        assert_eq!(h.tick(t(100.0)), None, "a quiet command is still working");
        let s = h.tick(t(1.0 + 600.0)).unwrap();
        assert_eq!((s.state, s.confidence), (RunState::Stale, Confidence::Likely));
        assert_eq!(s.detail.as_deref(), Some("no output for 10m00s"));
        assert_eq!(h.tick(t(2000.0)), None, "stale reported once");
        h.output(b"done\n", t(2001.0));
        assert!(h.tick(t(2001.0 + 700.0)).is_some(), "silence after output counts again");
    }

    #[test]
    fn decoration_under_a_line_is_read_through_but_history_is_not() {
        let reg = Registry::builtin();
        let mut h = Heuristics::new(reg.generic(), RunKind::Agent, t(0.0));
        h.output("Do you want to proceed? [y/N]\n╰────────╯\n".as_bytes(), t(0.0));
        assert_eq!(
            h.tick(t(10.0)).unwrap().state,
            RunState::NeedsInput,
            "border under the question"
        );
        h.output("y\nOverwrite file? (y/n)\n│ >\n".as_bytes(), t(20.0));
        let s = h.tick(t(30.0)).unwrap();
        assert_eq!(
            (s.state, s.confidence),
            (RunState::Idle, Confidence::Likely),
            "a newer prompt wins"
        );
        assert!(is_decoration(" ───── ") && is_decoration("+---+") && !is_decoration("? for shortcuts"));
    }
}
