//! The holder server: one child, one socket, N attached clients.
//!
//! Output flows child → ring buffer → every attached client. Input, resize and
//! signals flow the other way from whichever client sends them (the daemon
//! decides who may). After the child exits the holder lingers so a daemon that
//! was down at the time can still attach, replay the tail and read the exit —
//! then it goes away on `Close` or after `linger` with nobody attached.

use crate::backend::{self, Resizer, Spec};
use crate::exit::{Exit, write_exit_file};
use crate::proto::{Frame, Hello, read_frame, write_frame};
use crate::ring::Ring;
use anyhow::{Context, Result};
use jiff::Timestamp;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Scrollback kept for replay.
pub const RING_CAPACITY: usize = 1 << 20;
/// Output chunks a slow client may fall behind before it is dropped.
const BROADCAST_CAPACITY: usize = 4096;

#[derive(Debug, Clone)]
pub struct Config {
    pub id: String,
    pub socket: PathBuf,
    pub exit_file: Option<PathBuf>,
    pub spec: Spec,
    /// After the child exits: how long to stay with no clients attached.
    pub linger: Duration,
}

struct Shared {
    id: String,
    pid: u32,
    tty: bool,
    cols: u16,
    rows: u16,
    started_at: Timestamp,
    ring: Ring,
    exit: Option<Exit>,
}

impl Shared {
    fn hello(&self) -> Hello {
        Hello {
            id: self.id.clone(),
            pid: self.pid,
            holder_pid: std::process::id(),
            cols: self.cols,
            rows: self.rows,
            tty: self.tty,
            started_at: self.started_at,
            exit: self.exit.clone(),
            total: self.ring.total(),
            replay_from: self.ring.total(),
        }
    }
}

struct Ctx {
    shared: Mutex<Shared>,
    out: broadcast::Sender<Arc<[u8]>>,
    exit: watch::Sender<Option<Exit>>,
    input: mpsc::Sender<Vec<u8>>,
    resize: Resizer,
    clients: AtomicUsize,
    shutdown: CancellationToken,
}

/// Serves until `Close`, SIGTERM, or the linger timeout after exit. `ready`
/// fires once the socket is bound and the child is running.
pub async fn serve(cfg: Config, ready: oneshot::Sender<()>) -> Result<()> {
    let _ = std::fs::remove_file(&cfg.socket);
    let listener = UnixListener::bind(&cfg.socket).with_context(|| format!("binding {}", cfg.socket.display()))?;
    let spawned = backend::spawn(&cfg.spec)?;
    info!(id = %cfg.id, pid = spawned.pid, tty = cfg.spec.tty, argv = ?cfg.spec.argv, "session started");

    let (out_tx, _) = broadcast::channel(BROADCAST_CAPACITY);
    let (exit_tx, _) = watch::channel(None);
    let ctx = Arc::new(Ctx {
        shared: Mutex::new(Shared {
            id: cfg.id.clone(),
            pid: spawned.pid,
            tty: cfg.spec.tty,
            cols: cfg.spec.cols,
            rows: cfg.spec.rows,
            started_at: Timestamp::now(),
            ring: Ring::new(RING_CAPACITY),
            exit: None,
        }),
        out: out_tx,
        exit: exit_tx,
        input: spawned.input,
        resize: spawned.resize,
        clients: AtomicUsize::new(0),
        shutdown: CancellationToken::new(),
    });
    let _ = ready.send(());

    let pump = tokio::spawn(pump_output(ctx.clone(), spawned.output));
    tokio::spawn(watch_exit(
        ctx.clone(),
        spawned.exit,
        pump,
        cfg.exit_file.clone(),
        cfg.linger,
    ));
    tokio::spawn(watch_signals(ctx.clone()));

    loop {
        tokio::select! {
            _ = ctx.shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => { tokio::spawn(handle_client(ctx.clone(), stream)); }
                Err(e) => warn!(error = %e, "accept failed"),
            },
        }
    }

    // Closing with a live child: hang it up, give it a moment. The child is a
    // session leader (the PTY setsid'd it), so a negative pid reaches its whole
    // process group — the wrapper shell and anything it spawned, not just it.
    if ctx.exit.borrow().is_none() {
        let pid = ctx.shared.lock().unwrap().pid;
        let _ = kill(Pid::from_raw(-(pid as i32)), Signal::SIGHUP);
        let mut rx = ctx.exit.subscribe();
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while rx.borrow().is_none() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
    }
    let _ = std::fs::remove_file(&cfg.socket);
    info!(id = %cfg.id, "session holder exiting");
    Ok(())
}

/// Child output → ring + broadcast, under one lock so an attaching client's
/// snapshot and subscription line up with no gap and no duplicate.
async fn pump_output(ctx: Arc<Ctx>, mut output: mpsc::Receiver<Vec<u8>>) {
    while let Some(chunk) = output.recv().await {
        let mut s = ctx.shared.lock().unwrap();
        s.ring.push(&chunk);
        let _ = ctx.out.send(Arc::from(chunk));
    }
    debug!("output pump finished");
}

async fn watch_exit(
    ctx: Arc<Ctx>,
    exit: oneshot::Receiver<Exit>,
    pump: tokio::task::JoinHandle<()>,
    exit_file: Option<PathBuf>,
    linger: Duration,
) {
    let exit = match exit.await {
        Ok(e) => e,
        Err(_) => Exit {
            code: -1,
            signal: None,
            at: Timestamp::now(),
        },
    };
    // Let the last output drain before announcing the exit, but do not wait
    // on a grandchild that kept the pty open.
    let _ = tokio::time::timeout(Duration::from_secs(2), pump).await;
    info!(code = exit.code, signal = ?exit.signal, "child exited");
    if let Some(p) = &exit_file
        && let Err(e) = write_exit_file(p, &exit)
    {
        warn!(error = %e, path = %p.display(), "writing exit file");
    }
    ctx.shared.lock().unwrap().exit = Some(exit.clone());
    let _ = ctx.exit.send(Some(exit));

    // Linger: leave when nobody has been attached for `linger`.
    let mut idle = Duration::ZERO;
    let tick = Duration::from_secs(1);
    loop {
        tokio::time::sleep(tick).await;
        if ctx.clients.load(Ordering::SeqCst) == 0 {
            idle += tick;
            if idle >= linger {
                info!("linger elapsed with no clients");
                ctx.shutdown.cancel();
                return;
            }
        } else {
            idle = Duration::ZERO;
        }
    }
}

/// SIGTERM/SIGINT to the holder: hang up the child and leave.
async fn watch_signals(ctx: Arc<Ctx>) {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM");
    let mut int = signal(SignalKind::interrupt()).expect("SIGINT");
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    info!("holder signalled; shutting down");
    ctx.shutdown.cancel();
}

async fn handle_client(ctx: Arc<Ctx>, stream: UnixStream) {
    ctx.clients.fetch_add(1, Ordering::SeqCst);
    let r = client_loop(&ctx, stream).await;
    ctx.clients.fetch_sub(1, Ordering::SeqCst);
    if let Err(e) = r {
        debug!(error = %e, "client ended");
    }
}

async fn client_loop(ctx: &Ctx, stream: UnixStream) -> std::io::Result<()> {
    let (rd, mut wr) = stream.into_split();
    let mut rd = BufReader::new(rd);

    let (replay, from) = match read_frame(&mut rd).await? {
        Some(Frame::Attach { replay, from }) => (replay, from),
        _ => return Ok(()),
    };

    // Subscribe first, then snapshot under the lock: see pump_output.
    // The replay starts at `from`, or at the oldest byte the ring still has.
    let mut exit_rx = ctx.exit.subscribe();
    let (mut out_rx, snapshot, hello) = {
        let s = ctx.shared.lock().unwrap();
        let total = s.ring.total();
        let oldest = total - s.ring.len() as u64;
        let start = from.clamp(oldest, total);
        let mut hello = s.hello();
        let snapshot = if replay {
            hello.replay_from = start;
            s.ring.snapshot().split_off((start - oldest) as usize)
        } else {
            Vec::new()
        };
        (ctx.out.subscribe(), snapshot, hello)
    };
    let mut exit_sent = hello.exit.is_some();
    write_frame(&mut wr, &Frame::Hello(hello.clone())).await?;
    if replay && !snapshot.is_empty() {
        write_frame(&mut wr, &Frame::Output(snapshot)).await?;
    }
    write_frame(&mut wr, &Frame::ReplayEnd).await?;
    if let Some(exit) = hello.exit {
        write_frame(&mut wr, &Frame::Exit(exit)).await?;
    }

    // The child's own process group (it is a session leader; the PTY setsid'd
    // it): a negative pid signals the group, so `rl stop` reaps a wrapper shell
    // and everything it spawned, not just the leader.
    let group = Pid::from_raw(-(ctx.shared.lock().unwrap().pid as i32));
    let mut out_open = true;
    loop {
        tokio::select! {
            _ = ctx.shutdown.cancelled() => return Ok(()),
            frame = read_frame(&mut rd) => match frame? {
                None => return Ok(()),
                Some(Frame::Input(bytes)) => { let _ = ctx.input.send(bytes).await; }
                Some(Frame::Resize { cols, rows }) => {
                    if let Err(e) = (ctx.resize)(cols, rows) {
                        warn!(error = %e, "resize failed");
                    } else {
                        let mut s = ctx.shared.lock().unwrap();
                        s.cols = cols;
                        s.rows = rows;
                    }
                }
                Some(Frame::Signal(n)) => match Signal::try_from(n) {
                    Ok(sig) => { let _ = kill(group, sig); }
                    Err(_) => warn!(signal = n, "unknown signal"),
                },
                Some(Frame::Close) => { ctx.shutdown.cancel(); return Ok(()); }
                Some(other) => { debug!(?other, "ignoring server-side frame from client"); }
            },
            out = out_rx.recv(), if out_open => match out {
                Ok(chunk) => write_frame(&mut wr, &Frame::Output(chunk.to_vec())).await?,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(dropped = n, "client too slow; disconnecting");
                    return Ok(());
                }
                Err(broadcast::error::RecvError::Closed) => out_open = false,
            },
            changed = exit_rx.changed(), if !exit_sent => {
                if changed.is_err() { exit_sent = true; continue; }
                let exit = exit_rx.borrow().clone();
                if let Some(exit) = exit {
                    // The exit is announced only after the output pump has drained
                    // (see `watch_exit`), so every chunk is already in this client's
                    // broadcast buffer. Flush what is waiting before the exit frame,
                    // or `select!` could send the exit first and the relay — which
                    // closes on it — would drop the child's last words.
                    while let Ok(chunk) = out_rx.try_recv() {
                        write_frame(&mut wr, &Frame::Output(chunk.to_vec())).await?;
                    }
                    write_frame(&mut wr, &Frame::Exit(exit)).await?;
                    exit_sent = true;
                }
            }
        }
    }
}
