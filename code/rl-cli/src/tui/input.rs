//! What the terminal sends us that nobody asked for yet.
//!
//! A run we let the terminal to can ask it questions — which modes it speaks,
//! whether it synchronises output — and the answers come back as input. One
//! that arrives after we take the terminal back lands in our key reader, and
//! a reply the reader only half knows used to start a sequence that never
//! ended: every key after it went into that sequence, and the panel drew on
//! and heard nothing. The reader is fixed for that (`vendor/crossterm`); this
//! drops what is already waiting, so there is less of it to read at all.

use std::os::fd::AsFd;

/// Throws away input the terminal has queued and nobody has read. Not a
/// terminal: nothing to do.
pub(crate) fn discard_pending(fd: impl AsFd) {
    let _ = nix::sys::termios::tcflush(fd, nix::sys::termios::FlushArg::TCIFLUSH);
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
                    println!("key {:?}", k.code);
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
}
