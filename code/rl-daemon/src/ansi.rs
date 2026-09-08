//! Just enough terminal-output cleanup to pick a "last meaningful line" for a
//! fleet row. Not a terminal emulator.

/// Strips CSI/OSC/other escape sequences and resolves `\r` within a line.
pub fn strip(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: ESC [ params… final byte 0x40..=0x7E
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] … BEL or ESC \
                Some(']') => {
                    let mut prev = '\0';
                    for n in chars.by_ref() {
                        if n == '\x07' || (prev == '\x1b' && n == '\\') {
                            break;
                        }
                        prev = n;
                    }
                }
                // Two-byte escapes (ESC 7, ESC =, charset selects with one more byte…)
                Some('(') | Some(')') => {
                    chars.next();
                }
                Some(_) | None => {}
            },
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
        }
    }
    out
}

/// Last non-blank line of `text`, trimmed and capped at `max` chars.
pub fn last_line(text: &str, max: usize) -> Option<String> {
    let line = text.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(max).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colour_and_cursor_codes() {
        assert_eq!(strip(b"\x1b[32mok\x1b[0m done\n"), "ok done\n");
        assert_eq!(strip(b"\x1b]0;title\x07real"), "real");
        assert_eq!(strip(b"a\x1b[2K\x1b[1Gb"), "ab");
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
    }
}
