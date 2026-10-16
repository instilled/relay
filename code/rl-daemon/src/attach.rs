//! Viewers. `run.attach` turns a daemon connection into a terminal stream.
//! What comes first depends on the run: a terminal run's viewer is handed its
//! screen, drawn afresh by the holder; a pipes run's gets the journal's tail,
//! then the holder's replay from exactly where the journal stops; an ended
//! run's gets the journal's tail, drawn as a screen when it was a terminal
//! run. Then live output — and, for the one joiner, keystrokes and resizes
//! back, journaled on the way.
//!
//! Each viewer gets its own holder connection; the supervisor's own follow
//! connection keeps journaling regardless, so a viewer leaving costs nothing.

use crate::server::Daemon;
use crate::supervisor::{Live, Seat};
use jiff::Timestamp;
use rl_ledger::session_io::{self, Direction};
use rl_ledger::{presence, runs, sessions};
use rl_proto::methods::{AttachInfo, AttachParams};
use rl_proto::rpc::RpcError;
use rl_session::client::Session;
use rl_session::exit::Exit;
use rl_session::proto::{Frame, FrameReader, write_frame, write_output};
use rl_session::screen::Screen;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

/// How long a holder gets to answer a viewer's attach.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(2);
/// Scrollback is capped here whatever the viewer asks for.
const SCROLLBACK_MAX: u64 = 8 << 20;
/// A run this young when it is joined may not yet be listening for a resize:
/// Node and Bun agents (Claude Code) set that up half a second or so in, and
/// one that lands before is lost — it draws for the size it started at. So
/// it is nudged once, this long after it started: a row less, then its size
/// back.
const YOUNG: Duration = Duration::from_secs(3);
/// How long that nudge holds the smaller size: long enough for a busy app to
/// read it before it is given back — back to back, the two resizes read as
/// none — and short enough not to be seen.
const NUDGE_HOLD: Duration = Duration::from_millis(200);

/// An accepted attach, ready to stream.
pub struct Attached {
    info: AttachInfo,
    seat: Seat,
    joiner: bool,
    tail: Vec<u8>,
    /// `None` for an ended run: the journal is all there is.
    holder: Option<Session>,
    live: Option<Arc<Live>>,
    notes: Option<broadcast::Receiver<String>>,
}

impl Attached {
    pub fn info(&self) -> &AttachInfo {
        &self.info
    }
}

/// Validates, claims the keyboard if joining, reads the scrollback and
/// connects to the holder. Nothing has been written to the viewer yet.
pub async fn attach(d: &Daemon, p: AttachParams) -> Result<Attached, RpcError> {
    let id = p.id.clone();
    let run = d
        .ledger
        .call_read({
            let id = id.clone();
            move |c| runs::get(c, &id)
        })
        .await
        .map_err(RpcError::internal)?
        .ok_or_else(|| RpcError::not_found("run", &id))?;

    let now = Timestamp::now();
    let scrollback = p.scrollback.min(SCROLLBACK_MAX) as usize;
    let journaled = d
        .ledger
        .call({
            let id = id.clone();
            let client = p.client.clone();
            move |c| {
                presence::touch(c, &client, now)?;
                session_io::out_bytes(c, &id)
            }
        })
        .await
        .map_err(RpcError::internal)?;

    if run.state.is_terminal() {
        let tail = journal_tail(d, &id, scrollback, journaled).await?;
        let (cols, rows, tty) = d
            .ledger
            .call_read({
                let id = id.clone();
                move |c| sessions::get(c, &id)
            })
            .await
            .ok()
            .flatten()
            .map(|s| (s.cols, s.rows, s.tty))
            .unwrap_or((0, 0, false));
        // A terminal run's tail is redraws on redraws: what a viewer wants is
        // the screen it was left showing. Drawing megabytes of it through a
        // terminal is real work, kept off the runtime's own threads.
        let tail = if tty && cols > 0 {
            tokio::task::spawn_blocking(move || Screen::render(&tail, cols, rows))
                .await
                .map_err(RpcError::internal)?
        } else {
            tail
        };
        return Ok(Attached {
            info: AttachInfo {
                run,
                cols,
                rows,
                tty,
                live: false,
                joiner: None,
                journaled,
                replay_from: journaled,
            },
            seat: Seat {
                client: p.client,
                terminal: p.terminal,
            },
            joiner: false,
            tail,
            holder: None,
            live: None,
            notes: None,
        });
    }

    let live = d
        .supervisor
        .live(&id)
        .ok_or_else(|| RpcError::internal(format!("run {id} is alive but this daemon is not attached to it")))?;
    let seat = Seat {
        client: p.client.clone(),
        terminal: p.terminal.clone(),
    };
    // Attaching to a live run is joining it: there is no reading it over
    // someone's shoulder, so the seat is claimed or the attach is refused.
    let replaced = live.join(&seat, p.take).map_err(|current| {
        let who = current.said_to(&seat);
        let mut e = RpcError::new(
            RpcError::BUSY,
            format!("{id} is already joined by {who}; `--take` to take over"),
        );
        e.data = Some(serde_json::json!({ "joiner": who }));
        e
    })?;
    // Subscribe before announcing, so nothing is missed.
    let notes = live.notes.subscribe();

    let attached = async {
        let answered = if live.hello.tty {
            tokio::time::timeout(ATTACH_TIMEOUT, Session::attach_screen(&live.socket, journaled)).await
        } else {
            tokio::time::timeout(ATTACH_TIMEOUT, Session::attach_from(&live.socket, Some(journaled))).await
        };
        let (hello, holder) = match answered {
            Ok(Ok(x)) => x,
            Ok(Err(e)) => return Err(RpcError::internal(format!("session holder did not answer: {e:#}"))),
            Err(_) => return Err(RpcError::internal("session holder did not answer: timed out")),
        };
        // The screen is whole by itself: the journal's tail would only be
        // drawn over, so it is not even read. A pipes run's bytes pick up at
        // `journaled`, where the tail stops.
        let tail = if hello.tty {
            Vec::new()
        } else {
            journal_tail(d, &id, scrollback, journaled).await?
        };
        Ok((hello, holder, tail))
    }
    .await;
    let (hello, holder, tail) = match attached {
        Ok(x) => x,
        Err(e) => {
            match &replaced {
                // The take-over didn't go through: put the client we
                // evicted back in the joiner's seat instead of leaving
                // the run with no joiner at all.
                Some(old) => {
                    let _ = live.join(old, true);
                    let _ = live.notes.send(format!(
                        "{} could not take over; {} stays joined",
                        p.client,
                        old.said_to(&seat)
                    ));
                }
                None => {
                    live.leave(&seat);
                }
            }
            return Err(e);
        }
    };
    {
        let what = match replaced {
            // Your own other terminal: said so, not you twice.
            Some(old) if old != seat && old.client == seat.client => {
                format!("{} took over in another terminal", p.client)
            }
            Some(old) if old != seat => format!("{} took over from {}", p.client, old.client),
            _ => format!("{} joined", p.client),
        };
        let _ = live.notes.send(what);
    }
    // Nothing is missing between a screen and live.
    let replay_from = if hello.tty { journaled } else { hello.replay_from };
    info!(id, client = %p.client, tty = hello.tty, replay_from, journaled, "viewer attached");
    Ok(Attached {
        info: AttachInfo {
            run,
            cols: hello.cols,
            rows: hello.rows,
            tty: hello.tty,
            live: true,
            joiner: live.joiner().map(|j| j.said_to(&seat)),
            journaled,
            replay_from,
        },
        seat,
        joiner: true,
        tail,
        holder: Some(holder),
        live: Some(live),
        notes: Some(notes),
    })
}

/// The last `scrollback` bytes of the run's output that end exactly at
/// `journaled`. The journal goes on growing while the holder is asked, and
/// a replay picks up at `journaled`, so whatever was journaled since is read
/// over and cut off rather than shown twice.
async fn journal_tail(d: &Daemon, id: &str, scrollback: usize, journaled: u64) -> Result<Vec<u8>, RpcError> {
    let id = id.to_string();
    d.ledger
        .call_read(move |c| {
            let since = session_io::out_bytes(c, &id)?.saturating_sub(journaled) as usize;
            let mut tail = session_io::tail(c, &id, scrollback + since)?;
            tail.truncate(tail.len().saturating_sub(since));
            Ok(tail)
        })
        .await
        .map_err(RpcError::internal)
}

/// Streams until the viewer hangs up, the run ends, or the daemon stops.
pub async fn stream<R, W>(d: &Daemon, a: Attached, rd: &mut R, wr: &mut W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let id = a.info.run.id.clone();
    write_output(wr, &a.tail).await?;
    let gap = a.info.replay_from.saturating_sub(a.info.journaled);
    if gap > 0 {
        write_frame(
            wr,
            &Frame::Note(format!("{gap} bytes of output are missing between scrollback and live")),
        )
        .await?;
    }
    let (Some(holder), Some(live), Some(mut notes)) = (a.holder, a.live, a.notes) else {
        // Ended: the scrollback was everything; say how it ended.
        write_frame(wr, &Frame::ReplayEnd).await?;
        let run = &a.info.run;
        let exit = Exit {
            code: run.exit_code.unwrap_or(-1),
            signal: None,
            at: run.ended_at.unwrap_or_else(Timestamp::now),
        };
        write_frame(wr, &Frame::Exit(exit)).await?;
        return Ok(());
    };

    // The viewer's frames, read cancel-safely: this read shares the loop
    // below with the holder's output and rld's own shutdown.
    let mut rd = FrameReader::new(rd);
    let (mut hrd, mut hwr) = holder.split();
    let mut notes_open = true;
    let mut size = (a.info.cols, a.info.rows);
    // When a run joined young is nudged (see `YOUNG`), and when that nudge
    // gives the pty its size back.
    let mut again: Option<tokio::time::Instant> = None;
    let mut restore: Option<tokio::time::Instant> = None;
    let result: io::Result<()> = async {
        loop {
            tokio::select! {
                _ = async { tokio::time::sleep_until(restore.unwrap()).await }, if restore.is_some() => {
                    restore = None;
                    if live.is_joiner(&a.seat) {
                        hwr.resize(size.0, size.1).await?;
                    }
                }
                _ = async { tokio::time::sleep_until(again.unwrap()).await }, if again.is_some() => {
                    again = None;
                    if live.is_joiner(&a.seat) {
                        // A row rather than a column, as tmux does, so what
                        // the run printed is not wrapped twice.
                        hwr.resize(size.0, size.1.saturating_sub(1).max(1)).await?;
                        restore = Some(tokio::time::Instant::now() + NUDGE_HOLD);
                    }
                }
                _ = d.shutdown.cancelled() => {
                    write_frame(wr, &Frame::Note("daemon stopping; detached (the run keeps going)".into())).await?;
                    return Ok(());
                }
                frame = rd.next() => match frame? {
                    None => return Ok(()),
                    Some(Frame::Input(bytes)) => {
                        if a.joiner && live.is_joiner(&a.seat) {
                            hwr.input(bytes.clone()).await?;
                            let id = id.clone();
                            let now = Timestamp::now();
                            if let Err(e) = d
                                .ledger
                                .call(move |c| session_io::append(c, &id, Direction::In, &bytes, now).map(|_| ()))
                                .await
                            {
                                warn!(error = %e, "journaling input");
                            }
                        }
                    }
                    Some(Frame::Resize { cols, rows }) => {
                        if a.joiner && live.is_joiner(&a.seat) {
                            size = (cols, rows);
                            hwr.resize(cols, rows).await?;
                            let id = id.clone();
                            let _ = d.ledger.call(move |c| sessions::set_size(c, &id, cols, rows)).await;
                        }
                    }
                    Some(other) => debug!(?other, "ignoring frame from viewer"),
                },
                frame = hrd.recv() => match frame {
                    Ok(Some(Frame::Output(b))) => write_frame(wr, &Frame::Output(b)).await?,
                    Ok(Some(Frame::ReplayEnd)) => {
                        write_frame(wr, &Frame::ReplayEnd).await?;
                        if a.info.tty && a.joiner && live.is_joiner(&a.seat) {
                            let age = Timestamp::now().duration_since(a.info.run.started_at);
                            if let Ok(left) = Duration::try_from(jiff::SignedDuration::try_from(YOUNG).unwrap_or_default() - age)
                                && !left.is_zero()
                            {
                                again = Some(tokio::time::Instant::now() + left);
                            }
                        }
                    }
                    Ok(Some(Frame::Exit(e))) => {
                        write_frame(wr, &Frame::Exit(e)).await?;
                        return Ok(());
                    }
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => {
                        write_frame(wr, &Frame::Note("session holder went away".into())).await?;
                        return Ok(());
                    }
                },
                note = notes.recv(), if notes_open => match note {
                    Ok(s) => write_frame(wr, &Frame::Note(s)).await?,
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => notes_open = false,
                },
            }
        }
    }
    .await;

    // Gone inside the nudge: the run is not left a row short.
    if restore.is_some() && live.is_joiner(&a.seat) {
        let _ = hwr.resize(size.0, size.1).await;
    }
    if a.joiner && live.leave(&a.seat) {
        let _ = live.notes.send(format!("{} left", a.seat.client));
    }
    info!(id, client = %a.seat.client, "viewer detached");
    result
}
