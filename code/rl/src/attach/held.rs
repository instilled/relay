//! Attach straight to a Relay holder's socket.
//!
//! The loop ([`drive`]) runs over any stdin/stdout and an injectable stream of
//! window sizes; [`attach`] wraps it in the real terminal (raw mode, SIGWINCH).
//! A session that has ended is still attached to while its holder lingers
//! (the last screen can be seen); its exit file speaks only once the socket
//! is gone. Errors from the holder's side end the attach as `Closed`; only
//! the terminal's own can fail it.

use super::{AbortOnDrop, AltScreen, Ended, RawMode, dim, split_detach, title};
use crate::store::sessions::Meta;
use anyhow::{Context, Result};
use rl_core::Home;
use rl_session::client::Session;
use rl_session::exit::{Exit, read_exit_file};
use rl_session::proto::{Frame, Hello};
use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// A session this young when it is attached may not yet be listening for a
/// resize: Node and Bun agents (Claude Code) set that up half a second or so
/// in, and one that lands before is lost — it draws for the size it started
/// at. So it is nudged once, this long after it started: a row less, then
/// its size back.
const YOUNG: Duration = Duration::from_secs(3);
/// How long that nudge holds the smaller size: long enough for a busy app to
/// read it before it is given back — back to back, the two resizes read as
/// none — and short enough not to be seen.
const NUDGE_HOLD: Duration = Duration::from_millis(200);
/// How long a holder gets to write the exit file after its stream is cut.
const SETTLING: Duration = Duration::from_millis(1500);

/// What `drive` is attached to and how it should present itself.
pub struct View<'a> {
    /// The session's name, for the banner and window title.
    pub name: &'a str,
    /// A real terminal: banner, window title, and the terminal handed back.
    pub interactive: bool,
    /// The terminal's size now; `None` when there is no terminal to ask.
    pub size: Option<(u16, u16)>,
}

/// Runs until detached, input runs out, or the session ends, and says which.
/// `resizes` carries the terminal's new sizes as they happen.
pub async fn drive(
    session: Session,
    hello: &Hello,
    stdin: impl AsyncRead + Send + Unpin + 'static,
    out: &mut (impl AsyncWrite + Unpin),
    mut resizes: mpsc::Receiver<(u16, u16)>,
    view: View<'_>,
) -> Result<Ended> {
    let (mut rd, mut wr) = session.split();
    let mut size = view.size;
    if view.interactive {
        let banner = format!("[relay] {} · ctrl-\\ to detach", view.name);
        let status = title(&format!("relay · {} · ^\\ lets go", view.name));
        out.write_all(format!("\x1b[2m{banner}\x1b[0m\n{status}").as_bytes())
            .await?;
    } else {
        out.write_all(dim(&format!("[relay] {} · ctrl-\\ to detach", view.name)).as_bytes())
            .await?;
    }
    out.flush().await?;

    let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(64);
    // Aborted on every exit from this function (see `AbortOnDrop`), so the
    // reader never outlives the attach and races the next one for stdin.
    let _stdin_task = AbortOnDrop(tokio::spawn(async move {
        let mut stdin = stdin;
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = stdin.read(&mut buf).await {
            if stdin_tx.send(buf[..n].to_vec()).await.is_err() {
                break;
            }
        }
    }));

    let mut stdin_open = true;
    let mut resizes_open = true;
    let mut alt = AltScreen::default();
    // When a young session is nudged, and when the nudge is undone.
    let mut again: Option<tokio::time::Instant> = None;
    let mut restore: Option<tokio::time::Instant> = None;

    let ended: Result<(Ended, Option<String>)> = async {
        // The holder going away is how a session ends, not a failure: each
        // call to it that fails ends the attach as `Closed`.
        macro_rules! holder {
            ($e:expr) => {
                if $e.is_err() {
                    return Ok((Ended::Closed, None));
                }
            };
        }
        if let Some((cols, rows)) = size {
            holder!(wr.resize(cols, rows).await);
        }
        loop {
            tokio::select! {
                frame = rd.recv() => match frame {
                    Err(_) => return Ok((Ended::Closed, None)),
                    Ok(frame) => match frame {
                    Some(Frame::Output(b)) => {
                        alt.watch(&b);
                        out.write_all(&b).await?;
                        out.flush().await?;
                    }
                    Some(Frame::ReplayEnd) => {
                        if size.is_some() {
                            again = nudge_in(hello);
                        }
                    }
                    Some(Frame::Exit(e)) => {
                        let (ended, words) = exit_words(view.name, &e);
                        return Ok((ended, Some(words)));
                    }
                    Some(_) => {}
                    // The stream went without an Exit frame; the caller reads
                    // the exit file to say what became of the session.
                    None => return Ok((Ended::Closed, None)),
                    },
                },
                bytes = stdin_rx.recv(), if stdin_open => match bytes {
                    // Nothing more will be typed: a piped attach is over.
                    None => {
                        stdin_open = false;
                        return Ok((Ended::Left, None));
                    }
                    Some(bytes) => {
                        let (typed, detach) = split_detach(&bytes);
                        if !typed.is_empty() {
                            holder!(wr.input(typed).await);
                        }
                        if detach {
                            let words = format!("[relay] detached; {} keeps going", view.name);
                            return Ok((Ended::Detached, Some(words)));
                        }
                    }
                },
                new = resizes.recv(), if resizes_open => match new {
                    Some((cols, rows)) => {
                        size = Some((cols, rows));
                        holder!(wr.resize(cols, rows).await);
                    }
                    None => resizes_open = false,
                },
                _ = async { tokio::time::sleep_until(again.unwrap()).await }, if again.is_some() => {
                    again = None;
                    if let Some((cols, rows)) = size {
                        // A row rather than a column, as tmux does, so what the
                        // session printed is not wrapped twice.
                        holder!(wr.resize(cols, rows.saturating_sub(1).max(1)).await);
                        restore = Some(tokio::time::Instant::now() + NUDGE_HOLD);
                    }
                }
                _ = async { tokio::time::sleep_until(restore.unwrap()).await }, if restore.is_some() => {
                    restore = None;
                    if let Some((cols, rows)) = size {
                        holder!(wr.resize(cols, rows).await);
                    }
                }
            }
        }
    }
    .await;

    // Gone inside the nudge: the session is not left a row short.
    if restore.is_some()
        && let Some((cols, rows)) = size
    {
        let _ = wr.resize(cols, rows).await;
    }

    let back = view.interactive.then(|| alt.leave());
    match ended {
        Ok((ended, words)) => {
            leave(out, back.as_deref(), words.as_deref()).await?;
            Ok(ended)
        }
        Err(e) => {
            // Gone wrong, the terminal is still handed back, as far as it can
            // be; the error is what is said.
            let _ = leave(out, back.as_deref(), None).await;
            Err(e)
        }
    }
}

/// How long after the replay a session joined young is nudged, if it is.
fn nudge_in(hello: &Hello) -> Option<tokio::time::Instant> {
    if !hello.tty {
        return None;
    }
    let age = jiff::Timestamp::now().duration_since(hello.started_at);
    let left = Duration::try_from(jiff::SignedDuration::try_from(YOUNG).ok()? - age).ok()?;
    (!left.is_zero()).then(|| tokio::time::Instant::now() + left)
}

/// How a session ended, and the line that says so.
pub fn exit_words(name: &str, e: &Exit) -> (Ended, String) {
    let how = match &e.signal {
        Some(s) => format!("{s}, exit {}", e.code),
        None => format!("exit {}", e.code),
    };
    let words = format!("[relay] {name} ended ({how})");
    (Ended::Exit { code: e.code, how }, words)
}

/// The last thing written: the terminal handed back (`back`, on a terminal),
/// then our words on a line of their own and the window title given back.
/// The words come after, so they land on the screen the viewer is left on.
async fn leave(out: &mut (impl AsyncWrite + Unpin), back: Option<&[u8]>, words: Option<&str>) -> std::io::Result<()> {
    if let Some(b) = back {
        out.write_all(b).await?;
    }
    if let Some(w) = words {
        out.write_all(dim(w).as_bytes()).await?;
        if back.is_some() {
            out.write_all(title("").as_bytes()).await?;
        }
    }
    out.flush().await
}

/// How a session that has ended says so, from its exit file.
pub fn from_exit_file(dir: &Path, name: &str) -> Option<(Ended, String)> {
    let exit = read_exit_file(&dir.join("exit")).ok().flatten()?;
    Some(exit_words(name, &exit))
}

/// The exit file, given the holder a moment to write it.
async fn settled_exit(dir: &Path, name: &str) -> Option<(Ended, String)> {
    let deadline = tokio::time::Instant::now() + SETTLING;
    loop {
        if let Some(found) = from_exit_file(dir, name) {
            return Some(found);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// What an attach finds.
pub enum Opened {
    /// The holder answers: its session may be running or ended and lingering.
    Holder(Box<Hello>, Session),
    /// The holder is gone and the exit file says how the session ended.
    Over(Ended, String),
}

/// Connects to the holder, whether its session is running or has ended and
/// lingers; the exit file speaks only where the socket is gone.
///
/// `attach_screen` draws the screen afresh from the holder's emulator; for a
/// session without one it replays bytes from the start instead.
pub async fn open(dir: &Path, meta: &Meta) -> Result<Opened> {
    match Session::attach_screen(&meta.socket, 0).await {
        Ok((hello, session)) => Ok(Opened::Holder(Box::new(hello), session)),
        Err(e) => match from_exit_file(dir, &meta.name) {
            Some((ended, words)) => Ok(Opened::Over(ended, words)),
            None => Err(e).with_context(|| format!("{} is not answering", meta.name)),
        },
    }
}

/// Attaches this terminal to the session until detached or it ends. One that
/// has ended but whose holder lingers is attached to as any other: its last
/// screen shows, and it is left with ctrl-\.
pub async fn attach(home: &Home, meta: &Meta) -> Result<Ended> {
    let dir = home.session_dir(&meta.name);
    let mut out = tokio::io::stdout();
    let (hello, session) = match open(&dir, meta).await? {
        Opened::Holder(hello, session) => (*hello, session),
        Opened::Over(ended, words) => {
            out.write_all(dim(&words).as_bytes()).await?;
            out.flush().await?;
            return Ok(ended);
        }
    };

    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    // Listened for before the size is read, so no resize falls between.
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let _raw = if interactive {
        Some(RawMode::enable().context("entering raw mode")?)
    } else {
        None
    };
    let (sizes_tx, sizes) = mpsc::channel::<(u16, u16)>(4);
    let _sizes = AbortOnDrop(tokio::spawn(async move {
        while winch.recv().await.is_some() {
            if let Ok(size) = crossterm::terminal::size()
                && sizes_tx.send(size).await.is_err()
            {
                break;
            }
        }
    }));
    let size = if interactive {
        crossterm::terminal::size().ok()
    } else {
        None
    };
    let view = View {
        name: &meta.name,
        interactive,
        size,
    };
    let ended = drive(session, &hello, tokio::io::stdin(), &mut out, sizes, view).await?;
    if ended != Ended::Closed {
        return Ok(ended);
    }
    // Cut off without an Exit frame: the holder may have gone, or the child
    // ended and the holder with it. The exit file says which.
    let settled = settled_exit(&dir, &meta.name).await;
    let (ended, words) = match settled {
        Some(found) => found,
        None => (
            Ended::Closed,
            format!("[relay] lost touch with {} · the session holder went away", meta.name),
        ),
    };
    out.write_all(dim(&words).as_bytes()).await?;
    if interactive {
        out.write_all(title("").as_bytes()).await?;
    }
    out.flush().await?;
    Ok(ended)
}
