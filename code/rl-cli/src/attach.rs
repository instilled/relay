//! `rl watch` / `rl join`: this terminal becomes the run's terminal.
//!
//! One `run.attach` call, then the connection carries holder frames. On a
//! real terminal we go raw and forward resizes; piped, we just stream bytes
//! (so `rl watch <id> | tee log` works). Keys are bytes, never parsed: what
//! you type is what the child gets. `ctrl-\` detaches a join; `q` leaves a
//! watch. The run never notices either.
//!
//! Nothing of ours is ever drawn over the run's screen. What we have to say
//! while it has the terminal — how to let go, how many other runs need you,
//! a note from rld — goes in the window title, which the run cannot paint over.

use crate::client::Client;
use anyhow::{Context, Result};
use rl_core::Home;
use rl_proto::methods::{AttachParams, DecisionFilter, DecisionList, RunAttach, RunFilter, RunGet, RunId, RunList};
use rl_proto::types::{AttachMode, Run, RunState};
use rl_session::proto::{Frame, FrameReader, write_frame};
use std::io::IsTerminal;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// How long to let rld notice that a holder has gone, before saying what
/// became of a run whose stream was cut off.
const SETTLING: Duration = Duration::from_millis(1500);

/// A run on pipes ends its lines with `\n` alone, which a terminal takes as
/// "down one" and not "back to the start": its output would climb down the
/// screen as a staircase. On its way to a terminal each bare `\n` becomes
/// `\r\n`. A pty's output already says `\r\n` and passes as it is.
#[derive(Debug, Default)]
pub struct Newlines {
    /// The last byte seen ended in `\r`, so a `\n` that opens the next chunk
    /// is not bare.
    after_cr: bool,
}

impl Newlines {
    pub fn fix(&mut self, b: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(b.len() + b.len() / 16);
        for &c in b {
            if c == b'\n' && !self.after_cr {
                out.push(b'\r');
            }
            out.push(c);
            self.after_cr = c == b'\r';
        }
        out
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

    /// What hands the terminal back as we found it: input modes the run's
    /// screen may have turned on (mouse reporting, bracketed paste, the
    /// application cursor and keypad, a hidden cursor, colours) all off, and
    /// the main screen again if the stream left it on the other.
    pub fn leave(&self) -> Vec<u8> {
        let mut out = if self.on { b"\x1b[?1049l".to_vec() } else { Vec::new() };
        out.extend_from_slice(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1l\x1b>\x1b[?25h\x1b[m");
        out
    }
}

/// `ctrl-\`: detach. Nothing interactive we care about uses it.
pub(crate) const DETACH: u8 = 0x1c;
const CTRL_C: u8 = 0x03;

/// How often a joined terminal asks how many other runs need you.
const NEED_EVERY: Duration = Duration::from_secs(5);

/// How an attach came to an end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// `ctrl-\` on a join: the run keeps going.
    Detached,
    /// `q` on a watch, or a piped join whose input ran out.
    Left,
    /// The run ended while we were there: its code, and how in words.
    Exit { code: i32, how: String },
    /// The connection went without an exit: what rld says became of the run,
    /// and its code if it has one. `None` when rld could not be asked.
    Closed { state: Option<RunState>, code: Option<i32> },
}

impl Ended {
    /// The process exit code. Leaving on purpose is 0; a run that ended is
    /// its own code; a connection that went without one is the run's code if
    /// rld knows it, and 1 otherwise — never 0, which would read as having
    /// watched a run finish cleanly.
    pub fn code(&self) -> i32 {
        match self {
            Ended::Exit { code, .. } => *code,
            Ended::Closed { code, .. } => code.filter(|c| *c != 0).unwrap_or(1),
            Ended::Detached | Ended::Left => 0,
        }
    }
}

pub struct Opts {
    pub id: String,
    pub mode: AttachMode,
    pub take: bool,
    /// Bytes.
    pub scrollback: u64,
}

/// Runs until detached or the run ends, and says which.
pub async fn attach(home: &Home, o: Opts) -> Result<Ended> {
    let me = client_name();
    let mut c = Client::connect(home).await?;
    let info = c
        .call::<RunAttach>(AttachParams {
            id: o.id.clone(),
            mode: o.mode,
            client: me.clone(),
            scrollback: o.scrollback,
            take: o.take,
            terminal: Some(terminal()),
        })
        .await?;
    let (rd, mut wr) = c.into_parts();
    // The screen or scrollback arrives in output frames of up to a mebibyte,
    // and this read shares a `select!` with stdin and the "who needs you"
    // ticker. Read it cancel-safely or a lost race eats the connection.
    let mut rd = FrameReader::new(rd);
    let joined = o.mode == AttachMode::Join && info.live;
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();

    let mut out = tokio::io::stdout();
    let mut banner = format!("[relay] {} {} · {}", o.mode, info.run.id, info.run.title);
    if !info.live {
        banner.push_str(" · ended");
    } else if joined {
        banner.push_str(" · ctrl-\\ to detach");
    } else {
        banner.push_str(" · q to leave");
        if let Some(j) = &info.joiner {
            banner.push_str(&format!(" · joined by {j}"));
        }
    }
    // On a terminal the banner is one ordinary line above the session and
    // the window title carries the status from then on; nothing of ours is
    // ever drawn over the run's own screen. Piped, it is all plain lines.
    let mut status = Status {
        base: format!("relay · {} {}", o.mode, info.run.id),
        key: if joined {
            "^\\ lets go"
        } else if info.live {
            "q leaves"
        } else {
            ""
        },
        need: 0,
        note: None,
    };
    if interactive {
        out.write_all(format!("\x1b[2m{banner}\x1b[0m\n{}", title(&status.line())).as_bytes())
            .await?;
    } else {
        out.write_all(dim(&banner).as_bytes()).await?;
    }
    out.flush().await?;

    let _raw = if interactive {
        Some(RawMode::enable().context("entering raw mode")?)
    } else {
        None
    };
    if joined
        && interactive
        && let Ok((cols, rows)) = crossterm::terminal::size()
    {
        write_frame(&mut wr, &Frame::Resize { cols, rows }).await?;
    }

    let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(64);
    // Aborted on every exit from this function (see `AbortOnDrop`), so the
    // reader never outlives `attach()` and races the next one for stdin.
    let _stdin_task = AbortOnDrop(tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdin_tx.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
            }
        }
    }));
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let mut stdin_open = true;
    // Piped on, the bytes go as they came; to a terminal, a pipes run's lines
    // need their carriage returns.
    let mut newlines = (interactive && !info.tty).then(Newlines::default);
    // Only a terminal has a title to say it in; a second connection asks.
    let mut asker = if interactive && info.live {
        Client::connect(home).await.ok()
    } else {
        None
    };
    let mut every = tokio::time::interval(NEED_EVERY);
    let mut alt = AltScreen::default();

    // Each way out says how, and in which words; the terminal is handed back
    // below, the same way for all of them.
    let ended: Result<(Ended, Option<String>)> = async {
        loop {
            tokio::select! {
                frame = rd.next() => match frame? {
                    Some(Frame::Output(b)) => {
                        alt.watch(&b);
                        match newlines.as_mut() {
                            Some(n) => out.write_all(&n.fix(&b)).await?,
                            None => out.write_all(&b).await?,
                        }
                        out.flush().await?;
                    }
                    Some(Frame::Note(s)) => {
                        if interactive {
                            status.note = Some(s);
                            out.write_all(title(&status.line()).as_bytes()).await?;
                        } else {
                            out.write_all(dim(&format!("[relay] {s}")).as_bytes()).await?;
                        }
                        out.flush().await?;
                    }
                    Some(Frame::Exit(e)) => {
                        let how = match &e.signal {
                            Some(s) => format!("{s}, exit {}", e.code),
                            None => format!("exit {}", e.code),
                        };
                        let words = format!("[relay] run {} ended ({how})", info.run.id);
                        return Ok((Ended::Exit { code: e.code, how }, Some(words)));
                    }
                    Some(_) => {}
                    None => {
                        // The stream went without an Exit frame. What that means
                        // is rld's to say: the run may have failed, or may be
                        // going on without us. "connection closed" and exit 0
                        // told a script it had watched a clean ending.
                        let became = what_became(home, asker.as_mut(), &o.id).await;
                        let (state, code) = match &became {
                            Some(r) => (Some(r.state), r.exit_code),
                            None => (None, None),
                        };
                        let words = format!("[relay] {}", closed_words(&o.id, state));
                        return Ok((Ended::Closed { state, code }, Some(words)));
                    }
                },
                bytes = stdin_rx.recv(), if stdin_open => match bytes {
                    None => {
                        stdin_open = false;
                        if joined {
                            // Nothing more will be typed: a piped join is over.
                            return Ok((Ended::Left, None));
                        }
                    }
                    Some(bytes) => {
                        if joined {
                            if let Some(at) = bytes.iter().position(|&b| b == DETACH) {
                                if at > 0 {
                                    write_frame(&mut wr, &Frame::Input(bytes[..at].to_vec())).await?;
                                }
                                let words = "[relay] detached; the run keeps going".to_string();
                                return Ok((Ended::Detached, Some(words)));
                            }
                            write_frame(&mut wr, &Frame::Input(bytes)).await?;
                        } else if bytes.iter().any(|&b| b == b'q' || b == CTRL_C || b == DETACH) {
                            return Ok((Ended::Left, Some("[relay] left".to_string())));
                        }
                    }
                },
                _ = every.tick(), if asker.is_some() => {
                    let c = asker.as_mut().expect("guarded");
                    match need_you(c, &info.run.id).await {
                        Ok(n) if n != status.need => {
                            status.need = n;
                            out.write_all(title(&status.line()).as_bytes()).await?;
                            out.flush().await?;
                        }
                        Ok(_) => {}
                        // rld is going; its own note says so. Stop asking.
                        Err(_) => asker = None,
                    }
                }
                _ = winch.recv() => {
                    if joined && let Ok((cols, rows)) = crossterm::terminal::size() {
                        write_frame(&mut wr, &Frame::Resize { cols, rows }).await?;
                    }
                }
            }
        }
    }
    .await;

    let back = interactive.then(|| alt.leave());
    match ended {
        Ok((ended, words)) => {
            leave(&mut out, back.as_deref(), words.as_deref()).await?;
            Ok(ended)
        }
        Err(e) => {
            // Gone wrong, the terminal is still handed back, as far as it
            // can be; the error is what is said.
            let _ = leave(&mut out, back.as_deref(), None).await;
            Err(e)
        }
    }
}

/// The last thing written: the terminal handed back (`back`, on a terminal),
/// then our words on a line of their own and the window title given back.
/// The words come after, so they land on the screen the viewer is left on.
async fn leave(out: &mut tokio::io::Stdout, back: Option<&[u8]>, words: Option<&str>) -> std::io::Result<()> {
    if let Some(b) = back {
        out.write_all(b).await?;
    }
    if let Some(w) = words {
        out.write_all(dim(w).as_bytes()).await?;
        out.write_all(title("").as_bytes()).await?;
    }
    out.flush().await
}

/// What the window title says while a run has the terminal.
struct Status {
    /// `relay · join <id>`.
    base: String,
    /// How to get out: `^\ lets go`, `q leaves`.
    key: &'static str,
    /// Other runs waiting on you.
    need: usize,
    /// rld's latest word: who joined, who took over.
    note: Option<String>,
}

impl Status {
    fn line(&self) -> String {
        let mut parts = vec![self.base.clone()];
        if !self.key.is_empty() {
            parts.push(self.key.to_string());
        }
        if self.need > 0 {
            parts.push(format!("{} need you", self.need));
        }
        if let Some(n) = &self.note {
            parts.push(n.clone());
        }
        parts.join(" · ")
    }
}

/// How many other runs wait on you: open questions, and runs that need
/// input or are blocked without one — each counted once, this run left out.
/// What rld says became of the run, over whichever connection is at hand.
///
/// The stream usually goes because the holder did, and rld is finding that
/// out at the same moment: asked straight away it would still say "working",
/// and `rl ls` would say "failed" a second later. So give it a moment to
/// settle — but only a moment, and then say honestly that the run may still
/// be going, which for a daemon that merely restarted is the truth.
async fn what_became(home: &Home, asker: Option<&mut Client>, id: &str) -> Option<Run> {
    let mut own;
    let c = match asker {
        Some(c) => c,
        None => {
            own = Client::connect(home).await.ok()?;
            &mut own
        }
    };
    let deadline = tokio::time::Instant::now() + SETTLING;
    loop {
        let run = c.call::<RunGet>(RunId { id: id.to_string() }).await.ok()?;
        if run.state.is_terminal() || tokio::time::Instant::now() >= deadline {
            return Some(run);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// What to say when the stream went without an exit. Never "connection
/// closed" alone: that is the one thing the person already knows, and it
/// reads like nothing went wrong.
fn closed_words(id: &str, state: Option<RunState>) -> String {
    match state {
        Some(RunState::Failed) => format!("run {id} failed · the session holder went away"),
        Some(RunState::Finished) => format!("run {id} finished · the connection went with it"),
        Some(RunState::Stopped) => format!("run {id} was stopped"),
        Some(s) => format!("lost touch with run {id} · rld says it is {s}"),
        None => format!("lost touch with run {id} · rld could not say what became of it"),
    }
}

async fn need_you(c: &mut Client, this: &str) -> Result<usize> {
    let asked = c.call::<DecisionList>(DecisionFilter::default()).await?;
    let waiting = c
        .call::<RunList>(RunFilter {
            states: vec![RunState::NeedsInput, RunState::Blocked],
            ..Default::default()
        })
        .await?;
    let asked: Vec<Option<&str>> = asked.iter().map(|d| d.run.as_deref()).collect();
    let waiting: Vec<&str> = waiting.iter().map(|r| r.id.as_str()).collect();
    Ok(count_need(&asked, &waiting, this))
}

/// Open questions (by the run each is about, if any) and waiting runs.
fn count_need(asked: &[Option<&str>], waiting: &[&str], this: &str) -> usize {
    let mut runs: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut n = 0;
    for run in asked {
        match *run {
            Some(r) if r == this => {}
            Some(r) => {
                if runs.insert(r) {
                    n += 1;
                }
            }
            None => n += 1,
        }
    }
    for &r in waiting {
        if r != this && runs.insert(r) {
            n += 1;
        }
    }
    n
}

/// `user@host`: how a viewer is known to others and to presence.
pub fn client_name() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "someone".into());
    let host = nix::unistd::gethostname()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "localhost".into());
    let host = host.split('.').next().unwrap_or(&host).to_string();
    format!("{user}@{host}")
}

/// This terminal, as one of its person's: the process attaching. Two
/// terminals of the same person are two seats, so a second `rl join` is
/// refused (or takes over) instead of sharing the keyboard unseen.
pub fn terminal() -> String {
    std::process::id().to_string()
}

/// The terminal's window title: status without touching the screen. Empty
/// hands the title back to the terminal.
pub(crate) fn title(s: &str) -> String {
    format!("\x1b]0;{s}\x07")
}

/// A line of our own, distinguishable from the run's output. Starts on a
/// fresh line whatever state the cursor was in.
fn dim(s: &str) -> String {
    format!("\r\n\x1b[2m{s}\x1b[0m\r\n")
}

/// Aborts a spawned task when dropped, so it never outlives its owner. The
/// stdin-forwarding task in `attach()` is parked in a blocking read that
/// `abort()` can't preempt mid-syscall, so this is best-effort: it stops the
/// task from being polled (and thus from ever sending again) as soon as
/// `attach()` returns, closing the window where a stale reader could steal a
/// byte meant for the next `EventStream` — but on some platforms the
/// underlying blocking thread may still be parked in the OS read a moment
/// longer.
pub(crate) struct AbortOnDrop<T>(pub(crate) tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
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
mod newline_tests {
    use super::{AltScreen, Ended, Newlines, Status, closed_words, count_need};

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
    use rl_proto::types::RunState;

    /// Leaving on purpose is the only thing that may exit 0. A stream that
    /// went without an exit used to, which told a script it had watched a
    /// run finish cleanly.
    #[test]
    fn only_leaving_on_purpose_is_a_clean_exit() {
        assert_eq!(Ended::Detached.code(), 0);
        assert_eq!(Ended::Left.code(), 0);
        assert_eq!(
            Ended::Exit {
                code: 0,
                how: "exit 0".into()
            }
            .code(),
            0
        );
        assert_eq!(
            Ended::Exit {
                code: 3,
                how: "exit 3".into()
            }
            .code(),
            3
        );
        // Cut off: the run's own code when rld has one…
        assert_eq!(
            Ended::Closed {
                state: Some(RunState::Failed),
                code: Some(137)
            }
            .code(),
            137
        );
        // …and never 0, whether rld said nothing or said zero.
        assert_eq!(
            Ended::Closed {
                state: None,
                code: None
            }
            .code(),
            1
        );
        assert_eq!(
            Ended::Closed {
                state: Some(RunState::Failed),
                code: Some(0)
            }
            .code(),
            1
        );
    }

    #[test]
    fn a_cut_off_stream_says_what_became_of_the_run() {
        assert_eq!(
            closed_words("ab12", Some(RunState::Failed)),
            "run ab12 failed · the session holder went away"
        );
        assert_eq!(
            closed_words("ab12", Some(RunState::Working)),
            "lost touch with run ab12 · rld says it is working"
        );
        assert_eq!(
            closed_words("ab12", None),
            "lost touch with run ab12 · rld could not say what became of it"
        );
        // Never the old words, which read as nothing having gone wrong.
        for s in [None, Some(RunState::Failed), Some(RunState::Working)] {
            assert!(!closed_words("ab12", s).contains("connection closed"));
        }
    }

    #[test]
    fn who_waits_is_counted_once_and_never_this_run() {
        // A question of its own, one about run b (also waiting), one about
        // this run, and run c waiting without one.
        let asked = [None, Some("b"), Some("me")];
        let waiting = ["b", "c", "me"];
        assert_eq!(count_need(&asked, &waiting, "me"), 3);
        assert_eq!(count_need(&[], &[], "me"), 0);
    }

    #[test]
    fn the_title_says_how_to_get_out_and_who_waits() {
        let mut s = Status {
            base: "relay · join abc".into(),
            key: "^\\ lets go",
            need: 0,
            note: None,
        };
        assert_eq!(s.line(), "relay · join abc · ^\\ lets go");
        s.need = 2;
        s.note = Some("bo@x took over".into());
        assert_eq!(s.line(), "relay · join abc · ^\\ lets go · 2 need you · bo@x took over");
    }

    #[test]
    fn a_bare_newline_gets_its_carriage_return_across_chunks() {
        let mut n = Newlines::default();
        assert_eq!(n.fix(b"one\ntwo\r\n"), b"one\r\ntwo\r\n");
        // A `\r` that ends one chunk carries its `\n` in the next.
        assert_eq!(n.fix(b"three\r"), b"three\r");
        assert_eq!(n.fix(b"\nfour\n\n"), b"\nfour\r\n\r\n");
        assert_eq!(n.fix(b""), b"");
    }
}
