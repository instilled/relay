//! A harness — a CLI Relay runs or lists: everything Relay does with one, in
//! one place. One implementation per kind:
//!
//! - [`crate::claude_native::Claude`]: Claude Code, which has a background
//!   of its own (`claude --bg`, `claude attach`, `claude stop`, `claude rm`,
//!   `claude agents --json`); its MCP server goes through `claude mcp`; a caller of
//!   `rl mcp` is also known by `CLAUDE_CODE_SESSION_ID`; mail reaches its
//!   sessions through its hooks.
//! - [`crate::codex_native::Codex`]: Codex, whose app-server daemon runs
//!   every thread (Relay never holds one); its MCP server goes in
//!   `config.toml`.
//! - [`Held`]: every other profile (Gemini, Antigravity, cursor, opencode,
//!   the shells, `generic`): no background of its own, so Relay's holder runs
//!   what it starts; all it does comes from its profile (`bins`, `prompt`,
//!   `resume`, `hooks`, `sessions.hooks`, `sessions.ping`, `mcp`).
//!
//! Which a profile is, is its `sessions.list.reader` (`of`): the readers are
//! the two harnesses with a listing of their own.
//!
//! A native row says only what the harness says of it, in its words: Relay
//! re-labels nothing, and a field whose source is missing stays empty.

use crate::Registry;
use crate::mcp_entry::{How, Place};
use crate::registry::{Profile, find_bin};
pub use crate::sessions::Choice;
use crate::sessions::{ID, NAME, Rename, STOP_KEY, ScreenMark, TEXT, UserHooks};
use crate::settings;
use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use rl_core::Home;
use rl_core::state::{RunKind, RunState};
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Which of a run's pipes some bytes came from.
#[derive(Debug, Clone, Copy)]
enum Pipe {
    Out,
    Err,
}

/// How often a run looks whether it has exited.
const TICK: Duration = Duration::from_millis(20);
/// How long a run's pipes are read once it has exited.
const AFTER_EXIT: Duration = Duration::from_millis(200);

/// A pull request a session opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pr {
    pub number: u64,
    pub href: String,
}

/// A harness's own words for how one of its sessions stands, and the
/// `RunState` it maps to — for grouping and the icon only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeState {
    pub words: String,
    pub state: RunState,
}

/// One of a harness's own background sessions, as it lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRow {
    pub harness: String,
    /// The harness's id for it, what attaching takes.
    pub id: String,
    /// Its conversation's id, what hooks tell of.
    pub session_id: String,
    pub name: String,
    pub cwd: PathBuf,
    pub state: NativeState,
    pub summary: Option<String>,
    pub started: Option<Timestamp>,
    pub ended: Option<Timestamp>,
    /// When it came to stand as it does.
    pub at: Option<Timestamp>,
    pub pr: Option<Pr>,
    /// The colour its harness draws it in, by name (Claude's palette:
    /// `pink`); none, the usual.
    pub color: Option<String>,
}

/// What a start began.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    /// A session in its harness's background, by this id.
    Id(String),
    /// Nothing there yet: this, run in this terminal in the folder asked,
    /// begins one itself (Codex with no prompt: its TUI makes the thread).
    Here(Vec<String>),
    /// Nothing: Relay is to hold it, its binary run with these arguments.
    Held(Vec<String>),
}

/// How a conversation open in another terminal carries on, once it rests
/// there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarryOn {
    /// This, run in the harness's own background (`Harness::carry_on_there`).
    Background(Vec<String>),
    /// This, in a session Relay holds.
    Held(Vec<String>),
    /// This, run here: the conversation is in the harness's background
    /// already (Codex's thread on its daemon).
    Attach(Vec<String>),
}

/// What came of waking one of a harness's sessions for relay mail
/// (`Harness::wake`), in words for `send` to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Woke {
    /// The mail's line was typed into it, idle.
    Typed,
    /// It had stopped: it carries on in its harness's background, the line
    /// its prompt.
    Resumed,
    /// Left to its hooks, which tell it at the end of its turn: why.
    Left(String),
    /// It could have been woken, and was not: why.
    Failed(String),
}

/// Everything Relay does with one harness. `list`, `start`, `carry_on_there`,
/// `stop`, `close`, `rename`, `type_into` and `wake` run the harness and may
/// block; they are asked from a background task. Nothing else runs
/// anything.
pub trait Harness: Send + Sync {
    /// Its profile.
    fn profile(&self) -> &Profile;
    /// The binary it runs: the one found on this machine, else its
    /// profile's first.
    fn bin(&self) -> &str;
    /// Whether its binary is on this machine.
    fn found(&self) -> bool;
    /// Its name: `claude`.
    fn name(&self) -> &str {
        &self.profile().name
    }
    /// What its sessions are: agents, shells, commands.
    fn kind(&self) -> RunKind {
        self.profile().kind
    }

    // Its own sessions.

    /// Whether it lists sessions of its own (it has a background).
    fn lists(&self) -> bool {
        false
    }
    /// Its background sessions, as it lists them.
    fn list(&self) -> Result<Vec<NativeRow>> {
        self.list_within(LIST_WAIT)
    }
    /// The same, for no longer than `wait`. None but its own: empty.
    fn list_within(&self, wait: Duration) -> Result<Vec<NativeRow>> {
        let _ = wait;
        Ok(Vec::new())
    }
    /// Starts a session of it in `cwd`, named and prompted if given: what it
    /// began. `before`: the ids its last listing had, which the new one is
    /// not. Without a background of its own: `Started::Held`, its profile's
    /// prompt arguments, for Relay to hold.
    fn start(&self, cwd: &Path, name: Option<&str>, prompt: Option<&str>, before: &HashSet<String>) -> Result<Started> {
        let _ = (cwd, name, before);
        Ok(Started::Held(self.profile().prompt_args(prompt.unwrap_or(""))))
    }
    /// Whether Relay never holds a session of it: what starts one starts it
    /// in its background or in this terminal (Codex: its daemon runs every
    /// thread).
    fn never_held(&self) -> bool {
        false
    }
    /// What a session of it Relay holds runs, from `argv` (its binary and
    /// arguments): with the hooks its profile's `hooks` says it reports
    /// through (`claude --settings <file>`), the file written under `home`
    /// for `rl` (asked only then).
    fn held_argv(&self, home: &Home, mut argv: Vec<String>, rl: &dyn Fn() -> Result<PathBuf>) -> Result<Vec<String>> {
        if let Some(hooks) = &self.profile().hooks
            && hooks.dialect == crate::claude::DIALECT
        {
            let file = crate::claude::write_settings(home, &rl()?)?;
            let extra = hooks
                .args
                .iter()
                .map(|a| a.replace("{hooks_file}", &file.to_string_lossy()));
            let at = argv.len().min(1);
            argv.splice(at..at, extra);
        }
        Ok(argv)
    }
    /// What attaches this terminal to its session `id`. None: it has none
    /// of its own.
    fn attach_argv(&self, id: &str) -> Option<Vec<String>> {
        let _ = id;
        None
    }
    /// How an attached session of it says it shows its own list of
    /// sessions (Claude's agents view, which `←` opens): the list it shows
    /// is Relay's, so the attach lets go and Relay's comes back. Its
    /// profile's `sessions.agents_view`; none where Relay does not know.
    fn agents_view(&self) -> Option<&ScreenMark> {
        self.profile().sessions.as_ref()?.agents_view.as_ref()
    }
    /// How its conversation `session_id`, open in another terminal, carries
    /// on: by default its profile's `resume`, held by Relay. None: no way.
    fn carry_on(&self, session_id: &str) -> Option<CarryOn> {
        self.profile()
            .resume_argv(self.bin().to_string(), session_id)
            .map(CarryOn::Held)
    }
    /// Runs `argv`, a `CarryOn::Background`, in `cwd`: the id of the session
    /// it began there.
    fn carry_on_there(&self, argv: &[String], cwd: &Path, before: &HashSet<String>) -> Result<String> {
        let _ = (argv, cwd, before);
        bail!("{} carries nothing on in its background", self.name())
    }
    /// Whether `stop` would stop `row`, as it stands.
    fn stops(&self, row: &NativeRow) -> bool {
        let _ = row;
        false
    }
    /// Stops its session `id`, in `cwd` (Claude: `claude stop`; Codex:
    /// interrupts its running turn).
    fn stop(&self, id: &str, cwd: &Path) -> Result<()> {
        let _ = (id, cwd);
        bail!("nothing stops a {} session from here", self.name())
    }
    /// How `^x` twice offers to stop one of its sessions: `s` and its
    /// profile's `sessions.stop_choice` words (Claude: `stop · keeps the
    /// conversation`), else `s stop`.
    fn stop_choice(&self) -> Choice {
        self.profile()
            .sessions
            .as_ref()
            .and_then(|s| s.stop_choice.clone())
            .unwrap_or_else(|| Choice {
                key: STOP_KEY,
                words: "stop".into(),
                says: None,
                done: "stopped".into(),
            })
    }
    /// What `^x` twice offers beside the stop to close `row` for good, or off
    /// its lists (Claude: `r remove`; Codex: `a archive`): its profile's
    /// `sessions.close`. None: it has no way to.
    fn close_choice(&self, row: &NativeRow) -> Option<Choice> {
        let _ = row;
        Some(self.profile().sessions.as_ref()?.close.as_ref()?.choice.clone())
    }
    /// Closes its session `id`, in `cwd`: its profile's `sessions.close`
    /// arguments run with its binary (`claude rm <id>`, `codex archive
    /// <id>`), for no longer than a list may take.
    fn close(&self, id: &str, cwd: &Path) -> Result<()> {
        let Some(close) = self.profile().sessions.as_ref().and_then(|s| s.close.as_ref()) else {
            bail!("nothing closes a {} session from here", self.name());
        };
        let argv: Vec<String> = std::iter::once(self.bin().to_string())
            .chain(close.args.iter().map(|a| a.replace(ID, id)))
            .collect();
        run(&argv, Some(cwd), false, LIST_WAIT).map(drop)
    }
    /// What tells its session `id` `text` from outside it: its profile's
    /// `sessions.ping` (codex: queues it on the thread). None: nothing
    /// outside it can.
    fn ping_argv(&self, id: &str, text: &str) -> Option<Vec<String>> {
        let ping = &self.profile().sessions.as_ref()?.ping;
        if ping.is_empty() {
            return None;
        }
        let args = ping.iter().map(|a| a.replace(ID, id).replace(TEXT, text));
        Some(std::iter::once(self.bin().to_string()).chain(args).collect())
    }

    /// Whether `rename` would rename `row` to `name` in the harness itself,
    /// as it stands (its profile's `sessions.rename`): a call on its daemon
    /// always; a line typed only into one that is idle, and never to clear
    /// a name.
    fn renames(&self, row: &NativeRow, name: &str) -> bool {
        match self.profile().sessions.as_ref().and_then(|s| s.rename.as_ref()) {
            Some(Rename::Rpc(_)) => true,
            Some(Rename::Type(_)) => !name.trim().is_empty() && row.state.state == RunState::Idle,
            None => false,
        }
    }
    /// Renames its session `row` to `name` in the harness itself, as its
    /// profile's `sessions.rename` says; `locks` is where typing keeps its
    /// lock files. A harness whose rename is a call on its daemon makes the
    /// call itself (Codex).
    fn rename(&self, row: &NativeRow, name: &str, locks: &Path) -> Result<()> {
        let who = self.name();
        match self.profile().sessions.as_ref().and_then(|s| s.rename.as_ref()) {
            Some(Rename::Type(line)) if !name.trim().is_empty() => {
                self.type_into(row, &line.replace(NAME, name.trim()), locks)
            }
            Some(Rename::Type(_)) => bail!("{who} has no way to clear a name"),
            Some(Rename::Rpc(method)) => bail!("{who} renames through `{method}` on a daemon Relay has no way to"),
            None => bail!("{who} has no way to rename one of its sessions"),
        }
    }
    /// Types `line` into its session `row`, idle, through its attach on a
    /// hidden terminal (`typing::type_line`), as its profile's
    /// `sessions.typing` reads the screen.
    fn type_into(&self, row: &NativeRow, line: &str, locks: &Path) -> Result<()> {
        let typing = self
            .profile()
            .sessions
            .as_ref()
            .and_then(|s| s.typing.as_ref())
            .ok_or_else(|| anyhow!("{} says nothing of how to type into one", self.name()))?;
        if row.state.state != RunState::Idle {
            bail!("it is {}, not idle", row.state.words);
        }
        let argv = self
            .attach_argv(&row.id)
            .ok_or_else(|| anyhow!("{} has no attach", self.name()))?;
        let cwd = if row.cwd.is_dir() {
            row.cwd.as_path()
        } else {
            Path::new("/")
        };
        let lock = crate::typing::lock_path(locks, self.name(), &row.id);
        crate::typing::type_line(&argv, cwd, typing, line, &lock)
    }
    /// Wakes its session `row` for relay mail, `line` the words that say so,
    /// as its profile's `sessions.wake` says: an idle one has the line typed
    /// into it, one that has stopped is carried on in its background with
    /// the line as its prompt; one working or waiting on you is left to its
    /// hooks. None: it has no way to be woken (Codex is pinged instead).
    fn wake(&self, row: &NativeRow, line: &str, locks: &Path) -> Option<Woke> {
        let wake = &self.profile().sessions.as_ref()?.wake;
        if wake.idle.is_none() && wake.stopped.is_none() {
            return None;
        }
        let who = self.name();
        Some(match row.state.state {
            RunState::Idle if wake.idle.is_some() => match self.type_into(row, line, locks) {
                Ok(()) => Woke::Typed,
                Err(e) => Woke::Failed(format!("{who} wasn't woken: {e:#}")),
            },
            s if s.is_terminal() && wake.stopped.is_some() => match self.resume_with(row, line) {
                Ok(()) => Woke::Resumed,
                Err(e) => Woke::Failed(format!("{who} wasn't carried on: {e:#}")),
            },
            RunState::NeedsInput => Woke::Left(format!("{who} waits on you; it will see it once you answer")),
            RunState::Working | RunState::Starting => {
                Woke::Left(format!("{who} is busy; it will see it at the end of its turn"))
            }
            _ => Woke::Failed(format!("{who} is {}; nothing wakes it from there", row.state.words)),
        })
    }
    /// Carries its stopped session `row` on in its background with `line` as
    /// its prompt (Claude's `claude --resume <session> --bg <line>`).
    fn resume_with(&self, row: &NativeRow, line: &str) -> Result<()> {
        let Some(CarryOn::Background(mut argv)) = self.carry_on(&row.session_id) else {
            bail!("{} has no background to carry it on in", self.name());
        };
        argv.extend(self.profile().prompt_args(line));
        run(&argv, Some(&row.cwd), false, START_WAIT).map(drop)
    }

    // Setup.

    /// The hooks that tell of its own sessions, set in its user-level file
    /// by `rl setup` (its profile's `sessions.hooks`).
    fn user_hooks(&self) -> Option<&UserHooks> {
        self.profile().sessions.as_ref()?.hooks.as_ref()
    }
    /// Whether it has a place for Relay's MCP server (its profile's `mcp`).
    fn has_mcp(&self) -> bool {
        self.profile().mcp.is_some()
    }
    /// Where Relay's MCP server goes for it, and how, as `env` finds its
    /// file; None where it has no place. An error where its file is not
    /// known (`HOME` not set). Without a harness of its own: written into
    /// its profile's `mcp` file, or through its CLI's `mcp.cli` words.
    fn mcp(&self, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Option<Place>> {
        let how = match self.profile().mcp.as_ref() {
            Some(m) if !m.cli.is_empty() => How::Cli {
                cli: cli_name(self),
                bin: self.found().then(|| PathBuf::from(self.bin())),
                words: m.cli.clone(),
            },
            _ => How::Json,
        };
        mcp_place(self, env, how)
    }

    /// Where the keys `rl setup` sets in its own global config go, and
    /// which (its profile's `settings`); None where it has none. An error
    /// where its file is not known (`HOME` not set).
    fn settings(&self, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Option<settings::Place>> {
        let Some(s) = self.profile().settings.as_ref() else {
            return Ok(None);
        };
        let name = self.name();
        let path = s
            .file(env)
            .ok_or_else(|| anyhow!("{name}: HOME is not set, so where its global config is is not known"))?;
        Ok(Some(settings::Place {
            path,
            set: s.set.clone(),
        }))
    }

    // Mail and `rl mcp`.

    /// The variable its sessions set to their conversation's id, which its
    /// hooks tell of too: how `rl mcp` knows a caller Relay does not hold
    /// (`CLAUDE_CODE_SESSION_ID`).
    fn caller_id_var(&self) -> Option<&'static str> {
        None
    }
    /// Whether its sessions hear of relay mail from its hooks (`rl hook`'s
    /// news): it has no way in from outside (Claude).
    fn mail_by_hooks(&self) -> bool {
        false
    }
    /// What a caller of this harness is told to do to wake its own session
    /// `name` for mail, where it has a way Relay cannot run.
    fn wake_advice(&self, name: &str) -> Option<String> {
        let _ = name;
        None
    }
}

/// The name its own CLI goes by: its profile's first binary.
pub(crate) fn cli_name(h: &(impl Harness + ?Sized)) -> String {
    h.profile()
        .bins
        .first()
        .cloned()
        .unwrap_or_else(|| h.name().to_string())
}

/// Its MCP place, `how`, in the file its profile's `mcp` names as `env`
/// finds it; None where its profile names none.
pub(crate) fn mcp_place(
    h: &(impl Harness + ?Sized),
    env: &dyn Fn(&str) -> Option<OsString>,
    how: How,
) -> Result<Option<Place>> {
    let Some(m) = h.profile().mcp.as_ref() else {
        return Ok(None);
    };
    let name = h.name();
    let path = m
        .file(env)
        .ok_or_else(|| anyhow!("{name}: HOME is not set, so where its MCP servers are kept is not known"))?;
    Ok(Some(Place {
        harness: name.to_string(),
        path,
        how,
        note: m.note.clone(),
    }))
}

/// A harness without a background of its own: Relay holds what it starts,
/// and its profile says the rest.
#[derive(Debug, Clone)]
pub struct Held {
    profile: Profile,
    bin: String,
    found: bool,
}

impl Harness for Held {
    fn profile(&self) -> &Profile {
        &self.profile
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    fn found(&self) -> bool {
        self.found
    }
}

/// How long a list may take.
pub const LIST_WAIT: Duration = Duration::from_secs(10);
/// How long a start or a carry-on may take to say its id.
pub const START_WAIT: Duration = Duration::from_secs(30);

/// Whether two paths name one folder, through links.
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// The harness of `profile`, its binary `found` on this machine (or not:
/// then its profile's first, so what it would run reads the same
/// everywhere). Its `sessions.list.reader` says which kind it is.
pub fn of(profile: &Profile, found: Option<PathBuf>) -> Box<dyn Harness> {
    let here = found.is_some();
    let bin = found
        .or_else(|| profile.bins.first().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(&profile.name));
    let reader = profile
        .sessions
        .as_ref()
        .and_then(|s| s.list.as_ref())
        .map(|l| l.reader.as_str());
    let own: Option<Box<dyn Harness>> = match reader {
        Some(crate::claude_native::READER) => {
            crate::claude_native::Claude::of(profile, bin.clone()).map(|c| Box::new(c.found(here)) as _)
        }
        Some(crate::codex_native::READER) => {
            crate::codex_native::Codex::of(profile, bin.clone()).map(|c| Box::new(c.found(here)) as _)
        }
        _ => None,
    };
    own.unwrap_or_else(|| {
        Box::new(Held {
            profile: profile.clone(),
            bin: bin.to_string_lossy().into_owned(),
            found: here,
        })
    })
}

/// Every harness in `registry`, in name order, its binary the one `find`
/// finds of its profile's `bins` (`registry::find_bin`: on this machine's
/// `PATH`).
pub fn harnesses(registry: &Registry, find: impl Fn(&[String]) -> Option<PathBuf>) -> Vec<Box<dyn Harness>> {
    registry
        .names()
        .filter_map(|n| registry.profile(n))
        .map(|p| of(p, find(&p.bins)))
        .collect()
}

/// Every harness in `registry`, found on this machine's `PATH`.
pub fn on_path(registry: &Registry) -> Vec<Box<dyn Harness>> {
    harnesses(registry, find_bin)
}

/// Kills `child` and everything it started (it leads a group of its own,
/// `process_group(0)`), and reaps it.
pub(crate) fn kill_group(child: &mut std::process::Child) {
    let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs `argv` in `cwd` (this process's, without one), its input closed,
/// for no longer than `wait`, and returns what it printed. With `asking`,
/// any hook it fires knows it for Relay's own ask (`user_hooks::ASKING`),
/// not a session to tell of. A run that fails or takes too long is an
/// error named by its first two words: `claude agents`.
///
/// One that fails as a CLI does on words it does not know (`usage_error`)
/// is drift: the harness no longer takes what its profile says
/// (`<program>.cli.<word>`).
pub fn run(argv: &[String], cwd: Option<&Path>, asking: bool, wait: Duration) -> Result<String> {
    run_watched(argv, cwd, asking, wait, true)
}

/// `run`, with nothing noticed of how it fails: for what Relay asks of a
/// harness only to learn about it (`--version`).
pub fn run_quiet(argv: &[String], wait: Duration) -> Result<String> {
    run_watched(argv, None, true, wait, false)
}

/// Whether a run that exited `code` with `err` on its stderr failed on its
/// words — a subcommand or option it does not know — rather than at its
/// work: clap's usage exit (2) with its usage, or the errors clap and
/// commander print for one.
pub fn usage_error(code: Option<i32>, err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    let said = [
        "unrecognized subcommand",
        "unexpected argument",
        "unknown command",
        "unknown option",
        "unrecognized option",
        "invalid subcommand",
    ]
    .iter()
    .any(|w| e.contains(w));
    said || (code == Some(2) && e.contains("usage:"))
}

fn run_watched(argv: &[String], cwd: Option<&Path>, asking: bool, wait: Duration, watch: bool) -> Result<String> {
    let (bin, args) = argv.split_first().ok_or_else(|| anyhow!("nothing to run"))?;
    let program = Path::new(bin)
        .file_name()
        .map_or_else(|| bin.clone(), |n| n.to_string_lossy().into_owned());
    let what = match args.first() {
        Some(a) => format!("{program} {a}"),
        None => program,
    };
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // What a Relay session says of itself is not the harness's.
        .env_remove("RL_SESSION")
        .env_remove("RL_SESSION_AGENT");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if asking {
        cmd.env(crate::user_hooks::ASKING, crate::user_hooks::asking_since());
    }
    // A group of its own, so what it starts goes with it when it hangs.
    cmd.process_group(0);
    let mut child = cmd.spawn().with_context(|| format!("running `{what}`"))?;
    // Each pipe read on a thread of its own, its bytes sent as they come and
    // `None` at its end: nothing here waits on a read, only on the channel.
    let (tx, rx) = mpsc::channel::<(Pipe, Option<Vec<u8>>)>();
    let read = |which: Pipe, r: Option<Box<dyn Read + Send>>| {
        let tx = tx.clone();
        std::thread::spawn(move || {
            if let Some(mut r) = r {
                let mut buf = [0u8; 8192];
                while let Ok(n @ 1..) = r.read(&mut buf) {
                    if tx.send((which, Some(buf[..n].to_vec()))).is_err() {
                        return;
                    }
                }
            }
            let _ = tx.send((which, None));
        });
    };
    read(Pipe::Out, child.stdout.take().map(|o| Box::new(o) as _));
    read(Pipe::Err, child.stderr.take().map(|e| Box::new(e) as _));
    drop(tx);
    let deadline = Instant::now() + wait;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut open = 2;
    let mut status = None;
    // Once it has exited, what it printed is in the pipes or never will be:
    // something it left running may hold them open (a daemon it started),
    // which is no reason to wait.
    let mut until = deadline;
    while open > 0 || status.is_none() {
        let now = Instant::now();
        if status.is_none() {
            if let Some(s) = child.try_wait()? {
                status = Some(s);
                until = until.min(now + AFTER_EXIT);
                continue;
            }
            if now >= deadline {
                kill_group(&mut child);
                bail!("`{what}` did not answer in {}s", wait.as_secs());
            }
        } else if now >= until {
            break;
        }
        let left = until.saturating_duration_since(now).min(TICK);
        if open == 0 {
            std::thread::sleep(left);
            continue;
        }
        match rx.recv_timeout(left) {
            Ok((which, Some(bytes))) => match which {
                Pipe::Out => out.extend(bytes),
                Pipe::Err => err.extend(bytes),
            },
            Ok((_, None)) => open -= 1,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => open = 0,
        }
    }
    let Some(status) = status else {
        bail!("`{what}` did not answer in {}s", wait.as_secs());
    };
    let out = String::from_utf8_lossy(&out).into_owned();
    let err = String::from_utf8_lossy(&err).into_owned();
    if !status.success() {
        if watch && usage_error(status.code(), &err) {
            let program = Path::new(bin).file_name().map(|n| n.to_string_lossy().into_owned());
            let word = args.first().map(|a| a.trim_start_matches('-')).unwrap_or("");
            let program = program.unwrap_or_default();
            crate::drift::notice(
                crate::drift::Noticed::new(
                    &program,
                    Some(Path::new(bin)),
                    &format!("{program}.cli.{word}"),
                    format!(
                        "`{what}` is not taken as Relay runs it (exit {})",
                        status.code().unwrap_or(-1)
                    ),
                )
                .with_evidence(err.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim()),
            );
        }
        bail!("`{what}` failed: {}", err.lines().next().unwrap_or("no reason given"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_says_what_it_printed_and_why_it_did_not() {
        let sh = |s: &str| vec!["sh".to_string(), "-c".into(), s.into()];
        let tmp = tempfile::tempdir().unwrap();
        let said = run(&sh("pwd; echo \"$RELAY_ASKING\""), Some(tmp.path()), true, LIST_WAIT).unwrap();
        let mut lines = said.lines();
        let here = std::fs::canonicalize(tmp.path()).unwrap();
        assert_eq!(lines.next().map(PathBuf::from), Some(here), "in the folder asked");
        assert!(
            lines.next().is_some_and(|l| l.parse::<u64>().is_ok()),
            "its hooks know it for an ask"
        );
        let e = run(&sh("echo nope >&2; exit 3"), None, false, LIST_WAIT).unwrap_err();
        assert_eq!(e.to_string(), "`sh -c` failed: nope");
        let at = Instant::now();
        let e = run(&sh("sleep 5"), None, false, Duration::from_millis(200)).unwrap_err();
        assert!(e.to_string().contains("did not answer"), "{e}");
        assert!(at.elapsed() < Duration::from_secs(4), "given up at the wait");
    }

    #[test]
    fn a_subcommand_the_harness_no_longer_takes_is_drift_and_a_failure_at_its_work_is_not() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("codex");
        std::fs::write(
            &bin,
            "#!/bin/sh\n[ \"$1\" = archive ] && { echo \"error: unrecognized subcommand 'archive'\" >&2; echo 'Usage: codex [OPTIONS]' >&2; exit 2; }\necho 'no thread named x' >&2; exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let argv = |w: &str| vec![bin.display().to_string(), w.into(), "x".into()];
        let (said, noticed) = crate::drift::capture(|| run(&argv("archive"), None, false, LIST_WAIT));
        assert!(said.is_err());
        assert_eq!(noticed.len(), 1);
        assert_eq!(noticed[0].check, "codex.cli.archive");
        assert_eq!(
            noticed[0].what,
            "`codex archive` is not taken as Relay runs it (exit 2)"
        );
        assert_eq!(
            noticed[0].evidence.as_deref(),
            Some("error: unrecognized subcommand 'archive'")
        );
        let (said, noticed) = crate::drift::capture(|| run(&argv("queue"), None, false, LIST_WAIT));
        assert_eq!(said.unwrap_err().to_string(), "`codex queue` failed: no thread named x");
        assert!(noticed.is_empty(), "an ordinary failure");
        assert!(usage_error(Some(1), "error: unknown command 'rm'"), "commander's");
        assert!(!usage_error(Some(2), "no such file"));
    }

    #[test]
    fn a_run_whose_child_leaves_its_output_open_still_returns() {
        let sh = |s: &str| vec!["sh".to_string(), "-c".into(), s.into()];
        // It has said its word and gone; what it left behind holds the pipe.
        let at = Instant::now();
        let said = run(&sh("sleep 3 & echo hi"), None, false, Duration::from_secs(2)).unwrap();
        assert_eq!(said, "hi\n");
        assert!(at.elapsed() < Duration::from_secs(2), "{:?}", at.elapsed());
        // It hangs, and what it started with it: both go at the wait.
        let tmp = tempfile::tempdir().unwrap();
        let pid = tmp.path().join("pid");
        let at = Instant::now();
        let script = format!("sleep 20 & echo $! > '{}'; wait", pid.display());
        let e = run(&sh(&script), None, false, Duration::from_millis(500)).unwrap_err();
        assert!(e.to_string().contains("did not answer"), "{e}");
        assert!(at.elapsed() < Duration::from_secs(3), "{:?}", at.elapsed());
        let pid: i32 = std::fs::read_to_string(&pid).unwrap().trim().parse().unwrap();
        let gone = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err()
        });
        assert!(gone, "its whole group is killed");
    }

    #[test]
    fn claudes_and_codexs_profiles_have_a_background_and_the_rest_are_held() {
        let all = harnesses(&Registry::builtin(), |_| None);
        let names: Vec<&str> = all.iter().filter(|h| h.lists()).map(|h| h.name()).collect();
        assert_eq!(names, ["claude", "codex"]);
        let gemini = all.iter().find(|h| h.name() == "gemini").unwrap();
        assert!(
            !gemini.found() && gemini.bin() == "gemini",
            "its first binary, not here"
        );
        assert_eq!(
            gemini
                .start(Path::new("/w"), None, Some("fix it"), &HashSet::new())
                .unwrap(),
            Started::Held(vec!["fix it".into()]),
            "Relay holds it"
        );
        assert_eq!(
            gemini.carry_on("g-1"),
            Some(CarryOn::Held(vec!["gemini".into(), "--resume".into(), "g-1".into()]))
        );
        assert!(gemini.has_mcp() && gemini.user_hooks().is_some());
        let sh = all.iter().find(|h| h.name() == "sh").unwrap();
        assert!(!sh.has_mcp() && sh.carry_on("x").is_none() && sh.ping_argv("x", "y").is_none());
    }

    #[test]
    fn claude_and_codex_offer_a_stop_and_a_close_and_the_held_neither() {
        let all = harnesses(&Registry::builtin(), |_| None);
        let get = |n: &str| all.iter().find(|h| h.name() == n).unwrap();
        let row = NativeRow {
            harness: "claude".into(),
            id: "ab12cd34".into(),
            session_id: "s".into(),
            name: "x".into(),
            cwd: "/w".into(),
            state: NativeState {
                words: "working".into(),
                state: RunState::Working,
            },
            summary: None,
            started: None,
            ended: None,
            at: None,
            pr: None,
            color: None,
        };
        let offer = |n: &str| {
            let h = get(n);
            (h.stop_choice().offer(), h.close_choice(&row).map(|c| c.offer()))
        };
        assert_eq!(
            offer("claude"),
            (
                "s stop · keeps the conversation".into(),
                Some("r remove · deletes it and its worktree".into())
            )
        );
        assert_eq!(
            offer("codex"),
            (
                "s stop the turn".into(),
                Some("a archive · off the list, codex unarchive brings it back".into())
            )
        );
        assert_eq!(offer("antigravity"), ("s stop".into(), None), "held: nothing to close");
        let e = get("gemini").close("x", Path::new("/")).unwrap_err();
        assert_eq!(e.to_string(), "nothing closes a gemini session from here");
    }

    #[test]
    fn a_close_runs_its_profiles_args_with_its_binary_in_the_folder() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("claude");
        std::fs::write(&bin, "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" > said\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let r = Registry::builtin();
        for (name, word) in [("claude", "rm"), ("codex", "archive")] {
            let h = of(r.profile(name).unwrap(), Some(bin.clone()));
            h.close("ab12cd34", tmp.path()).unwrap();
            let said = std::fs::read_to_string(tmp.path().join("said")).unwrap();
            let here = std::fs::canonicalize(tmp.path()).unwrap();
            assert_eq!(said, format!("{}\n{word}\nab12cd34\n", here.display()), "{name}");
        }
    }

    /// A Claude whose binary is a stand-in in `dir`: its `attach` draws an
    /// idle Claude (`fixtures/stand-in-claude-attach`, as `variant`) and
    /// keeps what is typed in `dir/typed`; anything else it is run with is
    /// kept in `dir/argv`, a word a line.
    fn stand_in_claude(dir: &Path, variant: &str) -> Box<dyn Harness> {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("claude");
        let attach = concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-claude-attach");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\n[ \"$1\" = attach ] && exec python3 '{attach}' {variant} '{}'\nprintf '%s\\n' \"$@\" > '{}/argv'\n",
                dir.display(),
                dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        of(Registry::builtin().profile("claude").unwrap(), Some(bin))
    }

    fn row_at(cwd: &Path, words: &str, state: RunState) -> NativeRow {
        NativeRow {
            harness: "claude".into(),
            id: "ab12cd34".into(),
            session_id: "s-1".into(),
            name: "fix".into(),
            cwd: cwd.into(),
            state: NativeState {
                words: words.into(),
                state,
            },
            summary: None,
            started: None,
            ended: None,
            at: None,
            pr: None,
            color: None,
        }
    }

    #[test]
    fn claude_is_woken_by_a_line_typed_when_idle_and_carried_on_when_stopped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let locks = dir.join("typing");
        let c = stand_in_claude(&dir, "idle");
        let line = "relay mail from api: call the relay inbox tool to read it";
        let idle = row_at(&dir, "idle", RunState::Idle);
        assert_eq!(c.wake(&idle, line, &locks), Some(Woke::Typed));
        assert_eq!(std::fs::read_to_string(dir.join("typed")).unwrap(), format!("{line}\n"));
        let stopped = row_at(&dir, "completed", RunState::Finished);
        assert_eq!(c.wake(&stopped, line, &locks), Some(Woke::Resumed));
        assert_eq!(
            std::fs::read_to_string(dir.join("argv")).unwrap(),
            format!("--resume\ns-1\n--bg\n{line}\n"),
            "claude --resume <session> --bg <line>"
        );
        for (words, state, said) in [
            (
                "working",
                RunState::Working,
                "claude is busy; it will see it at the end of its turn",
            ),
            (
                "needs input",
                RunState::NeedsInput,
                "claude waits on you; it will see it once you answer",
            ),
        ] {
            assert_eq!(
                c.wake(&row_at(&dir, words, state), line, &locks),
                Some(Woke::Left(said.into()))
            );
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("typed")).unwrap().lines().count(),
            1,
            "nothing more typed"
        );
        // You are attached: not woken, and why.
        let lock = crate::typing::lock_path(&locks, "claude", "ab12cd34");
        let _you = crate::typing::Attached::hold(&lock).unwrap();
        assert_eq!(
            c.wake(&idle, line, &locks),
            Some(Woke::Failed("claude wasn't woken: you are attached to it".into()))
        );
    }

    #[test]
    fn claude_is_renamed_by_typing_its_slash_command_only_when_idle() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let locks = dir.join("typing");
        let c = stand_in_claude(&dir, "idle");
        let idle = row_at(&dir, "idle", RunState::Idle);
        let busy = row_at(&dir, "working", RunState::Working);
        assert!(c.renames(&idle, "the fix"));
        assert!(!c.renames(&busy, "the fix"), "busy: the name stays Relay's");
        assert!(!c.renames(&idle, " "), "a name is not cleared in Claude");
        c.rename(&idle, " the fix ", &locks).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("typed")).unwrap(), "/rename the fix\n");
        let e = c.rename(&busy, "x", &locks).unwrap_err();
        assert_eq!(e.to_string(), "it is working, not idle");
        // The held have no rename and no wake.
        let all = harnesses(&Registry::builtin(), |_| None);
        let gemini = all.iter().find(|h| h.name() == "gemini").unwrap();
        assert!(!gemini.renames(&idle, "x"));
        assert_eq!(gemini.wake(&idle, "x", &locks), None);
        let codex = all.iter().find(|h| h.name() == "codex").unwrap();
        assert!(
            codex.renames(&busy, "x") && codex.renames(&idle, ""),
            "a call: any time, empty too"
        );
        assert_eq!(codex.wake(&idle, "x", &locks), None, "codex is pinged");
    }
}
