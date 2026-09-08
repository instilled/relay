//! Key events back into the bytes a terminal would have sent. The TUI reads
//! parsed events (it has to, for its own keys); the run wants raw input.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// `app_cursor`: the screen is in application cursor mode (DECCKM), where
/// arrows are `ESC O x` rather than `ESC [ x`.
pub fn encode(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let mut out = Vec::new();
    if alt {
        out.push(0x1b);
    }
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                match c.to_ascii_lowercase() {
                    c @ 'a'..='z' => out.push(c as u8 & 0x1f),
                    '[' | '3' => out.push(0x1b),
                    '\\' | '4' => out.push(0x1c),
                    ']' | '5' => out.push(0x1d),
                    '^' | '6' => out.push(0x1e),
                    '_' | '7' | '/' => out.push(0x1f),
                    ' ' | '2' | '@' => out.push(0),
                    _ => return None,
                }
            } else {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => out.push(0x7f),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Up | KeyCode::Down | KeyCode::Right | KeyCode::Left => {
            let l = match key.code {
                KeyCode::Up => b'A',
                KeyCode::Down => b'B',
                KeyCode::Right => b'C',
                _ => b'D',
            };
            if ctrl || shift || alt {
                let m = 1 + (shift as u8) + (alt as u8) * 2 + (ctrl as u8) * 4;
                out.clear();
                out.extend_from_slice(format!("\x1b[1;{m}{}", l as char).as_bytes());
            } else if app_cursor {
                out.extend_from_slice(&[0x1b, b'O', l]);
            } else {
                out.extend_from_slice(&[0x1b, b'[', l]);
            }
        }
        KeyCode::Home => out.extend_from_slice(if app_cursor { b"\x1bOH" } else { b"\x1b[H" }),
        KeyCode::End => out.extend_from_slice(if app_cursor { b"\x1bOF" } else { b"\x1b[F" }),
        KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
        KeyCode::Insert => out.extend_from_slice(b"\x1b[2~"),
        KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
        KeyCode::F(n) => out.extend_from_slice(match n {
            1 => b"\x1bOP".as_slice(),
            2 => b"\x1bOQ",
            3 => b"\x1bOR",
            4 => b"\x1bOS",
            5 => b"\x1b[15~",
            6 => b"\x1b[17~",
            7 => b"\x1b[18~",
            8 => b"\x1b[19~",
            9 => b"\x1b[20~",
            10 => b"\x1b[21~",
            11 => b"\x1b[23~",
            12 => b"\x1b[24~",
            _ => return None,
        }),
        _ => return None,
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn plain_control_and_arrows() {
        assert_eq!(encode(&k(KeyCode::Char('y'), KeyModifiers::NONE), false).unwrap(), b"y");
        assert_eq!(
            encode(&k(KeyCode::Char('é'), KeyModifiers::NONE), false).unwrap(),
            "é".as_bytes()
        );
        assert_eq!(
            encode(&k(KeyCode::Char('c'), KeyModifiers::CONTROL), false).unwrap(),
            [3]
        );
        assert_eq!(
            encode(&k(KeyCode::Char('\\'), KeyModifiers::CONTROL), false).unwrap(),
            [0x1c]
        );
        assert_eq!(encode(&k(KeyCode::Enter, KeyModifiers::NONE), false).unwrap(), b"\r");
        assert_eq!(encode(&k(KeyCode::Esc, KeyModifiers::NONE), false).unwrap(), [0x1b]);
        assert_eq!(encode(&k(KeyCode::Up, KeyModifiers::NONE), false).unwrap(), b"\x1b[A");
        assert_eq!(encode(&k(KeyCode::Up, KeyModifiers::NONE), true).unwrap(), b"\x1bOA");
        assert_eq!(
            encode(&k(KeyCode::Left, KeyModifiers::SHIFT), false).unwrap(),
            b"\x1b[1;2D"
        );
        assert_eq!(
            encode(&k(KeyCode::Char('x'), KeyModifiers::ALT), false).unwrap(),
            b"\x1bx"
        );
        assert_eq!(
            encode(&k(KeyCode::F(5), KeyModifiers::NONE), false).unwrap(),
            b"\x1b[15~"
        );
        assert!(encode(&k(KeyCode::CapsLock, KeyModifiers::NONE), false).is_none());
    }
}
