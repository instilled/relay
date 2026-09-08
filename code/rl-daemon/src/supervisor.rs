//! The run supervisor: spawns a holder per run, attaches to it, journals its
//! output, and turns its exit into a terminal state and an event.
//!
//! `rld` never owns a child directly — every run lives under `rl-session`, so
//! this module is a client of those holders, nothing more. That is what makes
//! [`Supervisor::reconcile`] possible: a new daemon finds the sessions the old
//! one left in the ledger and picks up where the journal stops.

use crate::ansi;
use crate::git;
use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use relay_core::time::fmt_duration;
use relay_core::{Home, id::new_id};
use relay_ledger::events::NewEvent;
use relay_ledger::session_io::Direction;
use relay_ledger::sessions::SessionRow;
use relay_ledger::{Ledger, events, runs, session_io, sessions};
use relay_proto::methods::RunSpec;
use relay_proto::types::{Confidence, Run, RunKind, RunState, Source, event_kind};
use rl_session::client::{Session, SessionReader, SessionWriter};
use rl_session::exit::{Exit, read_exit_file};
use rl_session::proto::{Frame, Hello};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, info, warn};

/// Env var naming the `rl-session` binary; defaults to a sibling of `rld`.
pub const SESSION_BIN_ENV: &str = "RELAY_SESSION_BIN";
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;
const TITLE_MAX: usize = 80;
const LAST_LINE_MAX: usize = 120;
/// Output is journaled in batches: whichever of these comes first.
const JOURNAL_FLUSH: Duration = Duration::from_millis(100);
const JOURNAL_BATCH_BYTES: usize = 16 * 1024;
/// Chunks a watcher may fall behind before it is dropped.
const OUTPUT_BROADCAST: usize = 1024;
/// How long reconcile waits for one holder to answer.
const REATTACH_TIMEOUT: Duration = Duration::from_secs(2);
/// How long shutdown waits for follow tasks to flush.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// A run whose holder we are attached to right now.
pub struct Live {
    pub id: String,
    pub hello: Hello,
    pub writer: tokio::sync::Mutex<SessionWriter>,
    /// Live output for watchers (step 7); the journal is the durable copy.
    pub output: broadcast::Sender<Arc<[u8]>>,
    /// Set by `stop`, so the exit maps to `stopped` rather than `failed`.
    pub stopping: AtomicBool,
}

pub struct Supervisor {
    home: Home,
    ledger: Ledger,
    session_bin: PathBuf,
    live: Mutex<HashMap<String, Arc<Live>>>,
    /// Cancelled when the daemon is going away: follow tasks flush and detach,
    /// leaving every run exactly as it is.
    shutdown: CancellationToken,
    tasks: TaskTracker,
}

/// What [`Supervisor::reconcile`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// Holders still alive; we are attached again.
    pub reattached: usize,
    /// Runs that ended while no daemon was watching; closed from the sidecar.
    pub ended: usize,
    /// Holders gone with no exit recorded; closed as failed.
    pub lost: usize,
}

impl Supervisor {
    pub fn new(home: Home, ledger: Ledger, session_bin: PathBuf, shutdown: CancellationToken) -> Self {
        Self {
            home,
            ledger,
            session_bin,
            live: Mutex::new(HashMap::new()),
            shutdown,
            tasks: TaskTracker::new(),
        }
    }

    pub fn live(&self, id: &str) -> Option<Arc<Live>> {
        self.live.lock().unwrap().get(id).cloned()
    }

    /// Spawns the holder, records the run, attaches. Returns once the run is
    /// in the ledger with its holder ready.
    pub async fn start(self: &Arc<Self>, spec: RunSpec) -> Result<Run> {
        if spec.argv.is_empty() {
            bail!("argv is empty");
        }
        let cwd = std::fs::canonicalize(&spec.cwd).with_context(|| format!("cwd {}", spec.cwd.display()))?;
        let now = Timestamp::now();
        let id = new_id();
        let place = git::placement(&cwd).await;
        let workspace = place.toplevel.clone().unwrap_or_else(|| cwd.clone());
        let title = spec.title.clone().unwrap_or_else(|| default_title(&spec.argv));

        let mut run = Run::new(&id, spec.kind, title, spec.argv.clone(), cwd.clone(), workspace, now);
        run.branch = place.branch;
        run.ref_ = spec.ref_.clone();
        run.tags = spec.tags.clone();
        if spec.kind == RunKind::Agent {
            run.cli = spec.argv[0].rsplit('/').next().map(String::from);
        }

        let (cols, rows) = (spec.cols.unwrap_or(DEFAULT_COLS), spec.rows.unwrap_or(DEFAULT_ROWS));
        let socket = self.home.session_socket(&id);
        let exit_file = self.home.session_exit_file(&id);
        let log = self.home.session_log(&id);

        let mut env = vec![("RELAY_RUN".to_string(), id.clone())];
        if let Some(r) = &run.ref_ {
            env.push(("RELAY_REF".into(), r.clone()));
        }
        if !run.tags.is_empty() {
            env.push(("RELAY_TAGS".into(), run.tags.join(",")));
        }
        env.extend(spec.env.iter().cloned());

        self.spawn_holder(
            &id, &socket, &exit_file, &log, &cwd, cols, rows, spec.tty, &env, &spec.argv,
        )
        .await?;

        let (hello, session) = Session::attach(&socket, true)
            .await
            .with_context(|| format!("attaching to {}", socket.display()))?;
        run.state = RunState::Working;
        run.source = Source::Pty;
        run.confidence = Confidence::Guess;

        let row = SessionRow {
            run: id.clone(),
            socket: socket.clone(),
            exit_file: exit_file.clone(),
            log,
            pid: hello.pid,
            holder_pid: hello.holder_pid,
            tty: hello.tty,
            cols: hello.cols,
            rows: hello.rows,
            started_at: hello.started_at,
            attached_at: Some(now),
            closed_at: None,
        };
        let started = NewEvent::for_run(&run, now, event_kind::RUN_STARTED, format!("started `{}`", run.title))
            .with_data(json!({ "argv": run.argv, "cwd": run.cwd, "tty": hello.tty, "pid": hello.pid }));
        self.ledger
            .call({
                let run = run.clone();
                move |c| {
                    runs::insert(c, &run)?;
                    sessions::insert(c, &row)?;
                    events::append(c, &started)?;
                    Ok(())
                }
            })
            .await?;
        info!(id = %id, pid = hello.pid, title = %run.title, "run started");
        self.follow(run.clone(), hello, session, exit_file);
        Ok(run)
    }

    /// Picks up every session the previous daemon left open: re-attach where
    /// the holder is alive, resuming the journal from exactly where it stopped;
    /// otherwise close the run from its exit sidecar, or as lost.
    pub async fn reconcile(self: &Arc<Self>) -> Result<Reconciled> {
        let open = self.ledger.call(|c| sessions::list_open(c)).await?;
        let mut report = Reconciled::default();
        for row in open {
            let id = row.run.clone();
            let run = self
                .ledger
                .call({
                    let id = id.clone();
                    move |c| runs::get(c, &id)
                })
                .await?;
            let run = match run {
                Some(r) if !r.state.is_terminal() => r,
                _ => {
                    // Already ended (or never recorded): the session row is stale.
                    let now = Timestamp::now();
                    self.ledger
                        .call({
                            let id = id.clone();
                            move |c| sessions::set_closed(c, &id, now)
                        })
                        .await?;
                    continue;
                }
            };
            let journaled = self
                .ledger
                .call({
                    let id = id.clone();
                    move |c| session_io::out_bytes(c, &id)
                })
                .await?;

            match tokio::time::timeout(REATTACH_TIMEOUT, Session::attach_from(&row.socket, Some(journaled))).await {
                Ok(Ok((hello, session))) => {
                    let gap = hello.replay_from.saturating_sub(journaled);
                    let now = Timestamp::now();
                    self.ledger
                        .call({
                            let id = id.clone();
                            move |c| {
                                if gap > 0 {
                                    let note =
                                        format!("[relay] {gap} bytes of output were lost while the daemon was down");
                                    session_io::append(c, &id, Direction::Note, note.as_bytes(), now)?;
                                }
                                sessions::set_attached(c, &id, now)
                            }
                        })
                        .await?;
                    if gap > 0 {
                        warn!(id, gap, "output lost while away");
                    }
                    info!(
                        id,
                        pid = hello.pid,
                        journaled,
                        resume_at = hello.replay_from,
                        "re-attached"
                    );
                    self.follow(run, hello, session, row.exit_file.clone());
                    report.reattached += 1;
                }
                other => {
                    let why = match other {
                        Ok(Err(e)) => format!("{e:#}"),
                        _ => "timed out".to_string(),
                    };
                    let exit = read_exit_file(&row.exit_file).ok().flatten();
                    if exit.is_some() {
                        report.ended += 1;
                        info!(id, %why, "holder gone; closing from exit sidecar");
                    } else {
                        report.lost += 1;
                        warn!(id, %why, "holder gone and no exit recorded");
                    }
                    self.finish(&run, false, None, exit).await;
                }
            }
        }
        Ok(report)
    }

    /// Asks the child to stop. Returns the run as of now; the terminal state
    /// lands when the holder reports the exit.
    pub async fn stop(&self, id: &str, force: bool) -> Result<Run> {
        let live = self.live(id).ok_or_else(|| anyhow!("no live session for run {id}"))?;
        live.stopping.store(true, Ordering::SeqCst);
        let sig = if force { 9 } else { 15 };
        live.writer
            .lock()
            .await
            .signal(sig)
            .await
            .context("signalling holder")?;
        info!(id, signal = sig, "stop requested");
        self.ledger
            .call({
                let id = id.to_string();
                move |c| runs::get(c, &id)
            })
            .await?
            .ok_or_else(|| anyhow!("run {id} vanished"))
    }

    /// Waits for follow tasks to flush after `shutdown` was cancelled.
    pub async fn drain(&self) {
        self.tasks.close();
        if tokio::time::timeout(DRAIN_TIMEOUT, self.tasks.wait()).await.is_err() {
            warn!("some follow tasks did not flush in time");
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn_holder(
        &self,
        id: &str,
        socket: &Path,
        exit_file: &Path,
        log: &Path,
        cwd: &Path,
        cols: u16,
        rows: u16,
        tty: bool,
        env: &[(String, String)],
        argv: &[String],
    ) -> Result<()> {
        let log_file = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
        let mut cmd = tokio::process::Command::new(&self.session_bin);
        cmd.arg("--id")
            .arg(id)
            .arg("--socket")
            .arg(socket)
            .arg("--exit-file")
            .arg(exit_file)
            .arg("--cwd")
            .arg(cwd)
            .arg("--cols")
            .arg(cols.to_string())
            .arg("--rows")
            .arg(rows.to_string());
        if !tty {
            cmd.arg("--no-tty");
        }
        for (k, v) in env {
            cmd.arg("--env").arg(format!("{k}={v}"));
        }
        cmd.arg("--").args(argv);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log_file));
        let status = cmd
            .status()
            .await
            .with_context(|| format!("spawning {}", self.session_bin.display()))?;
        if !status.success() {
            let tail = std::fs::read_to_string(log).unwrap_or_default();
            let last = tail.lines().last().unwrap_or("no output").to_string();
            bail!("rl-session failed to start: {last}");
        }
        Ok(())
    }

    /// Registers the run as live and starts following its holder.
    fn follow(self: &Arc<Self>, run: Run, hello: Hello, session: Session, exit_file: PathBuf) {
        let (rd, wr) = session.split();
        let (out_tx, _) = broadcast::channel(OUTPUT_BROADCAST);
        let live = Arc::new(Live {
            id: run.id.clone(),
            hello,
            writer: tokio::sync::Mutex::new(wr),
            output: out_tx,
            stopping: AtomicBool::new(false),
        });
        self.live.lock().unwrap().insert(run.id.clone(), live.clone());
        self.tasks.spawn(self.clone().attached(run, live, rd, exit_file));
    }

    /// Follows one holder until its child exits: journal output, keep the
    /// fleet row's last line fresh, then close the run. On daemon shutdown it
    /// flushes and detaches instead — the run is somebody else's now.
    async fn attached(self: Arc<Self>, run: Run, live: Arc<Live>, mut rd: SessionReader, exit_file: PathBuf) {
        let id = run.id.clone();
        let mut batch: Vec<u8> = Vec::new();
        let mut recent = String::new();
        let mut exit: Option<Exit> = None;
        let mut flush = tokio::time::interval(JOURNAL_FLUSH);
        flush.tick().await;

        let detached = loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => break true,
                frame = rd.recv() => match frame {
                    Ok(Some(Frame::Output(bytes))) => {
                        let _ = live.output.send(Arc::from(bytes.as_slice()));
                        batch.extend_from_slice(&bytes);
                        if batch.len() >= JOURNAL_BATCH_BYTES {
                            self.flush(&id, &mut batch, &mut recent).await;
                        }
                    }
                    Ok(Some(Frame::Exit(e))) => { exit = Some(e); break false; }
                    Ok(Some(_)) => {}
                    Ok(None) => { warn!(id, "holder closed the connection"); break false; }
                    Err(e) => { warn!(id, error = %e, "holder connection failed"); break false; }
                },
                _ = flush.tick(), if !batch.is_empty() => {
                    self.flush(&id, &mut batch, &mut recent).await;
                }
            }
        };
        self.flush(&id, &mut batch, &mut recent).await;
        self.live.lock().unwrap().remove(&id);
        if detached {
            info!(id, "detached; run keeps going under its holder");
            return;
        }

        // No Exit frame: the holder went away. The sidecar may still tell us.
        let exit = exit.or_else(|| read_exit_file(&exit_file).ok().flatten());
        let stopping = live.stopping.load(Ordering::SeqCst);
        self.finish(&run, stopping, Some(&live), exit).await;
    }

    async fn flush(&self, id: &str, batch: &mut Vec<u8>, recent: &mut String) {
        if batch.is_empty() {
            return;
        }
        let data = std::mem::take(batch);
        recent.push_str(&ansi::strip(&data));
        if recent.len() > 4096 {
            let cut = recent.len() - 4096;
            let cut = recent.char_indices().map(|(i, _)| i).find(|&i| i >= cut).unwrap_or(0);
            recent.drain(..cut);
        }
        let last = ansi::last_line(recent, LAST_LINE_MAX);
        let id = id.to_string();
        let now = Timestamp::now();
        if let Err(e) = self
            .ledger
            .call(move |c| {
                session_io::append(c, &id, Direction::Out, &data, now)?;
                if let Some(l) = last {
                    runs::set_last_line(c, &id, &l)?;
                }
                Ok(())
            })
            .await
        {
            warn!(error = %e, "journaling output");
        }
    }

    /// Records the end of a run and, if we are still attached, releases the holder.
    async fn finish(&self, run: &Run, stopping: bool, live: Option<&Live>, exit: Option<Exit>) {
        let now = Timestamp::now();
        let (state, kind, summary, data) = match &exit {
            Some(e) => {
                let dur = fmt_duration(e.at.duration_since(run.started_at));
                let how = match &e.signal {
                    Some(s) => s.to_string(),
                    None => format!("exit {}", e.code),
                };
                let data = json!({
                    "exit_code": e.code,
                    "signal": e.signal,
                    "duration_secs": e.at.duration_since(run.started_at).as_secs(),
                });
                if stopping {
                    (
                        RunState::Stopped,
                        event_kind::RUN_STOPPED,
                        format!("stopped after {dur} ({how})"),
                        data,
                    )
                } else if e.success() {
                    (
                        RunState::Finished,
                        event_kind::RUN_FINISHED,
                        format!("finished in {dur} (exit 0)"),
                        data,
                    )
                } else {
                    (
                        RunState::Failed,
                        event_kind::RUN_FAILED,
                        format!("failed after {dur} ({how})"),
                        data,
                    )
                }
            }
            None => (
                RunState::Failed,
                event_kind::RUN_FAILED,
                "session holder went away without reporting an exit".to_string(),
                json!({}),
            ),
        };
        let ended_at = exit.as_ref().map(|e| e.at).unwrap_or(now);
        let exit_code = exit.as_ref().map(|e| e.code);
        let ev = NewEvent::for_run(run, ended_at, kind, summary.clone()).with_data(data);
        let id = run.id.clone();
        let r = self
            .ledger
            .call(move |c| {
                runs::finish(c, &id, state, Source::Pty, exit_code, ended_at)?;
                sessions::set_closed(c, &id, now)?;
                events::append(c, &ev)?;
                Ok(())
            })
            .await;
        if let Err(e) = r {
            warn!(error = %e, "recording run end");
        }
        info!(id = %run.id, state = %state, %summary, "run ended");
        // Everything is journaled; the holder can go.
        if let Some(live) = live
            && let Err(e) = live.writer.lock().await.close().await
        {
            debug!(error = %e, "closing holder");
        }
    }
}

fn default_title(argv: &[String]) -> String {
    let joined = argv.join(" ");
    if joined.chars().count() <= TITLE_MAX {
        joined
    } else {
        let cut: String = joined.chars().take(TITLE_MAX - 1).collect();
        format!("{cut}…")
    }
}

pub fn locate_session_bin() -> PathBuf {
    relay_core::locate_bin("rl-session", SESSION_BIN_ENV)
}
