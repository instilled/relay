//! The run supervisor: spawns a holder per run, attaches to it, journals its
//! output, keeps its state honest, and turns its exit into a terminal state
//! and an event.
//!
//! `rld` never owns a child directly — every run lives under `rl-session`, so
//! this module is a client of those holders, nothing more. That is what makes
//! [`Supervisor::reconcile`] possible: a new daemon finds the sessions the old
//! one left in the ledger and picks up where the journal stops.
//!
//! State changes all go through [`Supervisor::apply`]: PTY heuristics from the
//! follow task, hooks via `run.hook`, humans via `run.mark`. The ledger row is
//! the truth; [`rl_detect::step`] decides against it, on the ledger thread.

use crate::git;
use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use rl_core::time::fmt_duration;
use rl_core::{Home, id::new_id};
use rl_detect::pty::{Heuristics, ShellCommand};
use rl_detect::{Current, Registry, Signal, Transition, claude, step};
use rl_ledger::events::NewEvent;
use rl_ledger::session_io::Direction;
use rl_ledger::sessions::SessionRow;
use rl_ledger::{Ledger, events, runs, session_io, sessions};
use rl_proto::methods::RunSpec;
use rl_proto::types::{Confidence, Run, RunKind, RunState, Source, event_kind};
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
/// Env var naming the `rl` binary hooks call back with; defaults to a sibling of `rld`.
pub const RL_BIN_ENV: &str = "RELAY_RL_BIN";
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;
const TITLE_MAX: usize = 80;
const LAST_LINE_MAX: usize = 120;
/// Output is journaled in batches: whichever of these comes first.
const JOURNAL_FLUSH: Duration = Duration::from_millis(100);
const JOURNAL_BATCH_BYTES: usize = 16 * 1024;
/// How often the PTY heuristics get to look at the clock.
const DETECT_TICK: Duration = Duration::from_millis(250);
/// Chunks a watcher may fall behind before it is dropped.
const OUTPUT_BROADCAST: usize = 1024;
/// Viewer notes kept for a slow viewer.
const NOTES_BROADCAST: usize = 64;
/// How long reconcile waits for one holder to answer.
const REATTACH_TIMEOUT: Duration = Duration::from_secs(2);
/// How long shutdown waits for follow tasks to flush.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// A viewer at the keyboard: who, and from which of their terminals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    pub client: String,
    pub terminal: Option<String>,
}

impl Seat {
    /// How `self` reads to `other`: by name, and — when it is the same person
    /// — as their other terminal.
    pub fn said_to(&self, other: &Seat) -> String {
        if self.client == other.client && self.terminal != other.terminal {
            format!("{} in another terminal", self.client)
        } else {
            self.client.clone()
        }
    }
}

/// A run whose holder we are attached to right now.
pub struct Live {
    pub id: String,
    pub hello: Hello,
    pub writer: tokio::sync::Mutex<SessionWriter>,
    /// Live output for watchers (step 7); the journal is the durable copy.
    pub output: broadcast::Sender<Arc<[u8]>>,
    /// Set by `stop`, so the exit maps to `stopped` rather than `failed`.
    pub stopping: AtomicBool,
    /// Mirror of the ledger's state, so the follow task can skip signals
    /// that would change nothing without a round trip.
    current: Mutex<Current>,
    /// The holder's socket, for viewers to open their own connection.
    pub socket: PathBuf,
    /// Who owns the keyboard. Several may watch; one joins.
    joiner: Mutex<Option<Seat>>,
    /// Remarks for every attached viewer: joined, left, took over.
    pub notes: broadcast::Sender<String>,
    /// The folder the run last said it was in (OSC 7), for the list. Kept in
    /// memory only: a shell says it again at its next prompt.
    cwd: Mutex<Option<PathBuf>>,
}

impl Live {
    pub fn current(&self) -> Current {
        *self.current.lock().unwrap()
    }

    pub fn joiner(&self) -> Option<Seat> {
        self.joiner.lock().unwrap().clone()
    }

    pub fn is_joiner(&self, seat: &Seat) -> bool {
        self.joiner.lock().unwrap().as_ref() == Some(seat)
    }

    /// Claims the keyboard. `Err(current)` when another seat has it — another
    /// person, or another terminal of the same one — and `take` is false;
    /// `Ok(Some(previous))` when taken over.
    pub fn join(&self, seat: &Seat, take: bool) -> Result<Option<Seat>, Seat> {
        let mut j = self.joiner.lock().unwrap();
        match j.as_ref() {
            Some(cur) if cur == seat => Ok(None),
            Some(cur) if !take => Err(cur.clone()),
            prev => {
                let prev = prev.cloned();
                *j = Some(seat.clone());
                Ok(prev)
            }
        }
    }

    /// Releases the keyboard if `seat` holds it.
    pub fn leave(&self, seat: &Seat) -> bool {
        let mut j = self.joiner.lock().unwrap();
        if j.as_ref() == Some(seat) {
            *j = None;
            true
        } else {
            false
        }
    }
}

pub struct Supervisor {
    home: Home,
    ledger: Ledger,
    session_bin: PathBuf,
    registry: Arc<Registry>,
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
    pub fn new(
        home: Home,
        ledger: Ledger,
        session_bin: PathBuf,
        registry: Arc<Registry>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            home,
            ledger,
            session_bin,
            registry,
            live: Mutex::new(HashMap::new()),
            shutdown,
            tasks: TaskTracker::new(),
        }
    }

    pub fn live(&self, id: &str) -> Option<Arc<Live>> {
        self.live.lock().unwrap().get(id).cloned()
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Who holds the keyboard of which run right now.
    /// The folder each live run last said it was in, where one has.
    pub fn cwds(&self) -> HashMap<String, PathBuf> {
        self.live
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(id, l)| l.cwd.lock().unwrap().clone().map(|c| (id.clone(), c)))
            .collect()
    }

    /// The agent process of each live run: the pid its holder spawned.
    pub fn pids(&self) -> std::collections::HashSet<u32> {
        self.live.lock().unwrap().values().map(|l| l.hello.pid).collect()
    }

    pub fn joiners(&self) -> HashMap<String, String> {
        self.live
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(id, l)| l.joiner().map(|j| (id.clone(), j.client)))
            .collect()
    }

    /// Spawns the holder, records the run, attaches. Returns once the run is
    /// in the ledger with its holder ready.
    pub async fn start(self: &Arc<Self>, spec: RunSpec) -> Result<Run> {
        self.start_on(spec, None).await
    }

    /// The same, for a run an event set off: `event` is that event's id, kept
    /// in the run's `run.started` (`data.event`) so what started it is known.
    pub async fn start_on(self: &Arc<Self>, spec: RunSpec, event: Option<i64>) -> Result<Run> {
        if spec.argv.is_empty() {
            bail!("argv is empty");
        }
        let cwd = std::fs::canonicalize(&spec.cwd).with_context(|| format!("cwd {}", spec.cwd.display()))?;
        let now = Timestamp::now();
        let id = new_id().context("generating run id")?;
        let place = git::placement(&cwd).await;
        let workspace = crate::workspace::filed_under(&self.ledger, &cwd, place.toplevel.clone()).await?;
        let title = spec.title.clone().unwrap_or_else(|| default_title(&spec.argv));

        // What is this? A known CLI gets its kind, its name, and — where it
        // offers exact signals — the arguments that switch them on.
        let resolved = self.registry.resolve(spec.kind, &spec.argv);
        let mut argv = spec.argv.clone();
        if let Some(hooks) = &resolved.profile.hooks
            && hooks.dialect == claude::DIALECT
        {
            let file = claude::settings_path(&self.home);
            if file.is_file() {
                let extra = hooks
                    .args
                    .iter()
                    .map(|a| a.replace("{hooks_file}", &file.to_string_lossy()));
                argv.splice(1..1, extra);
            } else {
                warn!(id, "claude hooks settings missing; heuristics only");
            }
        }
        let mut run = Run::new(&id, resolved.kind, title, argv.clone(), cwd.clone(), workspace, now);
        run.cli = resolved.cli.clone();
        run.branch = place.branch;
        run.ref_ = spec.ref_.clone();
        run.tags = spec.tags.clone();
        run.cause = spec.cause.clone();
        if let Some(f) = &spec.chain {
            run.chain = f.clone();
        }

        let (cols, rows) = (spec.cols.unwrap_or(DEFAULT_COLS), spec.rows.unwrap_or(DEFAULT_ROWS));
        let socket = self.home.session_socket(&id);
        let exit_file = self.home.session_exit_file(&id);
        let log = self.home.session_log(&id);

        // The caller's environment, then what hooks and sub-runs need to find
        // their way back. Relay's own names win over anything the caller sent.
        let mut env = spec.env.clone();
        let mut set = |k: &str, v: String| {
            env.retain(|(e, _)| e != k);
            env.push((k.to_string(), v));
        };
        set("RELAY_RUN", id.clone());
        set(rl_core::HOME_ENV, self.home.path().to_string_lossy().into_owned());
        if let Some(r) = &run.ref_ {
            set("RELAY_REF", r.clone());
        }
        if !run.tags.is_empty() {
            set("RELAY_TAGS", run.tags.join(","));
        }
        // A whole environment travels by file, not by argv: it may hold secrets
        // and `ps` shows arguments.
        let env_file = (!spec.inherit_env).then(|| self.home.session_env_file(&id));
        if let Some(f) = &env_file {
            write_private(f, &serde_json::to_vec(&env)?)?;
        }
        let holder = self
            .spawn_holder(
                &id,
                &socket,
                &exit_file,
                &log,
                &cwd,
                cols,
                rows,
                spec.tty,
                spec.input,
                &env,
                env_file.as_deref(),
                &argv,
            )
            .await;
        if let Some(f) = &env_file {
            let _ = std::fs::remove_file(f);
        }
        holder?;

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
        let mut data = json!({ "argv": run.argv, "cwd": run.cwd, "tty": hello.tty, "pid": hello.pid, "cli": run.cli });
        if let Some(id) = event {
            data["event"] = json!(id);
        }
        let started =
            NewEvent::for_run(&run, now, event_kind::RUN_STARTED, format!("started `{}`", run.title)).with_data(data);
        self.ledger
            .call({
                let run = run.clone();
                let auto = spec.title.is_none();
                move |c| {
                    runs::insert(c, &run)?;
                    if auto {
                        runs::set_title_auto(c, &run.id)?;
                    }
                    sessions::insert(c, &row)?;
                    events::append(c, &started)?;
                    Ok(())
                }
            })
            .await?;
        info!(id = %id, pid = hello.pid, kind = %run.kind, cli = run.cli.as_deref().unwrap_or("-"), title = %run.title, "run started");
        self.follow(run.clone(), hello, session, socket, exit_file);
        Ok(run)
    }

    /// Picks up every session the previous daemon left open: re-attach where
    /// the holder is alive, resuming the journal from exactly where it stopped;
    /// otherwise close the run from its exit sidecar, or as lost.
    pub async fn reconcile(self: &Arc<Self>) -> Result<Reconciled> {
        let open = self.ledger.call_read(sessions::list_open).await?;
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
                        self.trouble(
                            &run,
                            format!("{gap} bytes of output were lost while Relay was not running"),
                            json!({ "bytes": gap }),
                        )
                        .await;
                    }
                    info!(
                        id,
                        pid = hello.pid,
                        journaled,
                        resume_at = hello.replay_from,
                        "re-attached"
                    );
                    self.follow(run, hello, session, row.socket.clone(), row.exit_file.clone());
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
                        self.trouble(
                            &run,
                            format!("the session holder went without recording an exit ({why})"),
                            json!({ "why": why }),
                        )
                        .await;
                        // Gone while Relay was not looking — a restart, most
                        // likely — and so something `run.resume` can pick up.
                        let ev = NewEvent::for_run(
                            &run,
                            Timestamp::now(),
                            event_kind::RUN_LOST,
                            format!("{} was lost while Relay was away", run.title),
                        );
                        if let Err(e) = self.ledger.call(move |c| events::append(c, &ev)).await {
                            warn!(error = %e, "recording a lost run");
                        }
                    }
                    self.finish(&run, false, None, exit).await;
                }
            }
        }
        Ok(report)
    }

    /// Asks the child to stop. Returns the run as of now; the terminal state
    /// lands when the holder reports the exit.
    /// Write bytes to a live run's terminal — the warm-shell path: a command
    /// typed on the panel goes to the shell's stdin, journaled as input, with
    /// no interactive join.
    pub async fn input(&self, id: &str, bytes: Vec<u8>) -> Result<()> {
        let live = self.live(id).ok_or_else(|| anyhow!("no live session for run {id}"))?;
        live.writer
            .lock()
            .await
            .input(bytes.clone())
            .await
            .context("writing to holder")?;
        let id = id.to_string();
        let now = Timestamp::now();
        self.ledger
            .call(move |c| session_io::append(c, &id, Direction::In, &bytes, now).map(|_| ()))
            .await?;
        Ok(())
    }

    /// `SIGKILL` when forced. Otherwise what the run leaves on: a shell a
    /// hang-up, as when its terminal closes — an interactive shell shrugs off
    /// `SIGTERM`, and hung up it passes that on to what it runs — anything
    /// else `SIGTERM`.
    pub async fn stop(&self, id: &str, force: bool) -> Result<Run> {
        let sig = if force {
            9
        } else {
            let run = self
                .ledger
                .call({
                    let id = id.to_string();
                    move |c| runs::get(c, &id)
                })
                .await?;
            if run.is_some_and(|r| r.is_shell()) { 1 } else { 15 }
        };
        self.stop_with(id, sig).await
    }

    /// Stops a run the way a closed terminal does, with `SIGHUP`: what an
    /// interactive shell leaves on, where it shrugs off `SIGTERM`.
    pub async fn hang_up(&self, id: &str) -> Result<Run> {
        self.stop_with(id, 1).await
    }

    async fn stop_with(&self, id: &str, sig: i32) -> Result<Run> {
        let live = self.live(id).ok_or_else(|| anyhow!("no live session for run {id}"))?;
        live.stopping.store(true, Ordering::SeqCst);
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

    /// Offers a signal about a run. Decided against the ledger row, on the
    /// ledger thread, so concurrent sources cannot interleave. Returns what
    /// changed, if anything.
    pub async fn apply(&self, id: &str, sig: Signal) -> Result<Option<Transition>> {
        let now = Timestamp::now();
        let t = self
            .ledger
            .call({
                let id = id.to_string();
                move |c| {
                    let run = runs::get(c, &id)?.ok_or_else(|| anyhow!("run {id} not found"))?;
                    // What an exact source says it is doing is kept even when
                    // the state does not move (a Claude turn is many tool
                    // calls, all `working`). The PTY's word is `last_line`.
                    let says = sig
                        .detail
                        .as_deref()
                        .filter(|d| !d.is_empty() && sig.source != Source::Pty);
                    let Some(t) = step(Current::from(&run), &sig) else {
                        if let Some(d) = says
                            && !run.state.is_terminal()
                            && sig.state == run.state
                            && sig.confidence >= run.confidence
                        {
                            runs::set_detail(c, &id, d)?;
                        }
                        return Ok(None);
                    };
                    runs::set_state(c, &id, t.state, t.source, t.confidence, now)?;
                    if let Some(d) = says {
                        runs::set_detail(c, &id, d)?;
                    }
                    if let Some(kind) = t.event_kind() {
                        events::append(c, &NewEvent::for_run(&run, now, kind, t.summary()).with_data(t.data()))?;
                    }
                    Ok(Some(t))
                }
            })
            .await?;
        if let Some(t) = &t {
            if let Some(live) = self.live(id) {
                *live.current.lock().unwrap() = Current {
                    state: t.state,
                    source: t.source,
                    confidence: t.confidence,
                };
            }
            if t.changed() {
                info!(id, from = %t.from, state = %t.state, source = %t.source, confidence = %t.confidence, detail = t.detail.as_deref().unwrap_or(""), "state");
            } else {
                debug!(id, state = %t.state, source = %t.source, confidence = %t.confidence, "confidence upgraded");
            }
        }
        Ok(t)
    }

    /// An agent nobody named takes its first prompt for a title — once: the
    /// title is Relay's to set only until it has, or until you rename the run
    /// yourself, which always wins. True when it was named now.
    /// `cli` stands in for the run's own when it has none.
    pub async fn name_from_prompt(&self, id: &str, cli: &str, prompt: &str) -> Result<bool> {
        let (id, cli, prompt) = (id.to_string(), cli.to_string(), prompt.to_string());
        let named = self
            .ledger
            .call({
                let id = id.clone();
                move |c| {
                    let Some(run) = runs::get(c, &id)? else {
                        return Ok(None);
                    };
                    let cli = run.cli.as_deref().unwrap_or(&cli);
                    match prompt_title(cli, &prompt) {
                        Some(t) if run.kind == RunKind::Agent && runs::name_once(c, &id, &t)? => Ok(Some(t)),
                        _ => Ok(None),
                    }
                }
            })
            .await?;
        if let Some(title) = &named {
            info!(id, title, "named for its first prompt");
        }
        Ok(named.is_some())
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
        input: bool,
        env: &[(String, String)],
        env_file: Option<&Path>,
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
        if !input {
            cmd.arg("--no-input");
        }
        match env_file {
            Some(f) => {
                cmd.arg("--env-file").arg(f);
            }
            None => {
                for (k, v) in env {
                    cmd.arg("--env").arg(format!("{k}={v}"));
                }
            }
        }
        // How long this run waits for a Relay that may never come back: read
        // now, so an edit takes effect on the next run rather than a restart.
        let config = crate::config::load(&self.home).config;
        if let Some(d) = crate::config::reap_after(&config) {
            cmd.arg("--reap-after").arg(d.as_secs().to_string());
        }
        // Zero is "read as fast as the child writes", which is what `off` means.
        cmd.arg("--settle").arg(
            crate::config::settle(&config)
                .unwrap_or_default()
                .as_millis()
                .to_string(),
        );
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
}

/// Writes `bytes` to `path`, readable by the owner only.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    f.write_all(bytes)?;
    Ok(())
}

impl Supervisor {
    /// Registers the run as live and starts following its holder.
    fn follow(self: &Arc<Self>, run: Run, hello: Hello, session: Session, socket: PathBuf, exit_file: PathBuf) {
        let (rd, wr) = session.split();
        let (out_tx, _) = broadcast::channel(OUTPUT_BROADCAST);
        let (notes, _) = broadcast::channel(NOTES_BROADCAST);
        let live = Arc::new(Live {
            id: run.id.clone(),
            hello,
            writer: tokio::sync::Mutex::new(wr),
            output: out_tx,
            stopping: AtomicBool::new(false),
            current: Mutex::new(Current::from(&run)),
            socket,
            joiner: Mutex::new(None),
            cwd: Mutex::new(None),
            notes,
        });
        self.live.lock().unwrap().insert(run.id.clone(), live.clone());
        self.tasks.spawn(self.clone().attached(run, live, rd, exit_file));
    }

    /// Follows one holder until its child exits: journal output, keep the
    /// fleet row's last line fresh, let the heuristics read the stream, then
    /// close the run. On daemon shutdown it flushes and detaches instead —
    /// the run is somebody else's now.
    async fn attached(self: Arc<Self>, run: Run, live: Arc<Live>, mut rd: SessionReader, exit_file: PathBuf) {
        let id = run.id.clone();
        let profile = self.registry.resolve(run.kind, &run.argv).profile;
        let mut heur = Heuristics::new(profile, run.kind, Timestamp::now());
        let mut batch: Vec<u8> = Vec::new();
        let mut exit: Option<Exit> = None;
        // Why the connection went, when it went without an exit.
        let mut gone: Option<String> = None;
        let mut flush = tokio::time::interval(JOURNAL_FLUSH);
        flush.tick().await;
        let mut tick = tokio::time::interval(DETECT_TICK);
        tick.tick().await;

        let detached = loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => break true,
                frame = rd.recv() => match frame {
                    Ok(Some(Frame::Output(bytes))) => {
                        let _ = live.output.send(Arc::from(bytes.as_slice()));
                        batch.extend_from_slice(&bytes);
                        let sig = heur.output(&bytes, Timestamp::now());
                        self.consider(&live, sig).await;
                        // Drained whatever the run is, so marks cannot pile
                        // up on one that would never file them; only a shell's
                        // are worth keeping.
                        let done = heur.take_commands();
                        if !done.is_empty() && run.kind == RunKind::Shell {
                            self.ran(&run, done).await;
                        }
                        // Where it says it is, as the filesystem has it, so a
                        // folder reached through a link (`/tmp` on macOS)
                        // still reads as inside its project.
                        if let Some(cwd) = heur.take_cwd() {
                            let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
                            *live.cwd.lock().unwrap() = Some(cwd);
                        }
                        if batch.len() >= JOURNAL_BATCH_BYTES {
                            self.flush(&id, &mut batch, heur.last_line(LAST_LINE_MAX)).await;
                        }
                    }
                    Ok(Some(Frame::Exit(e))) => { exit = Some(e); break false; }
                    // Whether the shell owns the terminal; the next detect tick
                    // classifies with it.
                    Ok(Some(Frame::Foreground(shell_fg))) => heur.set_foreground(shell_fg),
                    Ok(Some(_)) => {}
                    Ok(None) => { warn!(id, "holder closed the connection"); gone = Some("it closed the connection".to_string()); break false; }
                    Err(e) => { warn!(id, error = %e, "holder connection failed"); gone = Some(format!("{e}")); break false; }
                },
                _ = flush.tick(), if !batch.is_empty() => {
                    self.flush(&id, &mut batch, heur.last_line(LAST_LINE_MAX)).await;
                }
                _ = tick.tick() => {
                    if let Some(sig) = heur.tick(Timestamp::now()) {
                        self.consider(&live, sig).await;
                    }
                }
            }
        };
        self.flush(&id, &mut batch, heur.last_line(LAST_LINE_MAX)).await;
        self.live.lock().unwrap().remove(&id);
        if detached {
            info!(id, "detached; run keeps going under its holder");
            return;
        }

        // No Exit frame: the holder went away. The sidecar may still tell us.
        let exit = exit.or_else(|| read_exit_file(&exit_file).ok().flatten());
        // Only now is it trouble: a holder that wrote its exit and then closed
        // did its job, and the sidecar has just been read.
        if exit.is_none()
            && let Some(why) = gone
        {
            self.trouble(
                &run,
                format!("the session holder went without reporting an exit ({why})"),
                json!({ "why": why }),
            )
            .await;
        }
        let stopping = live.stopping.load(Ordering::SeqCst);
        self.finish(&run, stopping, Some(&live), exit).await;
    }

    /// A PTY signal from the follow task: skipped cheaply when the mirror
    /// says it would change nothing.
    async fn consider(&self, live: &Live, sig: Signal) {
        if step(live.current(), &sig).is_none() {
            return;
        }
        if let Err(e) = self.apply(&live.id, sig).await {
            warn!(id = %live.id, error = %format!("{e:#}"), "applying pty signal");
        }
    }

    async fn flush(&self, id: &str, batch: &mut Vec<u8>, last: Option<String>) {
        if batch.is_empty() {
            return;
        }
        let data = std::mem::take(batch);
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

    /// Files what a shell has run: one event each, and the newest as the run's
    /// detail, which is what its row goes on showing afterwards.
    ///
    /// This writes the detail itself rather than going through [`Self::apply`],
    /// where a PTY signal's detail is deliberately dropped in favour of
    /// `last_line`. That rule is about scraping: a prompt mark is the shell
    /// stating its own exit code, which is a signal like any other.
    async fn ran(&self, run: &Run, done: Vec<ShellCommand>) {
        let id = run.id.clone();
        let run = run.clone();
        let now = Timestamp::now();
        if let Err(e) = self
            .ledger
            .call(move |c| {
                let mut newest = None;
                for cmd in &done {
                    let summary = said(cmd);
                    events::append(
                        c,
                        &NewEvent::for_run(&run, now, rl_proto::types::event_kind::SHELL_COMMAND, summary.clone())
                            .with_data(json!({
                                "command": cmd.text,
                                "exit": cmd.exit,
                                "ms": cmd.took.as_millis() as i64,
                            })),
                    )?;
                    newest = Some(summary);
                }
                if let Some(d) = newest {
                    runs::set_detail(c, &id, &d)?;
                }
                Ok(())
            })
            .await
        {
            warn!(error = %e, "filing what the shell ran");
        }
    }

    /// Records the end of a run and, if we are still attached, releases the holder.
    /// Something went wrong around a run without ending it. A `warn!` is for
    /// whoever reads the log later; this is for whoever has the run in front
    /// of them, so it keeps rather than flashing past.
    async fn trouble(&self, run: &Run, words: impl Into<String>, data: serde_json::Value) {
        let words = words.into();
        warn!(id = %run.id, %words, "run trouble");
        let ev = NewEvent::for_run(run, Timestamp::now(), event_kind::RUN_TROUBLE, words).with_data(data);
        if let Err(e) = self.ledger.call(move |c| events::append(c, &ev)).await {
            warn!(error = %e, "recording run trouble");
        }
    }

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

/// One command as a line: what was typed, how it came out, and how long it
/// took. The exit is left out when the shell's mark did not carry one, rather
/// than shown as a zero it never said.
fn said(cmd: &ShellCommand) -> String {
    let mut s = cmd.text.clone();
    if let Some(e) = cmd.exit {
        s.push_str(&format!(" · exit {e}"));
    }
    s.push_str(&format!(" · {}", fmt_duration(cmd.took)));
    s
}

fn default_title(argv: &[String]) -> String {
    clip_title(&argv.join(" "))
}

/// A title no longer than `TITLE_MAX`, cut with `…`.
fn clip_title(s: &str) -> String {
    if s.chars().count() <= TITLE_MAX {
        s.to_string()
    } else {
        let cut: String = s.chars().take(TITLE_MAX - 1).collect();
        format!("{cut}…")
    }
}

/// What an agent is called for what it was first asked: `claude: add a
/// recipe import`, the first line of the prompt. Nothing for a prompt with
/// nothing in it, or a slash command — `/model` or `/clear` says how to run,
/// not what the run is for, so the title waits for the first real prompt.
pub fn prompt_title(cli: &str, prompt: &str) -> Option<String> {
    let first = prompt.trim().lines().next()?.trim();
    if first.is_empty() || first.starts_with('/') {
        return None;
    }
    Some(clip_title(&format!("{cli}: {first}")))
}

pub fn locate_session_bin() -> PathBuf {
    rl_core::locate_bin("rl-session", SESSION_BIN_ENV)
}

pub fn locate_rl_bin() -> PathBuf {
    rl_core::locate_bin("rl", RL_BIN_ENV)
}

#[cfg(test)]
mod tests {
    use super::{TITLE_MAX, prompt_title};

    #[test]
    fn an_agent_is_named_for_the_first_line_of_what_it_was_asked() {
        assert_eq!(
            prompt_title("claude", "  add a recipe import\nwith photos\n").as_deref(),
            Some("claude: add a recipe import")
        );
        assert_eq!(prompt_title("claude", "   \n  "), None, "nothing asked");
        assert_eq!(
            prompt_title("claude", "/model opus"),
            None,
            "a slash command is not the work"
        );
        let long = prompt_title("claude", &"word ".repeat(40)).unwrap();
        assert_eq!(long.chars().count(), TITLE_MAX);
        assert!(long.starts_with("claude: word word") && long.ends_with('…'), "{long}");
    }
}
