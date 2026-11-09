//! `rl attach`: this terminal becomes a session's terminal.
//!
//! [`held`] attaches straight to a Relay holder's socket; [`native`] runs a
//! harness's own attach command on a local pty. Keys are bytes, never parsed:
//! what you type is what the session gets. `ctrl-\` detaches from both, and
//! the session never notices. Nothing of ours is drawn over the session's
//! screen: what we have to say goes in the window title, which it cannot
//! paint over.

pub mod held;
pub mod native;

use anyhow::Result;

/// `ctrl-\`: detach. Nothing interactive we care about uses it.
pub(crate) const DETACH: u8 = 0x1c;

/// How an attach came to an end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// `ctrl-\`: the session keeps going.
    Detached,
    /// Input ran out on a piped attach.
    Left,
    /// The session ended: its code, and how in words.
    Exit { code: i32, how: String },
    /// The connection went without an exit and none was recorded.
    Closed,
}

impl Ended {
    /// The process exit code. Leaving on purpose is 0; a session that ended is
    /// its own code; a connection that went without one is 1, never 0, which
    /// would read as having seen a session finish cleanly.
    pub fn code(&self) -> i32 {
        match self {
            Ended::Exit { code, .. } => *code,
            Ended::Closed => 1,
            Ended::Detached | Ended::Left => 0,
        }
    }
}

/// Splits typed bytes at the first `ctrl-\`: what came before it (to send),
/// and whether a detach was asked for.
pub(crate) fn split_detach(bytes: &[u8]) -> (&[u8], bool) {
    match bytes.iter().position(|&b| b == DETACH) {
        Some(at) => (&bytes[..at], true),
        None => (bytes, false),
    }
}

/// Whether the stream has left the terminal on the alternate screen: the
/// last switch it carried, of `?1049`, `?1047` and `?47`. A screen drawn
/// for an app that is on it switches the viewer there too, and leaving has
/// to switch back; leaving with `?1049l` regardless would restore a cursor
/// never saved on a terminal that was never there. A switch cut across two
/// chunks is carried over.
#[derive(Debug, Default)]
pub struct AltScreen {
    on: bool,
    /// An escape begun and not yet ended: the bytes after its ESC.
    seq: Option<Vec<u8>>,
}

impl AltScreen {
    /// Longer than any switch; a sequence that runs past it is not one.
    const LONGEST: usize = 32;

    pub fn watch(&mut self, b: &[u8]) {
        for &c in b {
            if c == 0x1b {
                self.seq = Some(Vec::new());
                continue;
            }
            let Some(seq) = self.seq.as_mut() else { continue };
            seq.push(c);
            let ongoing = match (seq.len(), c) {
                (1, b'[') | (2, b'?') => true,
                (n, b'0'..=b'9' | b';') if n > 2 => true,
                (n, b'h' | b'l') if n > 2 => {
                    let modes = &seq[2..seq.len() - 1];
                    if modes
                        .split(|&m| m == b';')
                        .any(|m| matches!(m, b"1049" | b"1047" | b"47"))
                    {
                        self.on = c == b'h';
                    }
                    false
                }
                _ => false,
            };
            if !ongoing || seq.len() > Self::LONGEST {
                self.seq = None;
            }
        }
    }

    /// What hands the terminal back as we found it: input modes the session's
    /// screen may have turned on (mouse reporting, bracketed paste, the
    /// application cursor and keypad, a hidden cursor, colours) all off, and
    /// the main screen again if the stream left it on the other.
    pub fn leave(&self) -> Vec<u8> {
        let mut out = if self.on { b"\x1b[?1049l".to_vec() } else { Vec::new() };
        out.extend_from_slice(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1l\x1b>\x1b[?25h\x1b[m");
        out
    }
}

/// The terminal's window title: status without touching the screen. Empty
/// hands the title back to the terminal.
pub(crate) fn title(s: &str) -> String {
    format!("\x1b]0;{s}\x07")
}

/// A line of our own, distinguishable from the session's output. Starts on a
/// fresh line whatever state the cursor was in.
pub(crate) fn dim(s: &str) -> String {
    format!("\r\n\x1b[2m{s}\x1b[0m\r\n")
}

/// Aborts a spawned task when dropped, so it never outlives its owner. The
/// stdin-forwarding task is parked in a blocking read that `abort()` can't
/// preempt mid-syscall, so this is best-effort: it stops the task from being
/// polled (and thus from ever sending again) as soon as the attach returns,
/// closing the window where a stale reader could steal a byte meant for the
/// next `EventStream` — but on some platforms the underlying blocking thread
/// may still be parked in the OS read a moment longer.
pub(crate) struct AbortOnDrop<T>(pub(crate) tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The signals that mean "stop": a kill (SIGTERM), a closed terminal (SIGHUP)
/// and a SIGINT from outside (in raw mode ctrl+c is a key, never this).
/// Listening starts at `new`, and a signal that came since is kept for `recv`.
pub struct Stop {
    term: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
}

impl Stop {
    pub fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            term: signal(SignalKind::terminate())?,
            hup: signal(SignalKind::hangup())?,
            int: signal(SignalKind::interrupt())?,
        })
    }

    /// Resolves when one of them comes; cancel-safe.
    pub async fn recv(&mut self) {
        tokio::select! {
            _ = self.term.recv() => {}
            _ = self.hup.recv() => {}
            _ = self.int.recv() => {}
        }
    }

    /// Whether one has come since the last `recv`, without waiting.
    pub fn came(&mut self) -> bool {
        use futures::FutureExt;
        self.recv().now_or_never().is_some()
    }
}

pub(crate) struct RawMode;

impl RawMode {
    pub(crate) fn enable() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Leaving always turns the input modes off, and leaves the alternate
    /// screen only when the stream's last switch put the viewer there.
    #[test]
    fn leaving_hands_back_the_screen_the_stream_left_us_on() {
        const MODES: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1l\x1b>\x1b[?25h\x1b[m";
        let mut a = AltScreen::default();
        assert_eq!(a.leave(), MODES, "never there: no ?1049l");
        a.watch(b"text\x1b[1;31mred\x1b[?1049h\x1b[Hfull");
        assert_eq!(a.leave(), [b"\x1b[?1049l".as_slice(), MODES].concat());
        a.watch(b"\x1b[?1049l");
        assert_eq!(a.leave(), MODES);
        // A switch cut across chunks, anywhere in it, is still seen.
        for cut in 1..b"\x1b[?1049h".len() {
            let mut a = AltScreen::default();
            let (x, y) = b"\x1b[?1049h".split_at(cut);
            a.watch(x);
            a.watch(y);
            assert!(a.on, "cut at {cut}");
        }
        // The older switches, and one among other modes.
        let mut a = AltScreen::default();
        a.watch(b"\x1b[?47h");
        assert!(a.on);
        a.watch(b"\x1b[?1047l");
        assert!(!a.on);
        a.watch(b"\x1b[?25;1049h");
        assert!(a.on);
        // Not a switch: another private mode, a plain CSI with the digits.
        let mut a = AltScreen::default();
        a.watch(b"\x1b[?2004h\x1b[1049h\x1b[?10490h");
        assert!(!a.on);
    }

    /// Leaving on purpose is the only thing that may exit 0.
    #[test]
    fn only_leaving_on_purpose_is_a_clean_exit() {
        assert_eq!(Ended::Detached.code(), 0);
        assert_eq!(Ended::Left.code(), 0);
        assert_eq!(
            Ended::Exit {
                code: 3,
                how: "exit 3".into()
            }
            .code(),
            3
        );
        assert_eq!(Ended::Closed.code(), 1);
    }

    #[test]
    fn bytes_before_ctrl_backslash_are_sent_and_it_ends() {
        assert_eq!(split_detach(b"abc"), (b"abc".as_slice(), false));
        assert_eq!(split_detach(b"ab\x1ccd"), (b"ab".as_slice(), true));
        assert_eq!(split_detach(b"\x1c"), (b"".as_slice(), true));
        assert_eq!(split_detach(b""), (b"".as_slice(), false));
    }
}
