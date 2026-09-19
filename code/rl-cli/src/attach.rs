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
use rl_proto::methods::{AttachParams, DecisionFilter, DecisionList, RunAttach, RunFilter, RunList};
use rl_proto::types::{AttachMode, RunState};
use rl_session::proto::{Frame, read_frame, write_frame};
use std::io::IsTerminal;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

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

/// `ctrl-\`: detach. Nothing interactive we care about uses it.
const DETACH: u8 = 0x1c;
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
    /// The connection went without a word.
    Closed,
}

impl Ended {
    /// The process exit code: the run's own if it ended, else 0.
    pub fn code(&self) -> i32 {
        match self {
            Ended::Exit { code, .. } => *code,
            _ => 0,
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
        })
        .await?;
    let (mut rd, mut wr) = c.into_parts();
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

    loop {
        tokio::select! {
            frame = read_frame(&mut rd) => match frame? {
                Some(Frame::Output(b)) => {
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
                    out.write_all(dim(&format!("[relay] run {} ended ({how})", info.run.id)).as_bytes()).await?;
                    out.write_all(title("").as_bytes()).await?;
                    out.flush().await?;
                    return Ok(Ended::Exit { code: e.code, how });
                }
                Some(_) => {}
                None => {
                    out.write_all(dim("[relay] connection closed").as_bytes()).await?;
                    out.write_all(title("").as_bytes()).await?;
                    out.flush().await?;
                    return Ok(Ended::Closed);
                }
            },
            bytes = stdin_rx.recv(), if stdin_open => match bytes {
                None => {
                    stdin_open = false;
                    if joined {
                        // Nothing more will be typed: a piped join is over.
                        return Ok(Ended::Left);
                    }
                }
                Some(bytes) => {
                    if joined {
                        if let Some(at) = bytes.iter().position(|&b| b == DETACH) {
                            if at > 0 {
                                write_frame(&mut wr, &Frame::Input(bytes[..at].to_vec())).await?;
                            }
                            out.write_all(dim("[relay] detached; the run keeps going").as_bytes()).await?;
                            out.write_all(title("").as_bytes()).await?;
                            out.flush().await?;
                            return Ok(Ended::Detached);
                        }
                        write_frame(&mut wr, &Frame::Input(bytes)).await?;
                    } else if bytes.iter().any(|&b| b == b'q' || b == CTRL_C || b == DETACH) {
                        out.write_all(dim("[relay] left").as_bytes()).await?;
                        out.write_all(title("").as_bytes()).await?;
                        out.flush().await?;
                        return Ok(Ended::Left);
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

/// The terminal's window title: status without touching the screen. Empty
/// hands the title back to the terminal.
fn title(s: &str) -> String {
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
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct RawMode;

impl RawMode {
    fn enable() -> Result<Self> {
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
    use super::{Newlines, Status, count_need};

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
