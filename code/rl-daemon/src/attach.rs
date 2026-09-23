//! Viewers. `run.attach` turns a daemon connection into a terminal stream:
//! scrollback from the journal, then the holder's replay from exactly where
//! the journal stops, then live output — and, for the one joiner, keystrokes
//! and resizes back, journaled on the way.
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
use rl_proto::types::AttachMode;
use rl_session::client::{Session, SessionWriter};
use rl_session::exit::Exit;
use rl_session::proto::{Frame, FrameReader, write_frame};
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
/// How long the repaint nudge holds the smaller size: long enough for a busy
/// app to read it before it is given back — back to back, the two resizes
/// read as none — and short enough not to be seen.
const NUDGE_HOLD: Duration = Duration::from_millis(200);
/// How long after the replay a joiner has to say its size before the run is
/// nudged anyway: its first `Resize` follows the attach at once, so this only
/// runs out for one with no terminal to measure.
const JOIN_SIZE_WAIT: Duration = Duration::from_millis(300);

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
        .call({
            let id = id.clone();
            move |c| runs::get(c, &id)
        })
        .await
        .map_err(RpcError::internal)?
        .ok_or_else(|| RpcError::not_found("run", &id))?;

    let now = Timestamp::now();
    let scrollback = p.scrollback.min(SCROLLBACK_MAX) as usize;
    let (tail, journaled) = d
        .ledger
        .call({
            let id = id.clone();
            let client = p.client.clone();
            move |c| {
                presence::touch(c, &client, now)?;
                Ok((session_io::tail(c, &id, scrollback)?, session_io::out_bytes(c, &id)?))
            }
        })
        .await
        .map_err(RpcError::internal)?;

    if run.state.is_terminal() {
        let (cols, rows, tty) = d
            .ledger
            .call({
                let id = id.clone();
                move |c| sessions::get(c, &id)
            })
            .await
            .ok()
            .flatten()
            .map(|s| (s.cols, s.rows, s.tty))
            .unwrap_or((0, 0, false));
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
    let joiner = p.mode == AttachMode::Join;
    let seat = Seat {
        client: p.client.clone(),
        terminal: p.terminal.clone(),
    };
    let mut replaced = None;
    if joiner {
        replaced = live.join(&seat, p.take).map_err(|current| {
            let who = current.said_to(&seat);
            let mut e = RpcError::new(
                RpcError::BUSY,
                format!("{id} is already joined by {who}; `--take` to take over"),
            );
            e.data = Some(serde_json::json!({ "joiner": who }));
            e
        })?;
    }
    // Subscribe before announcing, so nothing is missed.
    let notes = live.notes.subscribe();

    let attached = tokio::time::timeout(ATTACH_TIMEOUT, Session::attach_from(&live.socket, Some(journaled))).await;
    let (hello, holder) = match attached {
        Ok(Ok(x)) => x,
        other => {
            if joiner {
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
            }
            let why = match other {
                Ok(Err(e)) => format!("{e:#}"),
                _ => "timed out".into(),
            };
            return Err(RpcError::internal(format!("session holder did not answer: {why}")));
        }
    };
    if joiner {
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
    info!(id, client = %p.client, mode = %p.mode, replay_from = hello.replay_from, journaled, "viewer attached");
    Ok(Attached {
        info: AttachInfo {
            run,
            cols: hello.cols,
            rows: hello.rows,
            tty: hello.tty,
            live: true,
            joiner: live.joiner().map(|j| j.said_to(&seat)),
            journaled,
            replay_from: hello.replay_from,
        },
        seat,
        joiner,
        tail,
        holder: Some(holder),
        live: Some(live),
        notes: Some(notes),
    })
}

/// Gives the pty a row less, to be given back at the instant returned; `None`
/// when another viewer's nudge already holds it. Held a moment, because sent
/// back to back the two resizes land together, the app reads the size it
/// already had, and a runtime that only reports a change (Node, Bun: Claude
/// Code) never repaints. A row rather than a column, as tmux does, so what
/// the run printed is not wrapped twice.
async fn nudge(live: &Live, hwr: &mut SessionWriter, size: (u16, u16)) -> io::Result<Option<tokio::time::Instant>> {
    if !live.nudge_start(size) {
        return Ok(None);
    }
    let (cols, rows) = size;
    hwr.resize(cols, rows.saturating_sub(1).max(1)).await?;
    Ok(Some(tokio::time::Instant::now() + NUDGE_HOLD))
}

/// Streams until the viewer hangs up, the run ends, or the daemon stops.
pub async fn stream<R, W>(d: &Daemon, a: Attached, rd: &mut R, wr: &mut W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let id = a.info.run.id.clone();
    if !a.tail.is_empty() {
        write_frame(wr, &Frame::Output(a.tail)).await?;
    }
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
    // Another viewer's nudge may be holding the pty a row short: its own
    // size is the one it will give back.
    let mut size = live.nudged_size().unwrap_or((a.info.cols, a.info.rows));
    // When our repaint nudge gives the pty its size back (see `ReplayEnd`).
    let mut restore: Option<tokio::time::Instant> = None;
    // A joiner's repaint turns on its first size: `None` until it says one,
    // then whether that changed the pty's. It may come before the replay has
    // ended or after; the second of the two decides.
    let mut first_size: Option<bool> = None;
    let mut replayed = false;
    // Until when a joiner that has said no size is waited for.
    let mut size_wait: Option<tokio::time::Instant> = None;
    // The run ended or rld is stopping: whatever else ended the stream was
    // the viewer going — a hang-up, or (Linux) a write that finds it gone.
    let mut over = false;
    let result: io::Result<()> = async {
        loop {
            tokio::select! {
                _ = async { tokio::time::sleep_until(restore.unwrap()).await }, if restore.is_some() => {
                    restore = None;
                    if let Some((cols, rows)) = live.nudge_end() {
                        hwr.resize(cols, rows).await?;
                    }
                }
                _ = async { tokio::time::sleep_until(size_wait.unwrap()).await }, if size_wait.is_some() => {
                    size_wait = None;
                    if live.is_joiner(&a.seat) {
                        restore = nudge(&live, &mut hwr, size).await?;
                    }
                }
                _ = d.shutdown.cancelled() => {
                    over = true;
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
                            let changed = (cols, rows) != size;
                            size = (cols, rows);
                            live.nudge_resized(size);
                            hwr.resize(cols, rows).await?;
                            let id = id.clone();
                            let _ = d.ledger.call(move |c| sessions::set_size(c, &id, cols, rows)).await;
                            // A new size is a repaint by itself. The same one
                            // is none: that takes the nudge.
                            if a.info.tty && first_size.is_none() {
                                first_size = Some(changed);
                                size_wait = None;
                                if replayed && !changed {
                                    restore = nudge(&live, &mut hwr, size).await?;
                                }
                            }
                        }
                    }
                    Some(other) => debug!(?other, "ignoring frame from viewer"),
                },
                frame = hrd.recv() => match frame {
                    Ok(Some(Frame::Output(b))) => write_frame(wr, &Frame::Output(b)).await?,
                    Ok(Some(Frame::ReplayEnd)) => {
                        write_frame(wr, &Frame::ReplayEnd).await?;
                        // Replayed bytes were drawn for a screen that is not this
                        // one, and a tail of them is not a screen at all: an app
                        // that only redraws what changed leaves its box and its
                        // status line out until it repaints. It repaints when
                        // its size changes — so a joiner at a size the run does
                        // not have needs nothing more, and one at the size it
                        // has gets the pty nudged (`nudge`). Which it is, the
                        // joiner's first `Resize` says.
                        //
                        // A watcher is nudged at once, but only when nobody is
                        // at the keyboard: opening a pane must not jolt a join.
                        replayed = true;
                        if a.info.tty && a.joiner && live.is_joiner(&a.seat) {
                            match first_size {
                                Some(true) => {}
                                Some(false) => restore = nudge(&live, &mut hwr, size).await?,
                                None => size_wait = Some(tokio::time::Instant::now() + JOIN_SIZE_WAIT),
                            }
                        } else if a.info.tty && live.joiner().is_none() {
                            restore = nudge(&live, &mut hwr, size).await?;
                        }
                    }
                    Ok(Some(Frame::Exit(e))) => {
                        over = true;
                        write_frame(wr, &Frame::Exit(e)).await?;
                        return Ok(());
                    }
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => {
                        over = true;
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

    // Gone inside our nudge: the run is not left a row short.
    if restore.is_some()
        && let Some(real) = live.nudge_end()
    {
        size = real;
        let _ = hwr.resize(size.0, size.1).await;
    }
    // Letting go of a run leaves it a row short, where nobody sees it redraw:
    // whoever joins next brings a size it does not have, and that one change
    // is the repaint — not two, as a nudge on the way in would be.
    if !over && a.info.tty && a.joiner && live.is_joiner(&a.seat) && live.nudged_size().is_none() {
        let _ = hwr.resize(size.0, size.1.saturating_sub(1).max(1)).await;
    }
    if a.joiner && live.leave(&a.seat) {
        let _ = live.notes.send(format!("{} left", a.seat.client));
    }
    info!(id, client = %a.seat.client, "viewer detached");
    result
}
