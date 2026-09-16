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
use rl_core::time::fmt_duration;
use rl_proto::types::{Confidence, RunKind, RunState};
use std::time::Duration;

/// How much cleaned output is kept for reading the last lines.
const TAIL_MAX: usize = 4096;
/// How many of the last non-blank lines may be read for a prompt: the newest
/// line with content, plus any pure decoration (box borders, rules) drawn
/// under it. An older question above a fresh prompt is history, not state.
const LINES_READ: usize = 3;
/// How much of a typed command is kept: enough to know it again, not a
/// pasted heredoc.
const CMD_MAX: usize = 512;

/// One command a shell ran, as its own prompt marks told it: what was typed,
/// how it came out, how long it took. Only a shell whose prompt emits OSC 133
/// has these — without marks there are simply none, and nothing is guessed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    pub text: String,
    /// The exit code, when the `D` mark carried one.
    pub exit: Option<i32>,
    pub took: jiff::SignedDuration,
}

/// How many finished lines of a gathered command are kept. More than one is
/// only a continuation or a redraw; the oldest is no longer of interest.
const CMD_LINES: usize = 8;

/// A line as its editor leaves it. A carriage return parks the cursor at the
/// start of the line and erases nothing by itself — the next character printed
/// is what overwrites — a backspace takes one character back, and a newline
/// finishes the line. Just enough of a terminal to read back what was typed,
/// and no more: the module proper is not one, and this is not trying to be.
#[derive(Debug, Default)]
struct Typed {
    lines: Vec<String>,
    cur: String,
    overwrite: bool,
}

impl Typed {
    fn put(&mut self, s: &str) {
        for c in s.chars() {
            if c == '\n' {
                // A return before the newline was the cursor moving, not the
                // line being thrown away: it ends as it stands.
                self.overwrite = false;
                self.lines.push(std::mem::take(&mut self.cur));
                if self.lines.len() > CMD_LINES {
                    self.lines.remove(0);
                }
                continue;
            }
            if self.overwrite {
                self.cur.clear();
                self.overwrite = false;
            }
            if self.cur.len() < CMD_MAX {
                self.cur.push(c);
            }
        }
    }

    fn ret(&mut self) {
        self.overwrite = true;
    }

    fn back(&mut self) {
        self.overwrite = false;
        self.cur.pop();
    }

    /// The last line with anything on it. A prompt draws and redraws, and it
    /// is the line left standing that was typed.
    fn line(&self) -> String {
        std::iter::once(&self.cur)
            .chain(self.lines.iter().rev())
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or_default()
            .to_string()
    }
}

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
    /// Bytes of a character cut off by a read boundary.
    pending: Vec<u8>,
    /// ANSI-stripping state carried across reads, so an escape sequence cut
    /// off by a read boundary is resumed rather than leaking its tail into
    /// the visible text.
    ansi: ansi::Stripper,
    /// What is being typed: gathered between the `B` and `C` marks.
    typing: Option<Typed>,
    /// The command now running, and when it started — from `C` until `D`.
    running: Option<(String, Timestamp)>,
    /// Commands finished since they were last drained.
    done: Vec<ShellCommand>,
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
            pending: Vec::new(),
            ansi: ansi::Stripper::new(),
            typing: None,
            running: None,
            done: Vec::new(),
        }
    }

    /// Raw bytes from the holder. Output is life: `working`, as a guess for
    /// anything that sits at a prompt (spinners and clocks redraw), likely
    /// for a plain command.
    pub fn output(&mut self, raw: &[u8], now: Timestamp) -> Signal {
        // A read can end mid-character; hold those bytes for the next one.
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(raw);
        let cut = match std::str::from_utf8(&bytes) {
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            _ => bytes.len(),
        };
        self.pending = bytes[cut..].to_vec();
        let text = self.ansi.strip(&bytes[..cut]);
        self.read_marks(now);
        self.tail.push_str(&text);
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

    /// Walks one chunk's events in order: gathers what is typed between `B`
    /// and `C`, lets the line editor's returns and backspaces act on it, and
    /// closes the command at `D`.
    ///
    /// This reads the echo rather than the stripped text. What `strip` returns
    /// has already had a line restart applied to it — a `\r` just before the
    /// command runs takes the typed line away with it — so the line is only
    /// recoverable from what was echoed, with the editing applied here.
    fn read_marks(&mut self, now: Timestamp) {
        let events = self.ansi.take_events();
        let echo = self.ansi.take_echo();
        let mut at = 0;
        for (pos, ev) in events {
            if let Some(typing) = &mut self.typing {
                typing.put(&echo[at..pos]);
            }
            at = pos;
            let mark = match ev {
                ansi::Ev::Return => {
                    if let Some(t) = &mut self.typing {
                        t.ret();
                    }
                    continue;
                }
                ansi::Ev::Backspace => {
                    if let Some(t) = &mut self.typing {
                        t.back();
                    }
                    continue;
                }
                ansi::Ev::Mark(m) => m,
            };
            match mark {
                // A prompt drawn afresh abandons whatever was half-typed.
                ansi::Mark::Prompt => self.typing = None,
                ansi::Mark::Input => self.typing = Some(Typed::default()),
                // What was typed ends here; what follows is the command's own
                // output, which is not part of it.
                ansi::Mark::Output => {
                    let line = self.typing.take().map(|t| t.line()).unwrap_or_default();
                    self.running = Some((line, now));
                }
                ansi::Mark::Ended { exit } => {
                    if let Some((text, started)) = self.running.take()
                        && !text.is_empty()
                    {
                        self.done.push(ShellCommand {
                            text,
                            exit,
                            took: now.duration_since(started),
                        });
                    }
                }
            }
        }
        if let Some(typing) = &mut self.typing {
            typing.put(&echo[at..]);
        }
    }

    /// The commands that have finished since this was last called. Draining.
    pub fn take_commands(&mut self) -> Vec<ShellCommand> {
        std::mem::take(&mut self.done)
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
    fn prompt_marks_make_a_command_with_its_exit_and_how_long_it_took() {
        let reg = Registry::builtin();
        let mut h = Heuristics::new(reg.generic(), RunKind::Shell, t(0.0));
        h.output(b"\x1b]133;A\x07$ ", t(1.0));
        h.output(b"\x1b]133;B\x07make test\n\x1b]133;C\x07", t(2.0));
        assert!(h.take_commands().is_empty(), "still running: nothing finished yet");
        h.output(b"all green\n", t(3.0));
        h.output(b"\x1b]133;D;0\x07", t(6.0));
        let done = h.take_commands();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].text, "make test", "what was typed, not what it printed");
        assert_eq!(done[0].exit, Some(0));
        assert_eq!(done[0].took, SignedDuration::from_secs(4), "C until D");
        assert!(h.take_commands().is_empty(), "draining: reported once");
        assert!(h.tail().contains("all green"), "the output is still the tail");
    }

    #[test]
    fn a_whole_command_in_one_read_is_still_pulled_apart() {
        let reg = Registry::builtin();
        let mut h = Heuristics::new(reg.generic(), RunKind::Shell, t(0.0));
        h.output(b"\x1b]133;B\x07echo hi\x1b]133;C\x07hi\n\x1b]133;D;1\x07", t(5.0));
        let done = h.take_commands();
        assert_eq!(done.len(), 1);
        assert_eq!((done[0].text.as_str(), done[0].exit), ("echo hi", Some(1)));
    }

    #[test]
    fn what_the_line_editor_echoed_is_read_back_as_the_line_typed() {
        let reg = Registry::builtin();
        let mut h = Heuristics::new(reg.generic(), RunKind::Shell, t(0.0));
        // Byte for byte what zsh echoes for `sleep 2`, taken from a real
        // session's journal: the first character, then a backspace and the
        // line over again, then `\r\r\n` before the command starts.
        h.output(b"\x1b]133;B\x07", t(1.0));
        h.output(b"s", t(1.0));
        h.output(b"\x08sleep ", t(1.0));
        h.output(b"2\x1b[?2004l\r\r\n\x1b]133;C\x07", t(2.0));
        h.output(b"\x1b]133;D;0\x07", t(6.0));
        let done = h.take_commands();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].text, "sleep 2", "neither `ssleep 2` nor nothing at all");
        assert_eq!(done[0].exit, Some(0));
        assert_eq!(done[0].took, SignedDuration::from_secs(4));
    }

    #[test]
    fn a_shell_whose_prompt_says_nothing_yields_no_commands() {
        let reg = Registry::builtin();
        let mut h = Heuristics::new(reg.generic(), RunKind::Shell, t(0.0));
        h.output(b"$ make test\nall green\n", t(1.0));
        assert!(h.take_commands().is_empty(), "nothing is invented without marks");
    }

    #[test]
    fn output_resumes_an_escape_sequence_split_across_reads() {
        let reg = Registry::builtin();
        let mut h = Heuristics::new(reg.generic(), RunKind::Agent, t(0.0));
        h.output(b"\x1b[", t(0.0));
        h.output(b"32mready\n", t(1.0));
        assert_eq!(h.tail(), "ready\n", "split escape must not leak into the tail");
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
