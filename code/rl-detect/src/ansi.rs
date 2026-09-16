//! Just enough terminal-output cleanup to read the last few lines of a run.
//! Not a terminal emulator: a full-screen TUI's bottom line is not always the
//! last thing it wrote, which is why heuristics built on this stay `likely`
//! at best.

/// Escape-parsing state carried across calls to [`Stripper::strip`] so a
/// CSI/OSC sequence split across two PTY reads is still recognized, instead
/// of the swallowed tail of one chunk (digits, a final letter) leaking into
/// the next chunk's visible text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum EscState {
    #[default]
    None,
    /// Saw ESC; waiting for the next byte to say what kind of escape this is.
    Escape,
    /// Inside `ESC [ … final`, waiting for a final byte in 0x40..=0x7E.
    Csi,
    /// Inside `ESC ] … BEL` or `… ESC \`, waiting for the terminator.
    Osc { prev_was_esc: bool },
    /// Saw `ESC (` or `ESC )`; waiting for exactly one more byte.
    Charset,
}

/// An escape sequence still unterminated after this many bytes is treated as
/// malformed and abandoned, so a stray, never-terminated ESC doesn't buffer
/// forever.
const MAX_PENDING_ESCAPE: usize = 64;

/// Stateful ANSI stripper: like [`strip`], but remembers an escape sequence
/// left open at the end of one chunk so the next call resumes it correctly
/// instead of re-parsing its tail as literal text. Use one `Stripper` per
/// stream (e.g. per PTY) and feed it consecutive chunks.
#[derive(Debug, Clone, Default)]
pub struct Stripper {
    state: EscState,
    pending: usize,
}

impl Stripper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Strips CSI/OSC/other escape sequences and resolves `\r` within a
    /// line, resuming any escape sequence left open by the previous call.
    pub fn strip(&mut self, bytes: &[u8]) -> String {
        let text = String::from_utf8_lossy(bytes);
        let mut out = String::with_capacity(text.len());
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
                        if chars.peek() != Some(&'\n') {
                            if let Some(pos) = out.rfind('\n') {
                                out.truncate(pos + 1);
                            } else {
                                out.clear();
                            }
                        }
                    }
                    '\x07' | '\x08' => {}
                    c if c.is_control() && c != '\n' && c != '\t' => {}
                    c => out.push(c),
                },
                // CSI: ESC [ params… final byte 0x40..=0x7E
                // OSC: ESC ] … BEL or ESC \
                // Two-byte escapes (ESC 7, ESC =, charset selects with one more byte…)
                EscState::Escape => {
                    self.pending += 1;
                    self.state = match c {
                        '[' => EscState::Csi,
                        ']' => EscState::Osc { prev_was_esc: false },
                        '(' | ')' => EscState::Charset,
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
                    self.state = if c == '\x07' || (prev_was_esc && c == '\\') {
                        EscState::None
                    } else {
                        EscState::Osc { prev_was_esc: c == '\x1b' }
                    };
                }
                EscState::Charset => {
                    self.pending += 1;
                    self.state = EscState::None;
                }
            }
            if self.state != EscState::None && self.pending > MAX_PENDING_ESCAPE {
                // Give up: not a well-formed escape after all.
                self.state = EscState::None;
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

    #[test]
    fn last_line_skips_rules_and_borders() {
        assert_eq!(
            last_line("hello\n──────⏵⏵ auto mode on ──\n────────\n", 80).as_deref(),
            Some("⏵⏵ auto mode on")
        );
        assert_eq!(last_line("│ > │\n", 80).as_deref(), Some(">"));
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
