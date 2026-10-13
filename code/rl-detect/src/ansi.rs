//! Just enough terminal-output cleanup to read the last few lines of a run.
//! Not a terminal emulator: a full-screen TUI's bottom line is not always the
//! last thing it wrote, which is why heuristics built on this stay `likely`
//! at best.

use rl_session::screen::{EscState, dec_graphic, overrun};
use std::path::PathBuf;

/// A shell's OSC 133 prompt mark: where a prompt was drawn, where what you
/// typed began, where it started running, and how it came out. Only a shell
/// whose prompt emits these has them; nothing here invents one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// `133;A` — a fresh prompt.
    Prompt,
    /// `133;B` — what you type starts here.
    Input,
    /// `133;C` — it is running now; its output starts here.
    Output,
    /// `133;D[;exit]` — it ended, with its exit code when the shell says one.
    Ended { exit: Option<i32> },
}

/// Something that happened at a point in a chunk's stripped text: a prompt
/// mark, or one of the two things a line editor does that the text alone
/// cannot show — the cursor going back to the start of the line, and going
/// back over one character. This is no terminal and drops both from what it
/// returns; whoever is gathering a line as it was typed needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ev {
    Mark(Mark),
    /// `\r`: back to the start of the line. Nothing is erased by this alone —
    /// the next character printed is what overwrites.
    Return,
    /// `\x08`: back over one character.
    Backspace,
}

/// An OSC payload read as a prompt mark. Anything else — a window title, a
/// colour query — is `None`, and is stripped the way it always was.
fn mark_of(payload: &str) -> Option<Mark> {
    let rest = payload.strip_prefix("133;")?;
    let mut parts = rest.split(';');
    // Shells add parameters of their own (`133;A;aid=7`); the letter is all
    // that is read.
    match parts.next()?.chars().next()? {
        'A' => Some(Mark::Prompt),
        'B' => Some(Mark::Input),
        'C' => Some(Mark::Output),
        'D' => Some(Mark::Ended {
            exit: parts.next().and_then(|c| c.trim().parse().ok()),
        }),
        _ => None,
    }
}

/// An OSC 7 payload — `7;file://host/path`, the path percent-encoded — read
/// as the folder a shell says it is in. The host is ignored: a shell under
/// Relay runs on this machine. Anything else is `None`.
pub fn cwd_of(payload: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = payload.strip_prefix("7;")?.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    Some(PathBuf::from(std::ffi::OsString::from_vec(percent_decode(path))))
}

/// `%XX` as the byte it stands for; a `%` not followed by two hex digits is
/// left as it is.
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Stateful ANSI stripper: like [`strip`], but remembers an escape sequence
/// left open at the end of one chunk so the next call resumes it correctly
/// instead of re-parsing its tail as literal text. Use one `Stripper` per
/// stream (e.g. per PTY) and feed it consecutive chunks.
#[derive(Debug, Clone, Default)]
pub struct Stripper {
    state: EscState,
    pending: usize,
    /// The OSC payload being read, kept only until its terminator.
    osc: String,
    /// What happened in the chunk just stripped, in order, each with the byte
    /// offset into that chunk's text where it stood. One read can hold a whole
    /// command — `B`, what was typed, `C`, its output, `D` — and the echo of
    /// a line being edited, so the offsets are what make the pieces tellable
    /// apart afterwards.
    events: Vec<(usize, Ev)>,
    /// The same chunk's printable characters with nothing undone: no line
    /// restart, no backspace. What [`Stripper::strip`] returns has those
    /// applied, destructively — a `\r` before the command runs takes the line
    /// with it — so a line being gathered as it was typed is recovered from
    /// here, and the events are positioned against this, not against that.
    echo: String,
    /// Whether each designated set is the box-drawing one: G0, then G1.
    sets: [bool; 2],
    /// Which of them letters are drawn from: shift-out chooses G1, shift-in
    /// G0. A prompt that rules a line off designates G1 once and shifts out
    /// for the run of `q`s; read as letters they spell `qqqqqq`.
    shifted: usize,
    /// The folder the stream last said it was in (OSC 7), until taken.
    cwd: Option<PathBuf>,
}

impl Stripper {
    pub fn new() -> Self {
        Self::default()
    }

    /// The prompt marks of the text [`strip`] last returned, each with the
    /// byte offset into that text where it stood. Draining: a mark is read
    /// once, and offsets only ever mean anything against that one chunk.
    pub fn take_events(&mut self) -> Vec<(usize, Ev)> {
        std::mem::take(&mut self.events)
    }

    /// The chunk just stripped as it was echoed, with nothing undone, which is
    /// what the offsets from [`Self::take_events`] measure against. Draining.
    pub fn take_echo(&mut self) -> String {
        std::mem::take(&mut self.echo)
    }

    /// The folder the stream last reported (OSC 7) since this was last
    /// called, the latest of several. Draining.
    pub fn take_cwd(&mut self) -> Option<PathBuf> {
        self.cwd.take()
    }

    /// Strips CSI/OSC/other escape sequences and resolves `\r` within a
    /// line, resuming any escape sequence left open by the previous call.
    pub fn strip(&mut self, bytes: &[u8]) -> String {
        let text = String::from_utf8_lossy(bytes);
        let mut out = String::with_capacity(text.len());
        // Last chunk's marks were measured against last chunk's text; they
        // would be nonsense against this one.
        self.events.clear();
        self.echo.clear();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match std::mem::take(&mut self.state) {
                EscState::None => match c {
                    '\x1b' => {
                        self.state = EscState::Escape;
                        self.pending = 1;
                    }
                    '\r' => {
                        // Carriage return without newline: the line restarts.
                        // Only for what is returned — the echo keeps what was
                        // written, and whoever gathers a line decides what a
                        // return means to it.
                        if chars.peek() != Some(&'\n') {
                            if let Some(pos) = out.rfind('\n') {
                                out.truncate(pos + 1);
                            } else {
                                out.clear();
                            }
                        }
                        self.events.push((self.echo.len(), Ev::Return));
                    }
                    '\x07' => {}
                    '\x08' => self.events.push((self.echo.len(), Ev::Backspace)),
                    // Shift-out and shift-in: which designated set letters are
                    // drawn from from here on.
                    '\x0e' => self.shifted = 1,
                    '\x0f' => self.shifted = 0,
                    c if c.is_control() && c != '\n' && c != '\t' => {}
                    c => {
                        let c = if self.sets[self.shifted] { dec_graphic(c) } else { c };
                        out.push(c);
                        self.echo.push(c);
                    }
                },
                // CSI: ESC [ params… final byte 0x40..=0x7E
                // OSC: ESC ] … BEL or ESC \
                // Two-byte escapes (ESC 7, ESC =, charset selects with one more byte…)
                EscState::Escape => {
                    self.pending += 1;
                    self.state = match c {
                        '[' => EscState::Csi,
                        ']' => {
                            self.osc.clear();
                            EscState::Osc { prev_was_esc: false }
                        }
                        '(' | ')' => EscState::Charset { g0: c == '(' },
                        _ => EscState::None,
                    };
                }
                EscState::Csi => {
                    self.pending += 1;
                    self.state = if ('\x40'..='\x7e').contains(&c) {
                        EscState::None
                    } else {
                        EscState::Csi
                    };
                }
                EscState::Osc { prev_was_esc } => {
                    self.pending += 1;
                    if c == '\x07' || (prev_was_esc && c == '\\') {
                        // Where the mark stood is where the text has got to.
                        if let Some(m) = mark_of(&self.osc) {
                            self.events.push((self.echo.len(), Ev::Mark(m)));
                        } else if let Some(cwd) = cwd_of(&self.osc) {
                            self.cwd = Some(cwd);
                        }
                        self.osc.clear();
                        self.state = EscState::None;
                    } else {
                        // The ESC of an `ESC \` terminator is not payload.
                        if c != '\x1b' {
                            self.osc.push(c);
                        }
                        self.state = EscState::Osc {
                            prev_was_esc: c == '\x1b',
                        };
                    }
                }
                EscState::Charset { g0 } => {
                    self.pending += 1;
                    // `0` is the box-drawing set, `B` ASCII. Designating a set
                    // does not draw from it: that takes a shift, which is how
                    // a prompt rules a line — `ESC ) 0` once, then shift out.
                    self.sets[usize::from(!g0)] = c == '0';
                    self.state = EscState::None;
                }
            }
            if overrun(&self.state, self.pending) {
                // Give up: not a well-formed escape after all, so whatever of
                // it was read is not half a mark either.
                self.state = EscState::None;
                self.osc.clear();
            }
        }
        out
    }
}

/// Strips CSI/OSC/other escape sequences and resolves `\r` within a line.
/// One-shot: bytes must already contain any escape sequence in full. For a
/// stream read in chunks, where a sequence may be split across reads, keep a
/// [`Stripper`] instead.
pub fn strip(bytes: &[u8]) -> String {
    Stripper::default().strip(bytes)
}

/// Last non-blank line of `text`, trimmed and capped at `max` chars.
pub fn last_line(text: &str, max: usize) -> Option<String> {
    let line = text
        .lines()
        .rev()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || is_rule(c)))
        .find(|l| !l.is_empty())?;
    Some(line.chars().take(max).collect())
}

/// Box-drawing and block characters: the rules and borders TUIs draw around
/// their status lines.
fn is_rule(c: char) -> bool {
    ('\u{2500}'..='\u{259F}').contains(&c)
}

/// The last `n` non-blank lines, newest first, trimmed.
pub fn last_lines(text: &str, n: usize) -> Vec<&str> {
    text.lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(n)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_session::screen::MAX_PENDING_ESCAPE;

    #[test]
    fn last_line_skips_rules_and_borders() {
        assert_eq!(
            last_line("hello\n──────⏵⏵ auto mode on ──\n────────\n", 80).as_deref(),
            Some("⏵⏵ auto mode on")
        );
        assert_eq!(last_line("│ > │\n", 80).as_deref(), Some(">"));
    }

    #[test]
    fn prompt_marks_are_read_out_of_the_osc_it_strips() {
        let mut s = Stripper::default();
        let out = s.strip(b"\x1b]133;A\x07$ \x1b]133;B\x07make test\x1b]133;C\x07ok\n\x1b]133;D;1\x07");
        assert_eq!(out, "$ make testok\n", "a mark leaves no text of its own behind");
        assert_eq!(
            s.take_events(),
            vec![
                (0, Ev::Mark(Mark::Prompt)),
                (2, Ev::Mark(Mark::Input)),
                (11, Ev::Mark(Mark::Output)),
                (14, Ev::Mark(Mark::Ended { exit: Some(1) })),
            ],
            "each mark stands where the echo had got to, so `make test` lies between B and C"
        );
        assert!(s.take_events().is_empty(), "draining: an event is read once");
    }

    #[test]
    fn a_mark_split_across_reads_and_an_osc_that_is_not_one() {
        let mut s = Stripper::default();
        assert_eq!(s.strip(b"\x1b]13"), "");
        assert!(s.take_events().is_empty(), "not terminated yet, so not a mark yet");
        // Resumed, and terminated by ST (`ESC \`) rather than BEL.
        assert_eq!(s.strip(b"3;D;0\x1b\\done\n"), "done\n");
        assert_eq!(s.take_events(), vec![(0, Ev::Mark(Mark::Ended { exit: Some(0) }))]);
        // A window title is stripped as it always was, and is no mark.
        assert_eq!(s.strip(b"\x1b]0;title\x07real"), "real");
        assert!(s.take_events().is_empty());
        // `D` with no code still ends the command; the exit is simply unknown.
        s.strip(b"\x1b]133;D\x07");
        assert_eq!(s.take_events(), vec![(0, Ev::Mark(Mark::Ended { exit: None }))]);
    }

    #[test]
    fn a_shell_says_its_folder_in_an_osc_7() {
        let p = |s: &str| cwd_of(s).map(|p| p.to_string_lossy().into_owned());
        // With a host, and without one; the host is not ours to check.
        assert_eq!(p("7;file://mac.local/Users/f/app").as_deref(), Some("/Users/f/app"));
        assert_eq!(p("7;file:///Users/f/app").as_deref(), Some("/Users/f/app"));
        // Percent-encoded: a space, an ampersand, a multibyte name.
        assert_eq!(
            p("7;file://h/Users/f/Finance%20%26%20Real%20Estate").as_deref(),
            Some("/Users/f/Finance & Real Estate")
        );
        assert_eq!(p("7;file://h/tmp/caf%C3%A9").as_deref(), Some("/tmp/café"));
        // Unencoded, and a stray `%`, pass through as written.
        assert_eq!(p("7;file://h/a b&c/50%").as_deref(), Some("/a b&c/50%"));
        assert_eq!(p("7;file://h/x/%zz").as_deref(), Some("/x/%zz"));
        // Not a folder: no path, another scheme, another OSC.
        assert_eq!(p("7;file://host"), None);
        assert_eq!(p("7;http://h/x"), None);
        assert_eq!(p("133;A"), None);
        assert_eq!(p("0;title"), None);

        // In a stream: stripped, the latest kept, read once — and a long one
        // is not mistaken for a stray escape that runs into the text.
        let mut s = Stripper::default();
        let long = format!("/Users/f/{}", "deep/".repeat(40));
        let bytes = format!("\x1b]7;file://h/tmp/a\x07$ \x1b]7;file://h{long}\x1b\\ok");
        assert_eq!(s.strip(bytes.as_bytes()), "$ ok");
        assert_eq!(s.take_cwd(), Some(PathBuf::from(&long)));
        assert_eq!(s.take_cwd(), None, "draining");
        // Split across reads.
        assert_eq!(s.strip(b"\x1b]7;file://h/tm"), "");
        assert_eq!(s.take_cwd(), None);
        assert_eq!(s.strip(b"p/x%20y\x07"), "");
        assert_eq!(s.take_cwd(), Some(PathBuf::from("/tmp/x y")));
    }

    #[test]
    fn what_a_line_editor_undoes_is_told_rather_than_done() {
        let mut s = Stripper::default();
        // A shell echoing `s`, then backspacing and writing the line again.
        assert_eq!(s.strip(b"s\x08sleep 2"), "ssleep 2", "no terminal here: both are kept");
        assert_eq!(
            s.take_events(),
            vec![(1, Ev::Backspace)],
            "but it says where the backspace fell"
        );
        // A return takes the line away from what is returned, and the echo
        // keeps it — which is the only place the `2` survives.
        assert_eq!(s.strip(b"2\r\r\n"), "\n");
        assert_eq!(s.take_events(), vec![(1, Ev::Return), (1, Ev::Return)]);
        assert_eq!(s.take_echo(), "2\n", "the echo still has the `2` the return took away");
        assert_eq!(s.take_echo(), "", "draining: read once");
    }

    #[test]
    fn the_box_drawing_set_is_drawn_rather_than_spelled() {
        // A prompt ruling a line off: choose the set, write `q`s, put ASCII
        // back. Read as letters, those are the `qqqqqq` in a shell's row.
        assert_eq!(strip(b"\x1b(0qqq\x1b(Bdone"), "───done");
        // Outside the set the same letters are only letters.
        assert_eq!(strip(b"qqq"), "qqq");
        // The set holds across reads, like every other state here.
        let mut s = Stripper::default();
        assert_eq!(s.strip(b"\x1b(0lq"), "┌─");
        assert_eq!(s.strip(b"k\x1b(Bx"), "┐x");
        // `ESC ) 0` designates G1 without drawing from it: that takes a shift.
        assert_eq!(strip(b"\x1b)0qq"), "qq");
        // Which is how a real prompt rules its line, taken from a session's
        // own journal: designate G1, shift out, `q`s, shift back.
        assert_eq!(strip(b"\x1b)0\x0eqq\x0fq"), "──q");
    }

    #[test]
    fn strips_colour_and_cursor_codes() {
        assert_eq!(strip(b"\x1b[32mok\x1b[0m done\n"), "ok done\n");
        assert_eq!(strip(b"\x1b]0;title\x07real"), "real");
        assert_eq!(strip(b"a\x1b[2K\x1b[1Gb"), "ab");
    }

    #[test]
    fn stripper_resumes_a_csi_split_across_two_chunks() {
        let mut s = Stripper::default();
        assert_eq!(s.strip(b"\x1b["), "");
        assert_eq!(s.strip(b"32mready\n"), "ready\n");
    }

    #[test]
    fn stripper_gives_up_on_a_never_terminated_escape() {
        let mut s = Stripper::default();
        assert_eq!(s.strip(b"\x1b["), "");
        // Way past MAX_PENDING_ESCAPE bytes with no final byte: the escape
        // is abandoned (never buffered forever) and normal text right after
        // it is still read correctly.
        let junk = "1".repeat(MAX_PENDING_ESCAPE + 10);
        s.strip(junk.as_bytes());
        assert_eq!(s.strip(b"hi\n"), "hi\n");
    }

    #[test]
    fn carriage_return_restarts_the_line() {
        assert_eq!(strip(b"10%\r50%\r100%\n"), "100%\n");
        assert_eq!(strip(b"line\r\nnext"), "line\nnext");
    }

    #[test]
    fn last_line_skips_blanks() {
        let t = strip(b"one\n\x1b[1mtwo\x1b[0m\n   \n\n");
        assert_eq!(last_line(&t, 80).as_deref(), Some("two"));
        assert_eq!(last_line("", 80), None);
        assert_eq!(last_line("abcdef", 3).as_deref(), Some("abc"));
        assert_eq!(last_lines("a\n\nb\nc\n", 2), ["c", "b"]);
    }
}
