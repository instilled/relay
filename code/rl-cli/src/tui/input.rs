//! What the terminal sends us that nobody asked for yet.
//!
//! A run we let the terminal to can ask it questions — which modes it speaks,
//! whether it synchronises output — and the answers come back as input. One
//! that arrives after we take the terminal back lands in our key reader, and
//! a reply the reader only half knows used to start a sequence that never
//! ended: every key after it went into that sequence, and the panel drew on
//! and heard nothing. The reader is fixed for that (`vendor/crossterm`); this
//! drops what is already waiting, so there is less of it to read at all.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::os::fd::AsFd;

/// Throws away input the terminal has queued and nobody has read. Not a
/// terminal: nothing to do.
pub(crate) fn discard_pending(fd: impl AsFd) {
    let _ = nix::sys::termios::tcflush(fd, nix::sys::termios::FlushArg::TCIFLUSH);
}

/// A key as it was typed. With every key reported as an escape code, a
/// shifted key comes as its unshifted one plus shift — `/` for `?`, `h` for
/// `H` — unless the terminal also sends what shift made of it, which the panel
/// asks for and most do. Where one does not, a letter is still put right;
/// what shift makes of anything else depends on the keyboard, so it is left.
pub(crate) fn typed(mut k: KeyEvent) -> KeyEvent {
    if k.modifiers == KeyModifiers::SHIFT
        && let KeyCode::Char(c) = k.code
        && c.is_lowercase()
        && let Some(up) = c.to_uppercase().next()
    {
        k.code = KeyCode::Char(up);
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode};
    use nix::errno::Errno;
    use nix::pty::openpty;
    use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};
    use std::io::{Read, Write};
    use std::os::fd::OwnedFd;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// A pty in raw mode, as the panel keeps its terminal: bytes reach the
    /// reader as they are written, not a line at a time.
    fn raw_pty() -> (std::fs::File, OwnedFd) {
        let pty = openpty(None, None).unwrap();
        let mut t = tcgetattr(&pty.slave).unwrap();
        cfmakeraw(&mut t);
        tcsetattr(&pty.slave, SetArg::TCSANOW, &t).unwrap();
        (std::fs::File::from(pty.master), pty.slave)
    }

    #[test]
    fn what_the_terminal_already_sent_is_dropped() {
        let (mut master, slave) = raw_pty();
        master.write_all(b"\x1b[?2026;2$y").unwrap();
        discard_pending(&slave);

        let fl = nix::fcntl::fcntl(&slave, nix::fcntl::F_GETFL).unwrap();
        let fl = nix::fcntl::OFlag::from_bits_truncate(fl) | nix::fcntl::OFlag::O_NONBLOCK;
        nix::fcntl::fcntl(&slave, nix::fcntl::F_SETFL(fl)).unwrap();
        let mut buf = [0u8; 64];
        assert_eq!(nix::unistd::read(&slave, &mut buf), Err(Errno::EAGAIN), "nothing left");

        // And what comes after is read as ever.
        master.write_all(b"x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match nix::unistd::read(&slave, &mut buf) {
                Ok(n) => break assert_eq!(&buf[..n], b"x"),
                Err(Errno::EAGAIN) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => panic!("reading after the flush: {e}"),
            }
        }
    }

    /// Set in the child that [`a_mode_report_does_not_take_the_keys_after_it`]
    /// runs: this test binary again, with a pty for stdin.
    const KEY_READER: &str = "RL_TEST_KEY_READER";

    /// The child: reads keys with crossterm, as the panel does, and prints
    /// each, until a `q`.
    #[test]
    fn key_reader_child() {
        if std::env::var_os(KEY_READER).is_none() {
            return;
        }
        loop {
            match crossterm::event::read() {
                Ok(Event::Key(k)) => {
                    println!("key {:?}", typed(k).code);
                    if k.code == KeyCode::Char('q') {
                        break;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    println!("error {e}");
                    break;
                }
            }
        }
    }

    /// The reader itself, over a real terminal. `CSI ? 2026 ; 2 $ y` is what a
    /// terminal answers a run asking whether it synchronises output; crossterm
    /// 0.29 as released waits on after it for a `u` or a `c`, and takes the
    /// `b` and the `q` into it.
    #[test]
    fn a_mode_report_does_not_take_the_keys_after_it() {
        let (mut master, slave) = raw_pty();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tui::input::tests::key_reader_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(KEY_READER, "1")
            .stdin(Stdio::from(slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        master.write_all(b"\x1b[?2026;2$y").unwrap();
        master.write_all(b"b").unwrap();
        master.write_all(b"q").unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let finished = loop {
            if child.try_wait().unwrap().is_some() {
                break true;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut out = String::new();
        child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        assert!(finished, "the reader never saw the q; it printed:\n{out}");
        assert!(
            out.contains("key Char('b')"),
            "the b went missing; the reader printed:\n{out}"
        );
    }

    /// Shift, where every key is an escape code: what the terminal says shift
    /// made of the key is taken (`?` from shift and `/`), and a letter comes
    /// out a capital even from a terminal that does not say.
    #[test]
    fn a_shifted_key_is_what_was_typed() {
        let (mut master, slave) = raw_pty();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tui::input::tests::key_reader_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(KEY_READER, "1")
            .stdin(Stdio::from(slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // shift-/ saying `?`, shift-h saying `H`, shift-h saying nothing more.
        master.write_all(b"\x1b[47:63;2u\x1b[104:72;2u\x1b[104;2u").unwrap();
        master.write_all(b"q").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.wait();
        let mut out = String::new();
        child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        // The first shares its line with the harness's own `test … ...`.
        let keys: Vec<&str> = out.lines().filter_map(|l| l.find("key ").map(|i| &l[i..])).collect();
        assert_eq!(
            keys,
            ["key Char('?')", "key Char('H')", "key Char('H')", "key Char('q')"],
            "{out}"
        );
    }

    #[test]
    fn a_letter_with_shift_is_a_capital_and_nothing_else_changes() {
        let k = |c, m| KeyEvent::new(KeyCode::Char(c), m);
        assert_eq!(typed(k('h', KeyModifiers::SHIFT)).code, KeyCode::Char('H'));
        assert_eq!(typed(k('é', KeyModifiers::SHIFT)).code, KeyCode::Char('É'));
        // What shift makes of `/` depends on the keyboard.
        assert_eq!(typed(k('/', KeyModifiers::SHIFT)).code, KeyCode::Char('/'));
        // ctrl-shift-h is a chord, not a capital.
        let chord = KeyModifiers::SHIFT | KeyModifiers::CONTROL;
        assert_eq!(typed(k('h', chord)).code, KeyCode::Char('h'));
        assert_eq!(typed(k('h', KeyModifiers::NONE)).code, KeyCode::Char('h'));
    }
}
