//! `rl watch` / `rl join`: this terminal becomes the run's terminal.
//!
//! One `run.attach` call, then the connection carries holder frames. On a
//! real terminal we go raw and forward resizes; piped, we just stream bytes
//! (so `rl watch <id> | tee log` works). Keys are bytes, never parsed: what
//! you type is what the child gets. `ctrl-\` detaches a join; `q` leaves a
//! watch. The run never notices either.

use crate::client::Client;
use anyhow::{Context, Result};
use relay_core::Home;
use relay_proto::methods::{AttachParams, RunAttach};
use relay_proto::types::AttachMode;
use rl_session::proto::{Frame, read_frame, write_frame};
use std::io::IsTerminal;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// `ctrl-\`: detach. Nothing interactive we care about uses it.
const DETACH: u8 = 0x1c;
const CTRL_C: u8 = 0x03;

pub struct Opts {
    pub id: String,
    pub mode: AttachMode,
    pub take: bool,
    /// Bytes.
    pub scrollback: u64,
}

/// Runs until detached or the run ends. Returns the process exit code to use:
/// the run's own if it ended while we were there, else 0.
pub async fn attach(home: &Home, o: Opts) -> Result<i32> {
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
    let status = format!("relay · {} {}", o.mode, info.run.id);
    if interactive {
        out.write_all(format!("\x1b[2m{banner}\x1b[0m\n{}", title(&status)).as_bytes())
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
    tokio::spawn(async move {
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
    });
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let mut stdin_open = true;

    loop {
        tokio::select! {
            frame = read_frame(&mut rd) => match frame? {
                Some(Frame::Output(b)) => {
                    out.write_all(&b).await?;
                    out.flush().await?;
                }
                Some(Frame::Note(s)) => {
                    if interactive {
                        out.write_all(title(&format!("{status} · {s}")).as_bytes()).await?;
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
                    return Ok(e.code);
                }
                Some(_) => {}
                None => {
                    out.write_all(dim("[relay] connection closed").as_bytes()).await?;
                    out.write_all(title("").as_bytes()).await?;
                    out.flush().await?;
                    return Ok(0);
                }
            },
            bytes = stdin_rx.recv(), if stdin_open => match bytes {
                None => {
                    stdin_open = false;
                    if joined {
                        // Nothing more will be typed: a piped join is over.
                        return Ok(0);
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
                            return Ok(0);
                        }
                        write_frame(&mut wr, &Frame::Input(bytes)).await?;
                    } else if bytes.iter().any(|&b| b == b'q' || b == CTRL_C || b == DETACH) {
                        out.write_all(dim("[relay] left").as_bytes()).await?;
                        out.write_all(title("").as_bytes()).await?;
                        out.flush().await?;
                        return Ok(0);
                    }
                }
            },
            _ = winch.recv() => {
                if joined && let Ok((cols, rows)) = crossterm::terminal::size() {
                    write_frame(&mut wr, &Frame::Resize { cols, rows }).await?;
                }
            }
        }
    }
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

struct RawMode;

impl RawMode {
    fn enable() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        use std::io::Write;
        // The run may have taken the terminal into an alternate screen or
        // turned on mouse reporting. Put it back: main screen, cursor shown,
        // mouse and bracketed paste off, attributes reset — so a shell prompt
        // or the panel comes back to a clean screen, not the run's leftovers.
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?1049l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?25h\x1b[0m");
        let _ = out.flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
