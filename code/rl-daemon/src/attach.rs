//! Viewers. `run.attach` turns a daemon connection into a terminal stream:
//! scrollback from the journal, then the holder's replay from exactly where
//! the journal stops, then live output — and, for the one joiner, keystrokes
//! and resizes back, journaled on the way.
//!
//! Each viewer gets its own holder connection; the supervisor's own follow
//! connection keeps journaling regardless, so a viewer leaving costs nothing.

use crate::server::Daemon;
use crate::supervisor::Live;
use jiff::Timestamp;
use relay_ledger::session_io::{self, Direction};
use relay_ledger::{presence, runs, sessions};
use relay_proto::methods::{AttachInfo, AttachParams};
use relay_proto::rpc::RpcError;
use relay_proto::types::AttachMode;
use rl_session::client::Session;
use rl_session::exit::Exit;
use rl_session::proto::{Frame, read_frame, write_frame};
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

/// An accepted attach, ready to stream.
pub struct Attached {
    info: AttachInfo,
    client: String,
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
            client: p.client,
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
    let mut replaced = None;
    if joiner {
        replaced = live.join(&p.client, p.take).map_err(|current| {
            RpcError::new(
                RpcError::BUSY,
                format!("{id} is already joined by {current}; `--take` to take over"),
            )
        })?;
    }
    // Subscribe before announcing, so nothing is missed.
    let notes = live.notes.subscribe();

    let attached = tokio::time::timeout(ATTACH_TIMEOUT, Session::attach_from(&live.socket, Some(journaled))).await;
    let (hello, holder) = match attached {
        Ok(Ok(x)) => x,
        other => {
            if joiner {
                live.leave(&p.client);
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
            Some(old) if old != p.client => format!("{} took over from {old}", p.client),
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
            joiner: live.joiner(),
            journaled,
            replay_from: hello.replay_from,
        },
        client: p.client,
        joiner,
        tail,
        holder: Some(holder),
        live: Some(live),
        notes: Some(notes),
    })
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

    let (mut hrd, mut hwr) = holder.split();
    let mut notes_open = true;
    let mut size = (a.info.cols, a.info.rows);
    let result: io::Result<()> = async {
        loop {
            tokio::select! {
                _ = d.shutdown.cancelled() => {
                    write_frame(wr, &Frame::Note("daemon stopping; detached (the run keeps going)".into())).await?;
                    return Ok(());
                }
                frame = read_frame(rd) => match frame? {
                    None => return Ok(()),
                    Some(Frame::Input(bytes)) => {
                        if a.joiner && live.is_joiner(&a.client) {
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
                        if a.joiner && live.is_joiner(&a.client) {
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
                        // Replayed bytes were drawn for a screen that is not this
                        // one: a full-screen app paints at stale cursor positions
                        // until it repaints. Nudge the pty size so it does now.
                        if a.info.tty {
                            let (cols, rows) = size;
                            hwr.resize(cols.saturating_sub(1).max(1), rows).await?;
                            hwr.resize(cols, rows).await?;
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

    if a.joiner && live.leave(&a.client) {
        let _ = live.notes.send(format!("{} left", a.client));
    }
    info!(id, client = %a.client, "viewer detached");
    result
}
