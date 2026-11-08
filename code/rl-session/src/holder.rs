//! The holder server: one child, one socket, N attached clients.
//!
//! Output flows child → ring buffer and screen → every attached client. A
//! client that asks gets the screen drawn afresh instead of the ring's bytes.
//! Input, resize and signals flow the other way from whichever client sends
//! them. After the child exits the holder lingers so a client that was away
//! at the time can still attach, replay the tail and read the exit — then it
//! goes away on `Close` or after `linger` with nobody attached.
//!
//! Its socket is the only way anyone reaches it. When that file is gone — the
//! Relay home deleted, a test's temporary home cleaned up — nobody can ever
//! come back for the run, so the holder hangs up its child and leaves rather
//! than wait out a linger (or a child) nobody will collect.
//!
//! The holder also knows its own state. Given a detect spec it feeds the output
//! (and the foreground probe) to a tracker, which works out whether the run is
//! working, idle, stale or waiting on input, and keeps the answer in a state
//! file — so a reader needs no client attached to learn it.
//!

use crate::backend::{self, Resizer, Spec};
use crate::detect::heuristics::{Patterns, Timing};
use crate::detect::tracker::Tracker;
use crate::exit::{Exit, write_exit_file};
use crate::proto::{Frame, FrameReader, Hello, Replay, write_frame, write_output};
use crate::ring::Ring;
use crate::screen::Screen;
use crate::state::write_state_file;
use anyhow::{Context, Result};
use jiff::Timestamp;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use rl_core::state::RunKind;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, broadcast, mpsc, oneshot, watch};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Scrollback kept for replay.
pub const RING_CAPACITY: usize = 1 << 20;
/// Output chunks a slow client may fall behind before it is dropped.
const BROADCAST_CAPACITY: usize = 4096;
/// How often the foreground probe is sampled while a run is active.
const FG_POLL: Duration = Duration::from_secs(1);
/// Consecutive samples with no change and no output before the foreground
/// watcher parks on the next output chunk — a resting shell then costs nothing.
const FG_QUIET_SAMPLES: u32 = 8;
/// How often the tracker's clocks (idle, stale) are checked.
pub const STATE_TICK: Duration = Duration::from_millis(250);
/// How often a changed state is written to the state file.
pub const STATE_FLUSH: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct Config {
    pub id: String,
    pub socket: PathBuf,
    pub exit_file: Option<PathBuf>,
    pub spec: Spec,
    /// After the child exits: how long to stay with no clients attached.
    pub linger: Duration,
    /// How often to check that the socket is still there. Nothing waits on
    /// an orphaned holder leaving, so this can be slow.
    pub socket_check: Duration,
    /// What the holder works out its own run state with. `None`: it does not.
    pub detect: Option<(Patterns, Timing, RunKind)>,
    /// Where the run state is kept, as it changes. Without `detect` nothing
    /// is written.
    pub state_file: Option<PathBuf>,
}

struct Shared {
    id: String,
    pid: u32,
    cols: u16,
    rows: u16,
    started_at: Timestamp,
    ring: Ring,
    /// What the run has drawn so far: the ring holds bytes, this holds the
    /// picture they make.
    screen: Screen,
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
            started_at: self.started_at,
            exit: self.exit.clone(),
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
    /// Whether the shell itself owns the terminal foreground. Starts `true`: a
    /// fresh shell rests at its own prompt.
    fg: watch::Sender<bool>,
    /// Pulsed once per output chunk, so the foreground watcher can park deeply
    /// while the run is quiet and wake the moment the child writes again.
    output_wake: Arc<Notify>,
    /// The holder's own answer to "what state is this run in?". Never locked
    /// while `shared` is held.
    tracker: Option<Mutex<Tracker>>,
}

/// Serves until `Close`, SIGTERM, or the linger timeout after exit. `ready`
/// fires once the socket is bound and the child is running.
pub async fn serve(cfg: Config, ready: oneshot::Sender<()>) -> Result<()> {
    let _ = std::fs::remove_file(&cfg.socket);
    let listener = UnixListener::bind(&cfg.socket).with_context(|| format!("binding {}", cfg.socket.display()))?;
    let spawned = backend::spawn(&cfg.spec)?;
    info!(id = %cfg.id, pid = spawned.pid, argv = ?cfg.spec.argv, "session started");

    let (out_tx, _) = broadcast::channel(BROADCAST_CAPACITY);
    let (exit_tx, _) = watch::channel(None);
    let (fg, _) = watch::channel(true);
    let ctx = Arc::new(Ctx {
        shared: Mutex::new(Shared {
            id: cfg.id.clone(),
            pid: spawned.pid,
            cols: cfg.spec.cols,
            rows: cfg.spec.rows,
            started_at: Timestamp::now(),
            ring: Ring::new(RING_CAPACITY),
            screen: Screen::new(cfg.spec.cols, cfg.spec.rows),
            exit: None,
        }),
        out: out_tx,
        exit: exit_tx,
        input: spawned.input,
        resize: spawned.resize,
        clients: AtomicUsize::new(0),
        shutdown: CancellationToken::new(),
        fg,
        output_wake: Arc::new(Notify::new()),
        tracker: cfg
            .detect
            .clone()
            .map(|(patterns, timing, kind)| Mutex::new(Tracker::new(patterns, timing, kind, Timestamp::now()))),
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
    tokio::spawn(watch_foreground(ctx.clone(), spawned.foreground));
    if let (Some(path), true) = (cfg.state_file.clone(), ctx.tracker.is_some()) {
        tokio::spawn(watch_state(ctx.clone(), path));
    }
    tokio::spawn(watch_signals(ctx.clone()));
    tokio::spawn(watch_socket(ctx.clone(), cfg.socket.clone(), cfg.socket_check));

    loop {
        tokio::select! {
            _ = ctx.shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => { tokio::spawn(handle_client(ctx.clone(), stream)); }
                Err(e) => warn!(error = %e, "accept failed"),
            },
        }
    }

    // Closing with a live child: hang it up, give it a moment.
    if ctx.exit.borrow().is_none() {
        let pid = ctx.shared.lock().unwrap().pid as i32;
        signal_tree(pid, Signal::SIGHUP);
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

/// Signal a run's child and its whole process group (the pty setsid'd it), so
/// a wrapper shell and everything it spawned go together. Both are
/// best-effort.
fn signal_tree(pid: i32, sig: Signal) {
    let _ = kill(Pid::from_raw(pid), sig);
    let _ = kill(Pid::from_raw(-pid), sig);
}

/// Child output → ring + screen + broadcast, under one lock so an attaching
/// client's snapshot and subscription line up with no gap and no duplicate.
async fn pump_output(ctx: Arc<Ctx>, mut output: mpsc::Receiver<Vec<u8>>) {
    while let Some(chunk) = output.recv().await {
        let mut s = ctx.shared.lock().unwrap();
        s.ring.push(&chunk);
        s.screen.feed(&chunk);
        let chunk: Arc<[u8]> = Arc::from(chunk);
        let _ = ctx.out.send(chunk.clone());
        drop(s);
        // The tracker is its own lock, taken once the shared one is let go.
        if let Some(t) = &ctx.tracker {
            t.lock().unwrap().output(&chunk, Timestamp::now());
        }
        // Wake the foreground watcher: it parks on this while the run is quiet.
        ctx.output_wake.notify_one();
    }
    debug!("output pump finished");
}

/// Polls the terminal-foreground probe and publishes `ctx.fg` on change, so an
/// attached client can tell a shell resting at its own prompt from one with a
/// child program in front of it.
///
/// The cost is kept near zero when nothing is happening. With no client and no
/// tracker there is nobody to classify, so it does not poll at all; a tracker
/// needs the value whether or not anyone is attached. Otherwise it samples
/// once a second, but a shell that has sat unchanged for a while is deep-idle:
/// the watcher stops sampling and parks on the next output chunk, waking only
/// when the child writes again.
async fn watch_foreground(ctx: Arc<Ctx>, probe: backend::Foreground) {
    let fg = &ctx.fg;
    let mut poll = tokio::time::interval(FG_POLL);
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // Did a chunk arrive since the last sample? If so the run is not quiet,
    // so the deep-idle park is held off even when the value itself hasn't moved.
    let mut saw_output = false;
    let mut quiet: u32 = 0;
    loop {
        if ctx.shutdown.is_cancelled() {
            return;
        }

        // No client and no tracker: nobody is classifying. Don't touch the
        // probe; wait for a client to attach, re-checking cheaply. A client
        // attaching produces no output, so this can't park on `output_wake`.
        while ctx.tracker.is_none() && ctx.clients.load(Ordering::SeqCst) == 0 {
            tokio::select! {
                _ = ctx.shutdown.cancelled() => return,
                _ = tokio::time::sleep(FG_POLL) => {}
            }
        }

        // Deep idle: the value has held and no output has flowed for a while.
        // Stop sampling and park until the child writes again — zero syscalls
        // and zero wakeups until then.
        if quiet >= FG_QUIET_SAMPLES {
            tokio::select! {
                _ = ctx.shutdown.cancelled() => return,
                _ = ctx.output_wake.notified() => {}
            }
            quiet = 0;
            saw_output = true;
            poll.reset();
            continue;
        }

        // Active: sample about once a second. Output chunks that land between
        // ticks wake us only to note that the run is not quiet; the fixed
        // interval keeps sampling to ~1 Hz even under a steady stream.
        tokio::select! {
            _ = ctx.shutdown.cancelled() => return,
            _ = ctx.output_wake.notified() => {
                saw_output = true;
                continue;
            }
            _ = poll.tick() => {}
        }

        let sample = probe();
        if let (Some(v), Some(t)) = (sample, &ctx.tracker) {
            // The tracker drops a repeat itself; telling it every sample also
            // gives it the first value, which is the one that never "changes".
            t.lock().unwrap().foreground(v);
        }
        let changed = match sample {
            // Publish only on a real change; a `None` leaves the last value be.
            Some(v) => fg.send_if_modified(|cur| {
                let diff = *cur != v;
                if diff {
                    *cur = v;
                }
                diff
            }),
            None => false,
        };
        if changed || saw_output {
            quiet = 0;
        } else {
            quiet = quiet.saturating_add(1);
        }
        saw_output = false;
    }
}

/// Keeps the tracker's clocks running and its state on disk: ticks every
/// [`STATE_TICK`], writes a changed state every [`STATE_FLUSH`], and once more
/// when the child has exited or the holder is shutting down (the file is then
/// left as it is). A write that fails is logged once per kind of error, kept
/// and tried again at the next flush; it never stops the holder.
async fn watch_state(ctx: Arc<Ctx>, path: PathBuf) {
    let Some(tracker) = ctx.tracker.as_ref() else { return };
    let mut exit = ctx.exit.subscribe();
    let mut tick = tokio::time::interval(STATE_TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_flush = tokio::time::Instant::now();
    let mut warned = HashSet::new();
    // The newest snapshot not yet on disk.
    let mut pending = None;
    let mut flush = |tracker: &Mutex<Tracker>| {
        pending = tracker.lock().unwrap().take_dirty().or(pending.take());
        if let Some(s) = &pending {
            match write_state_file(&path, s) {
                Ok(()) => pending = None,
                Err(e) => {
                    if warned.insert(e.kind()) {
                        warn!(error = %e, path = %path.display(), "writing state file");
                    }
                }
            }
        }
    };
    // The child may have exited before this task first ran: `changed()` would
    // never fire for a value already there.
    if exit.borrow().is_some() {
        flush(tracker);
        return;
    }
    loop {
        tokio::select! {
            _ = ctx.shutdown.cancelled() => {
                flush(tracker);
                return;
            }
            changed = exit.changed() => {
                // Exited (or the sender is gone): the last word, then done.
                if changed.is_err() || exit.borrow().is_some() {
                    flush(tracker);
                    return;
                }
            }
            _ = tick.tick() => {
                tracker.lock().unwrap().tick(Timestamp::now());
                if last_flush.elapsed() >= STATE_FLUSH {
                    last_flush = tokio::time::Instant::now();
                    flush(tracker);
                }
            }
        }
    }
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

/// The socket file removed from under the holder: nobody can reach it again,
/// so hang up the child and leave.
async fn watch_socket(ctx: Arc<Ctx>, socket: PathBuf, every: Duration) {
    loop {
        tokio::select! {
            _ = ctx.shutdown.cancelled() => return,
            _ = tokio::time::sleep(every) => {}
        }
        if std::fs::symlink_metadata(&socket).is_err() {
            info!(socket = %socket.display(), "socket removed; nobody can reach this session");
            ctx.shutdown.cancel();
            return;
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
    let mut rd = FrameReader::new(BufReader::new(rd));

    let replay = match rd.next().await? {
        Some(Frame::Attach { replay }) => replay,
        _ => return Ok(()),
    };

    // Subscribe first, then snapshot under the lock: see pump_output.
    let mut exit_rx = ctx.exit.subscribe();
    let (mut out_rx, snapshot, hello) = {
        let mut s = ctx.shared.lock().unwrap();
        let snapshot = match replay {
            // The screen stands in for the bytes: nothing is replayed from
            // the ring.
            Replay::Screen => s.screen.snapshot(),
            Replay::Bytes => s.ring.snapshot(),
            Replay::None => Vec::new(),
        };
        (ctx.out.subscribe(), snapshot, s.hello())
    };
    let mut exit_sent = hello.exit.is_some();
    write_frame(&mut wr, &Frame::Hello(hello.clone())).await?;
    write_output(&mut wr, &snapshot).await?;
    write_frame(&mut wr, &Frame::ReplayEnd).await?;
    if let Some(exit) = hello.exit {
        write_frame(&mut wr, &Frame::Exit(exit)).await?;
    }

    // Foreground: tell a freshly attached client the current state once, then
    // follow it on every change.
    let mut fg_rx = ctx.fg.subscribe();
    let cur = *fg_rx.borrow();
    write_frame(&mut wr, &Frame::Foreground(cur)).await?;

    let pid = ctx.shared.lock().unwrap().pid as i32;
    let mut out_open = true;
    loop {
        tokio::select! {
            _ = ctx.shutdown.cancelled() => return Ok(()),
            frame = rd.next() => match frame? {
                None => return Ok(()),
                Some(Frame::Input(bytes)) => { let _ = ctx.input.send(bytes).await; }
                Some(Frame::Resize { cols, rows }) => {
                    if let Err(e) = (ctx.resize)(cols, rows) {
                        warn!(error = %e, "resize failed");
                    } else {
                        let mut s = ctx.shared.lock().unwrap();
                        s.cols = cols;
                        s.rows = rows;
                        s.screen.resize(cols, rows);
                    }
                }
                Some(Frame::Signal(n)) => match Signal::try_from(n) {
                    Ok(sig) => signal_tree(pid, sig),
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
            // The sender lives in `Ctx` for the holder's life, so this only
            // fails if the run is tearing down: the client's loop ends then.
            fg = fg_rx.changed() => {
                fg.map_err(|_| std::io::ErrorKind::BrokenPipe)?;
                let v = *fg_rx.borrow();
                write_frame(&mut wr, &Frame::Foreground(v)).await?;
            }
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
