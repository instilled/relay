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
use crate::sessions::{ID, TEXT};
use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use rl_core::Home;
use rl_core::state::{RunKind, RunState};

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

/// One of a harness's own sessions, as it lists it: in its background, or
/// open in a terminal of its own (`elsewhere`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRow {
    pub harness: String,
    /// The harness's id for it, what attaching takes; for one `elsewhere`,
    /// its conversation's.
    pub id: String,
    /// Open in a terminal of its own, not in its harness's background: the
    /// list's row of it is `Owner::Elsewhere`'s, one with what its hooks
    /// tell.
    pub elsewhere: bool,
    /// Its conversation's id, what hooks tell of.
    pub session_id: String,
    /// What its harness calls it; empty for one `elsewhere` it gives no
    /// name.
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
    Background(String),
    /// Nothing there yet: this, run in this terminal in the folder asked,
    /// begins one itself (Codex with no prompt: its TUI makes the thread).
    Here(Vec<String>),
    /// Nothing: Relay is to hold it, its binary run with these arguments.
    Held(Vec<String>),
}

/// What a session of `profile` that Relay holds runs, from `argv` (its
/// binary and arguments, `Started::Held`'s after the binary): with the hooks
/// its profile's `hooks` says it reports through (`claude --settings
/// <file>`), the file written under `home` for `rl` (asked only then). The
/// held side of every harness's start: profile data, the same for all.
pub fn held_argv(
    profile: &Profile,
    home: &Home,
    mut argv: Vec<String>,
    rl: &dyn Fn() -> Result<PathBuf>,
) -> Result<Vec<String>> {
    if let Some(hooks) = &profile.hooks
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

/// The name its own CLI goes by: its profile's first binary.
pub(crate) fn cli_name(p: &Profile) -> String {
    p.bins.first().cloned().unwrap_or_else(|| p.name.clone())
}

/// Its MCP place, `how`, in the file its profile's `mcp` names as `env`
/// finds it; None where its profile names none.
pub(crate) fn mcp_place(p: &Profile, env: &dyn Fn(&str) -> Option<OsString>, how: How) -> Result<Option<Place>> {
    let Some(m) = p.mcp.as_ref() else {
        return Ok(None);
    };
    let name = &p.name;
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

/// What tells its session `id` `text` from outside it: its profile's
/// `sessions.ping`, run with `bin` (codex: queues it on the thread). None:
/// nothing outside it can.
pub(crate) fn ping_argv(p: &Profile, bin: &str, id: &str, text: &str) -> Option<Vec<String>> {
    let ping = &p.sessions.as_ref()?.ping;
    if ping.is_empty() {
        return None;
    }
    let args = ping.iter().map(|a| a.replace(ID, id).replace(TEXT, text));
    Some(std::iter::once(bin.to_string()).chain(args).collect())
}

/// Deletes its session `id` for good, in `cwd`: its profile's
/// `sessions.remove` arguments run with `bin` (`claude rm <id>`), for no
/// longer than a list may take. Where it refuses, the error is what it
/// said, as it said it (Claude's unpushed commits).
pub(crate) fn remove_id(p: &Profile, bin: &str, id: &str, cwd: &Path) -> Result<()> {
    let Some(s) = p.sessions.as_ref().filter(|s| !s.remove.is_empty()) else {
        bail!("nothing removes a {} session from here", p.name);
    };
    let argv: Vec<String> = std::iter::once(bin.to_string())
        .chain(s.remove.iter().map(|a| a.replace(ID, id)))
        .collect();
    run(&argv, Some(cwd), false, LIST_WAIT).map(drop).map_err(said)
}

// ---- The intents ---------------------------------------------------------

/// What a harness is, for the list and `rl doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// Its profile's name: `claude`.
    pub name: String,
    /// What its sessions are: agents, shells, commands.
    pub kind: RunKind,
    /// The glyph its rows wear, and its colour.
    pub icon: Option<String>,
    pub color: Option<(u8, u8, u8)>,
    /// The binary it runs: the one found on this machine, else its
    /// profile's first.
    pub bin: String,
    /// Whether that binary is on this machine.
    pub found: bool,
    /// Whether its sessions hear of relay mail from Relay's hooks as they
    /// run (`rl hook` prints the news into them): Claude's.
    pub mail_by_hooks: bool,
}

impl Info {
    fn of(profile: &Profile, bin: &str, found: bool, mail_by_hooks: bool) -> Info {
        Info {
            name: profile.name.clone(),
            kind: profile.kind,
            icon: profile.icon.clone(),
            color: profile.color,
            bin: bin.to_string(),
            found,
            mail_by_hooks,
        }
    }

    /// Its version, as `--version` says; None where it is not here or does
    /// not say.
    pub fn version(&self) -> Option<String> {
        self.found
            .then(|| crate::drift::version(Path::new(&self.bin)))
            .flatten()
    }
}

/// Whose a session is, and so what does what with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// Relay holds it (its `id` is its name), its folder `dir`; `live`
    /// while it is still going.
    Relay { live: bool, dir: PathBuf },
    /// Its harness's own, in its background or on its daemon (its `id` is
    /// its harness's).
    Own,
    /// Open in a terminal of its own, told of by its hooks or listed by its
    /// harness as such (`NativeRow::elsewhere`; its `id` is its
    /// conversation's). `listed`: its harness has listed its own
    /// sessions since, so one carried on is not running there already.
    Elsewhere { listed: bool },
}

/// One session on the list, whoever runs it: what an intent is asked of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Its harness's name.
    pub harness: String,
    pub owner: Owner,
    /// A held one's name; its harness's id for one of its own; the
    /// conversation's for one elsewhere.
    pub id: String,
    /// Its conversation's id, what hooks tell of; empty where none is known.
    pub session_id: String,
    pub cwd: PathBuf,
    pub state: RunState,
    /// How its harness says it stands.
    pub words: String,
    /// What it is called, by its harness or by Relay.
    pub name: String,
    /// The name you gave it in Relay, where it is not its harness's.
    pub relay_name: Option<String>,
    /// Whether you are attached to it from Relay's list right now.
    pub attached: bool,
}

impl Session {
    /// One of a harness's own, as it listed it.
    /// One of `harness`'s own that a start just began, by its id, in `cwd`:
    /// not listed yet.
    pub fn started(harness: &str, id: &str, cwd: &Path) -> Session {
        Session {
            harness: harness.into(),
            owner: Owner::Own,
            id: id.into(),
            session_id: String::new(),
            cwd: cwd.into(),
            state: RunState::Starting,
            words: String::new(),
            name: String::new(),
            relay_name: None,
            attached: false,
        }
    }

    pub fn own(n: &NativeRow) -> Session {
        Session {
            harness: n.harness.clone(),
            owner: Owner::Own,
            id: n.id.clone(),
            session_id: n.session_id.clone(),
            cwd: n.cwd.clone(),
            state: n.state.state,
            words: n.state.words.clone(),
            name: n.name.clone(),
            relay_name: None,
            attached: false,
        }
    }
}

/// What a harness's own listing came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listed {
    /// Its sessions, in its words (none for a harness Relay holds).
    Rows(Vec<NativeRow>),
    /// It could not say, and why.
    Unavailable(String),
}

impl Listed {
    pub fn rows(self) -> Result<Vec<NativeRow>> {
        match self {
            Listed::Rows(r) => Ok(r),
            Listed::Unavailable(why) => Err(anyhow!(why)),
        }
    }
}

impl From<Result<Vec<NativeRow>>> for Listed {
    fn from(r: Result<Vec<NativeRow>>) -> Listed {
        match r {
            Ok(rows) => Listed::Rows(rows),
            Err(e) => Listed::Unavailable(format!("{e:#}")),
        }
    }
}

/// What to start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Start {
    pub cwd: PathBuf,
    /// Its name, where you gave one.
    pub name: Option<String>,
    /// Its first prompt.
    pub prompt: Option<String>,
    /// The conversation it carries on (its id), where it carries one on.
    pub resume: Option<String>,
    /// Exactly these words after its binary (`rl new -- claude --model x`),
    /// in place of a prompt.
    pub words: Option<Vec<String>>,
}

/// What enter on a session does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attach {
    /// Relay's own attach to the session it holds.
    Held,
    /// This takes the terminal: the harness's own attach.
    Argv(Vec<String>),
    /// Carried on first, as this start says, then attached to what it
    /// began.
    CarryOn(Start),
    /// Nothing attaches it from here.
    No,
}

/// How a rename lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameHow {
    /// In the harness itself, at once.
    Now,
    /// Relay keeps it, and gives it to the harness the next time Relay
    /// starts or carries the session on.
    AtNextStart,
    /// In Relay only.
    RelayOnly,
}

/// What the list may offer for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actions {
    /// Stopping it: it is still going, and its harness can stop it.
    pub stop: bool,
    /// Removing it for good.
    pub remove: bool,
    /// How a name you give it lands.
    pub rename: RenameHow,
}

impl Actions {
    pub const NONE: Actions = Actions {
        stop: false,
        remove: false,
        rename: RenameHow::RelayOnly,
    };
}

/// What came of a rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Renamed {
    /// The harness has it now.
    Now,
    /// Given to the harness the next time Relay starts or carries it on.
    AtNextStart,
    /// Relay's alone: why, where the harness refused it.
    RelayOnly(Option<String>),
}

/// Relay mail left for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    /// Who sent it, by address.
    pub from: String,
    /// The harness of the session that sent it.
    pub from_harness: String,
}

impl Mail {
    /// The line a session is told: `relay mail from api: call the relay
    /// inbox tool to read it`.
    pub fn line(&self) -> String {
        format!("relay mail from {}: call the relay inbox tool to read it", self.from)
    }
}

/// A session carried on as a new one on the same conversation: its id and
/// its conversation id, both new.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forked {
    pub id: String,
    pub session_id: String,
}

/// What came of telling a session it has mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notified {
    /// Woken now, the mail's line its prompt: it was told. Where waking it
    /// began a new session on its conversation (Claude's `--resume … --bg`),
    /// that one.
    Woken(Option<Forked>),
    /// Queued on it, for it to see now or when it next runs.
    Queued,
    /// Its hooks tell it, at its next prompt or turn's end; why it is left
    /// to them, where there is anything to say.
    ByHooks(Option<String>),
    /// It sees it when it next calls its inbox.
    AtNextInbox,
    /// It could have been told, and was not: why.
    Failed(String),
}

impl Notified {
    /// What `send` adds about it: ` · woke claude`; nothing where there is
    /// nothing to say.
    pub fn words(&self, harness: &str) -> String {
        match self {
            Notified::Woken(_) => format!(" · woke {harness}"),
            Notified::Queued => format!(" · pinged {harness}"),
            Notified::ByHooks(Some(why)) | Notified::Failed(why) => format!(" · {why}"),
            Notified::ByHooks(None) | Notified::AtNextInbox => String::new(),
        }
    }
}

/// How a harness stands against what Relay uses of it (`Harness::check`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compat {
    pub version: Option<String>,
    /// What is off, each by its drift check's id and in words.
    pub off: Vec<(String, String)>,
}

/// What Relay's own sessions are stopped and removed by: the store of held
/// sessions `rl` installs (`set_holder`). With none, a held session is
/// neither.
pub trait Holder: Send + Sync {
    /// Closes the held session `name`, kept in `dir`, and waits for it to
    /// end.
    fn stop(&self, dir: &Path, name: &str) -> Result<()>;
    /// Removes the held session `name`, kept in `dir`, ended, and all Relay
    /// kept of it.
    fn remove(&self, dir: &Path, name: &str) -> Result<()>;
}

static HOLDER: std::sync::RwLock<Option<std::sync::Arc<dyn Holder>>> = std::sync::RwLock::new(None);

/// Installs what stops and removes Relay's own sessions.
pub fn set_holder(h: std::sync::Arc<dyn Holder>) {
    *HOLDER.write().unwrap_or_else(|e| e.into_inner()) = Some(h);
}

fn holder() -> Result<std::sync::Arc<dyn Holder>> {
    HOLDER
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| anyhow!("no store of Relay's sessions here"))
}

/// Everything Relay does with one harness, as intents: each harness binds
/// its own steps behind them, and a caller shows what came of one.
/// `sessions`, `start`, `stop`, `remove`, `rename`, `notify` and `check`
/// may run the harness and block; they are asked from a background task.
pub trait Harness: Send + Sync {
    fn info(&self) -> &Info;
    /// Its own sessions, for no longer than `wait`: none for one Relay
    /// holds.
    fn sessions(&self, wait: Duration) -> Listed;
    /// Starts one as `s` says: in its background, here, or for Relay to
    /// hold.
    fn start(&self, s: &Start) -> Result<Started>;
    /// What enter on `s` does.
    fn attach(&self, s: &Session) -> Attach;
    /// What the list may offer for `s`.
    fn actions(&self, s: &Session) -> Actions;
    fn stop(&self, s: &Session) -> Result<()>;
    /// Deletes `s` for good: one still going is stopped first.
    fn remove(&self, s: &Session) -> Result<()>;
    /// Gives `s` the name `name` (empty: its own again), as far as it can.
    fn rename(&self, s: &Session, name: &str) -> Renamed;
    /// Tells `s` it has `mail`.
    fn notify(&self, s: &Session, mail: &Mail) -> Notified;
    /// What `rl setup` does for it, part by part, each in its file as `env`
    /// finds it. An error where one's file is not known (`HOME` not set).
    fn setup(&self, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Vec<crate::setup::Part>>;
    /// The conversation id a session of it that runs `rl mcp` has, as its
    /// environment (`env`) says: how a caller Relay does not hold is known.
    fn caller(&self, env: &dyn Fn(&str) -> Option<String>) -> Option<String>;
    /// How it stands against what Relay uses of it; what is off is drift,
    /// noticed as it is found.
    fn check(&self) -> Compat;
}

/// What Relay holds of a harness without a background of its own, or for
/// one that has: its sessions Relay holds.
pub(crate) mod held {
    use super::*;

    pub fn info(profile: &Profile, bin: &str, found: bool) -> Info {
        Info::of(profile, bin, found, false)
    }

    /// One Relay holds: `Started::Held` with its profile's arguments.
    pub fn start(p: &Profile, s: &Start) -> Result<Started> {
        if let Some(words) = &s.words {
            return Ok(Started::Held(words.clone()));
        }
        if let Some(id) = &s.resume {
            let args = p
                .resume_argv(String::new(), id)
                .ok_or_else(|| anyhow!("{} has no way to carry a conversation on", p.name))?;
            return Ok(Started::Held(args[1..].to_vec()));
        }
        Ok(Started::Held(p.prompt_args(s.prompt.as_deref().unwrap_or(""))))
    }

    /// A held one: Relay's attach. One elsewhere, at rest: carried on in a
    /// session Relay holds, where its profile says how.
    pub fn attach(p: &Profile, s: &Session) -> Attach {
        match &s.owner {
            Owner::Relay { .. } => Attach::Held,
            Owner::Elsewhere { .. } if s.state == RunState::Idle && !p.resume.is_empty() => Attach::CarryOn(Start {
                cwd: s.cwd.clone(),
                resume: Some(s.session_id.clone()),
                ..Start::default()
            }),
            _ => Attach::No,
        }
    }

    pub fn actions(s: &Session) -> Actions {
        match s.owner {
            Owner::Relay { live, .. } => Actions {
                stop: live,
                remove: true,
                rename: RenameHow::RelayOnly,
            },
            _ => Actions::NONE,
        }
    }

    pub fn stop(s: &Session) -> Result<()> {
        match s.owner {
            Owner::Relay { ref dir, .. } => holder()?.stop(dir, &s.id),
            _ => bail!("nothing stops a {} session from here", s.harness),
        }
    }

    pub fn remove(s: &Session) -> Result<()> {
        match s.owner {
            Owner::Relay { live, ref dir } => {
                let h = holder()?;
                if live {
                    h.stop(dir, &s.id)?;
                }
                h.remove(dir, &s.id)
            }
            _ => bail!("nothing removes a {} session from here", s.harness),
        }
    }

    /// Mail is queued on it where its profile has a way in from outside
    /// (`sessions.ping`); else it sees it at its next inbox.
    pub fn notify(p: &Profile, bin: &str, s: &Session, mail: &Mail) -> Notified {
        // A held one by its conversation's id, as its hooks said it.
        let id = match s.owner {
            Owner::Relay { .. } => &s.session_id,
            _ => &s.id,
        };
        let Some(argv) = ping_argv(p, bin, id, &mail.line()).filter(|_| !id.is_empty()) else {
            return Notified::AtNextInbox;
        };
        // Asking: the harness's own hooks let a run of Relay's go.
        match run(&argv, None, true, PING_WAIT) {
            Ok(_) => Notified::Queued,
            Err(e) => Notified::Failed(format!("{} didn't take the ping: {e:#}", s.harness)),
        }
    }

    /// Its hooks, its MCP server, its settings, as its profile says; its
    /// MCP server through `how`.
    pub fn setup(p: &Profile, env: &dyn Fn(&str) -> Option<OsString>, how: How) -> Result<Vec<crate::setup::Part>> {
        let mcp = mcp_place(p, env, how)?;
        let hooks = p.sessions.as_ref().and_then(|s| s.hooks.as_ref());
        crate::setup::parts(&p.name, hooks, mcp, p.settings.as_ref(), env)
    }

    /// Written into its profile's `mcp` file, or through its CLI's `mcp.cli`
    /// words.
    pub fn mcp_how(p: &Profile, bin: &str, found: bool) -> How {
        match p.mcp.as_ref() {
            Some(m) if !m.cli.is_empty() => How::Cli {
                cli: cli_name(p),
                bin: found.then(|| PathBuf::from(bin)),
                words: m.cli.clone(),
            },
            _ => How::Json,
        }
    }
}

/// How long telling a session from outside may take: `codex queue` queues
/// and resumes the thread on Codex's daemon.
pub const PING_WAIT: Duration = Duration::from_secs(10);

impl Harness for Held {
    fn info(&self) -> &Info {
        &self.info
    }

    fn sessions(&self, _: Duration) -> Listed {
        Listed::Rows(Vec::new())
    }

    fn start(&self, s: &Start) -> Result<Started> {
        held::start(&self.profile, s)
    }

    fn attach(&self, s: &Session) -> Attach {
        held::attach(&self.profile, s)
    }

    fn actions(&self, s: &Session) -> Actions {
        held::actions(s)
    }

    fn stop(&self, s: &Session) -> Result<()> {
        held::stop(s)
    }

    fn remove(&self, s: &Session) -> Result<()> {
        held::remove(s)
    }

    fn rename(&self, _: &Session, _: &str) -> Renamed {
        Renamed::RelayOnly(None)
    }

    fn notify(&self, s: &Session, mail: &Mail) -> Notified {
        held::notify(&self.profile, &self.info.bin, s, mail)
    }

    fn setup(&self, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Vec<crate::setup::Part>> {
        let how = held::mcp_how(&self.profile, &self.info.bin, self.info.found);
        held::setup(&self.profile, env, how)
    }

    fn caller(&self, _: &dyn Fn(&str) -> Option<String>) -> Option<String> {
        None
    }

    fn check(&self) -> Compat {
        Compat {
            version: self.info.version(),
            off: Vec::new(),
        }
    }
}

/// A harness without a background of its own: Relay holds what it starts,
/// and its profile says the rest.
#[derive(Debug, Clone)]
pub struct Held {
    profile: Profile,
    info: Info,
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
            info: held::info(profile, &bin.to_string_lossy(), here),
            profile: profile.clone(),
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
        let said = if err.trim().is_empty() { &out } else { &err };
        return Err(Failed {
            what,
            first: err.lines().next().unwrap_or("no reason given").to_string(),
            said: said.trim().to_string(),
        }
        .into());
    }
    Ok(out)
}

/// A run that exited unsuccessfully: in a line, `` `<what>` failed: <the
/// first line of its stderr> ``; `said` keeps all it printed, its stderr
/// (its stdout where that is empty), trimmed.
#[derive(Debug)]
pub struct Failed {
    pub what: String,
    first: String,
    pub said: String,
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` failed: {}", self.what, self.first)
    }
}

impl std::error::Error for Failed {}

/// A run's error as the harness said it, every line of it, where it
/// failed and said something (`Failed::said`); any other error as it is.
pub fn said(e: anyhow::Error) -> anyhow::Error {
    match e.downcast::<Failed>() {
        Ok(f) if !f.said.is_empty() => anyhow!(f.said),
        Ok(f) => f.into(),
        Err(e) => e,
    }
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
        let bin = tmp.path().join("claude");
        std::fs::write(
            &bin,
            "#!/bin/sh\n[ \"$1\" = rm ] && { echo \"error: unrecognized subcommand 'rm'\" >&2; echo 'Usage: claude [OPTIONS]' >&2; exit 2; }\necho 'no session named x' >&2; exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let argv = |w: &str| vec![bin.display().to_string(), w.into(), "x".into()];
        let (said, noticed) = crate::drift::capture(|| run(&argv("rm"), None, false, LIST_WAIT));
        assert!(said.is_err());
        assert_eq!(noticed.len(), 1);
        assert_eq!(noticed[0].check, "claude.cli.rm");
        assert_eq!(noticed[0].what, "`claude rm` is not taken as Relay runs it (exit 2)");
        assert_eq!(
            noticed[0].evidence.as_deref(),
            Some("error: unrecognized subcommand 'rm'")
        );
        let (said, noticed) = crate::drift::capture(|| run(&argv("stop"), None, false, LIST_WAIT));
        assert_eq!(
            said.unwrap_err().to_string(),
            "`claude stop` failed: no session named x"
        );
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
    fn the_held_list_nothing_of_their_own_and_relay_holds_what_they_start() {
        let all = harnesses(&Registry::builtin(), |_| None);
        let get = |n: &str| all.iter().find(|h| h.info().name == n).unwrap();
        for h in all
            .iter()
            .filter(|h| !["claude", "codex"].contains(&h.info().name.as_str()))
        {
            assert_eq!(h.sessions(LIST_WAIT), Listed::Rows(Vec::new()), "{}", h.info().name);
        }
        let gemini = get("gemini");
        assert!(
            !gemini.info().found && gemini.info().bin == "gemini",
            "its first binary, not here"
        );
        assert_eq!(
            gemini
                .start(&Start {
                    cwd: "/w".into(),
                    prompt: Some("fix it".into()),
                    ..Start::default()
                })
                .unwrap(),
            Started::Held(vec!["fix it".into()]),
            "Relay holds it"
        );
        let env = |k: &str| (k == "HOME").then(|| OsString::from("/h"));
        assert!(!gemini.setup(&env).unwrap().is_empty(), "its hooks and its server");
        let sh = get("sh");
        let elsewhere = Session {
            harness: "sh".into(),
            owner: Owner::Elsewhere { listed: true },
            id: "x".into(),
            session_id: "x".into(),
            cwd: "/w".into(),
            state: RunState::Idle,
            words: String::new(),
            name: String::new(),
            relay_name: None,
            attached: false,
        };
        assert!(sh.setup(&env).unwrap().is_empty());
        assert_eq!(sh.attach(&elsewhere), Attach::No, "no way to carry one on");
        let mail = Mail {
            from: "a".into(),
            from_harness: "sh".into(),
        };
        assert_eq!(
            sh.notify(&elsewhere, &mail),
            Notified::AtNextInbox,
            "no way in from outside"
        );
    }

    #[test]
    fn claude_and_codex_remove_for_good_and_the_held_have_no_remove_of_their_own() {
        let all = harnesses(&Registry::builtin(), |_| None);
        let get = |n: &str| all.iter().find(|h| h.info().name == n).unwrap();
        let row = NativeRow {
            harness: "claude".into(),
            id: "ab12cd34".into(),
            elsewhere: false,
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
        let own = Session::own(&row);
        assert!(get("claude").actions(&own).remove);
        assert!(get("codex").actions(&own).remove, "thread/delete");
        assert!(!get("antigravity").actions(&own).remove, "held: Relay removes its own");
        let theirs = Session {
            harness: "gemini".into(),
            ..own
        };
        let e = get("gemini").remove(&theirs).unwrap_err();
        assert_eq!(e.to_string(), "nothing removes a gemini session from here");
    }

    #[test]
    fn claudes_remove_is_claude_rm_in_the_folder_and_a_refusal_is_said_as_it_said_it() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("claude");
        std::fs::write(
            &bin,
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" > said\n[ -f refuse ] && { cat refuse >&2; exit 1; }\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let h = of(Registry::builtin().profile("claude").unwrap(), Some(bin));
        let own = Session::started("claude", "ab12cd34", tmp.path());
        h.remove(&own).unwrap();
        let said = std::fs::read_to_string(tmp.path().join("said")).unwrap();
        let here = std::fs::canonicalize(tmp.path()).unwrap();
        assert_eq!(
            said,
            format!("{}\nrm\nab12cd34\n", here.display()),
            "claude rm <id>, no flags"
        );
        let refusal =
            "ab12cd34 has 2 unpushed commits in worktree w-1\nrerun with --discard-unpushed 9f1e@w-1 to discard them";
        std::fs::write(tmp.path().join("refuse"), format!("{refusal}\n")).unwrap();
        let e = h.remove(&own).unwrap_err();
        assert_eq!(e.to_string(), refusal, "every line, as said");
    }

    // ---- The intents ----------------------------------------------------

    fn session(harness: &str, owner: Owner, state: RunState, cwd: &Path) -> Session {
        Session {
            harness: harness.into(),
            owner,
            id: "ab12cd34".into(),
            session_id: "s-1".into(),
            cwd: cwd.into(),
            state,
            words: state.to_string(),
            name: "fix".into(),
            relay_name: None,
            attached: false,
        }
    }

    /// A Holder that keeps what it was asked.
    #[derive(Default)]
    struct Kept(std::sync::Mutex<Vec<String>>);

    impl Holder for Kept {
        fn stop(&self, _: &Path, name: &str) -> Result<()> {
            self.0.lock().unwrap().push(format!("stop {name}"));
            Ok(())
        }
        fn remove(&self, _: &Path, name: &str) -> Result<()> {
            self.0.lock().unwrap().push(format!("remove {name}"));
            Ok(())
        }
    }

    #[test]
    fn a_held_harness_binds_each_intent_through_its_profile_and_relays_holder() {
        let all = harnesses(&Registry::builtin(), |_| None);
        let gemini = all.iter().find(|h| h.info().name == "gemini").unwrap();
        let at = Path::new("/w");
        let start = |s: Start| Harness::start(gemini.as_ref(), &s).unwrap();
        assert_eq!(
            start(Start {
                words: Some(vec!["-m".into(), "x".into()]),
                ..Start::default()
            }),
            Started::Held(vec!["-m".into(), "x".into()]),
            "words as given"
        );
        assert_eq!(
            start(Start {
                resume: Some("g-1".into()),
                ..Start::default()
            }),
            Started::Held(vec!["--resume".into(), "g-1".into()]),
            "carried on as its profile's resume says"
        );
        let held = session(
            "gemini",
            Owner::Relay {
                live: true,
                dir: "/h/sessions/x".into(),
            },
            RunState::Working,
            at,
        );
        assert_eq!(gemini.attach(&held), Attach::Held);
        let elsewhere = session("gemini", Owner::Elsewhere { listed: false }, RunState::Idle, at);
        let Attach::CarryOn(s) = gemini.attach(&elsewhere) else {
            panic!("carried on in a session Relay holds");
        };
        assert_eq!(s.resume.as_deref(), Some("s-1"));
        let busy = Session {
            state: RunState::Working,
            ..elsewhere.clone()
        };
        assert_eq!(gemini.attach(&busy), Attach::No, "in use where it is");
        assert_eq!(
            gemini.actions(&held),
            Actions {
                stop: true,
                remove: true,
                rename: RenameHow::RelayOnly
            }
        );
        assert_eq!(gemini.actions(&elsewhere), Actions::NONE);
        let kept = std::sync::Arc::new(Kept::default());
        set_holder(kept.clone());
        let held = Session {
            id: "api-gemini".into(),
            ..held
        };
        Harness::stop(gemini.as_ref(), &held).unwrap();
        Harness::remove(gemini.as_ref(), &held).unwrap();
        let ended = Session {
            owner: Owner::Relay {
                live: false,
                dir: "/h/sessions/x".into(),
            },
            ..held.clone()
        };
        Harness::remove(gemini.as_ref(), &ended).unwrap();
        assert_eq!(
            *kept.0.lock().unwrap(),
            [
                "stop api-gemini",
                "stop api-gemini",
                "remove api-gemini",
                "remove api-gemini"
            ],
            "a live one stopped before it is removed"
        );
        assert!(Harness::stop(gemini.as_ref(), &elsewhere).is_err());
        assert_eq!(Harness::rename(gemini.as_ref(), &held, "x"), Renamed::RelayOnly(None));
        let mail = Mail {
            from: "api".into(),
            from_harness: "codex".into(),
        };
        assert_eq!(gemini.notify(&held, &mail), Notified::AtNextInbox);
        assert_eq!(gemini.caller(&|_| Some("x".into())), None);
        let env = |k: &str| (k == "HOME").then(|| OsString::from("/h"));
        let parts: Vec<String> = gemini
            .setup(&env)
            .unwrap()
            .iter()
            .map(|p| p.what().to_string())
            .collect();
        assert_eq!(parts, ["hooks", "mcp relay"]);
        assert!(gemini.setup(&|_| None).is_err(), "no HOME: its files are not known");
        let sh = all.iter().find(|h| h.info().name == "sh").unwrap();
        assert!(sh.setup(&env).unwrap().is_empty());
        assert_eq!(sh.info().kind, RunKind::Shell);
    }

    /// A Claude whose binary keeps what it is run with in `dir/calls`, a
    /// word a line and `--` after each run; a `stop` fails where `dir` has
    /// `refuse-stop`.
    fn recording_claude(dir: &Path) -> Box<dyn Harness> {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("claude");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" -- >> '{d}/calls'\n[ \"$1\" = stop ] && [ -f '{d}/refuse-stop' ] && {{ echo 'no such session' >&2; exit 1; }}\n[ \"$1\" = agents ] && echo '[]'\nexit 0\n",
                d = dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        of(Registry::builtin().profile("claude").unwrap(), Some(bin))
    }

    fn calls(dir: &Path) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(dir.join("calls")).unwrap_or_default();
        let _ = std::fs::remove_file(dir.join("calls"));
        text.split("--\n")
            .filter(|c| !c.is_empty())
            .map(|c| c.lines().map(String::from).collect())
            .collect()
    }

    #[test]
    fn claude_is_told_of_mail_by_a_stop_and_a_resume_in_its_background_and_by_its_hooks_while_busy() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let c = recording_claude(&dir);
        let mail = Mail {
            from: "api".into(),
            from_harness: "codex".into(),
        };
        let line = "relay mail from api: call the relay inbox tool to read it";
        let idle = Session {
            relay_name: Some("the fix".into()),
            ..session("claude", Owner::Own, RunState::Idle, &dir)
        };
        assert_eq!(c.notify(&idle, &mail), Notified::Woken(None), "it printed no id");
        assert_eq!(
            calls(&dir),
            [
                vec!["stop", "ab12cd34"],
                vec!["--resume", "s-1", "--bg", "-n", "the fix", line]
            ],
            "claude stop <id>, then claude --resume <session> --bg -n <name> <line>"
        );
        let ended = session("claude", Owner::Own, RunState::Finished, &dir);
        assert_eq!(c.notify(&ended, &mail), Notified::Woken(None));
        assert_eq!(
            calls(&dir),
            [vec!["--resume", "s-1", "--bg", line]],
            "nothing to stop; no name of Relay's, none given"
        );
        for (state, why) in [
            (
                RunState::Working,
                "claude is busy; it will see it at the end of its turn",
            ),
            (
                RunState::NeedsInput,
                "claude waits on you; it will see it once you answer",
            ),
        ] {
            let s = session("claude", Owner::Own, state, &dir);
            assert_eq!(c.notify(&s, &mail), Notified::ByHooks(Some(why.into())));
        }
        let yours = Session {
            attached: true,
            ..idle.clone()
        };
        assert_eq!(
            c.notify(&yours, &mail),
            Notified::ByHooks(Some(
                "you are attached to it; it will see it at your next prompt".into()
            ))
        );
        for owner in [
            Owner::Relay {
                live: true,
                dir: "/h/sessions/x".into(),
            },
            Owner::Elsewhere { listed: true },
        ] {
            assert_eq!(
                c.notify(&session("claude", owner, RunState::Idle, &dir), &mail),
                Notified::ByHooks(None)
            );
        }
        assert!(calls(&dir).is_empty(), "nothing run for those");
        std::fs::write(dir.join("refuse-stop"), "").unwrap();
        let Notified::Failed(why) = c.notify(&idle, &mail) else {
            panic!("a stop that fails is said");
        };
        assert_eq!(why, "claude wasn't woken: `claude stop` failed: no such session");
        assert_eq!(calls(&dir).len(), 1, "not carried on once its stop failed");
        let from_claude = Mail {
            from_harness: "claude".into(),
            ..mail
        };
        let Notified::Failed(why) = c.notify(&idle, &from_claude) else {
            panic!("failed");
        };
        assert!(
            why.ends_with(
                "fix is a Claude session — to wake it now, also SendMessage it (\"relay mail waiting: call inbox\")."
            ),
            "{why}"
        );
    }

    #[test]
    fn claude_keeps_a_name_for_its_next_start_and_carries_one_elsewhere_on_with_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let c = recording_claude(&dir);
        let own = session("claude", Owner::Own, RunState::Idle, &dir);
        assert_eq!(Harness::rename(c.as_ref(), &own, "the fix"), Renamed::AtNextStart);
        assert_eq!(Harness::rename(c.as_ref(), &own, " "), Renamed::RelayOnly(None));
        assert!(calls(&dir).is_empty(), "nothing typed, nothing run");
        assert_eq!(
            c.actions(&own),
            Actions {
                stop: true,
                remove: true,
                rename: RenameHow::AtNextStart
            }
        );
        let done = Session {
            state: RunState::Finished,
            ..own.clone()
        };
        assert!(!c.actions(&done).stop, "nothing left to stop");
        let Attach::Argv(argv) = c.attach(&own) else {
            panic!("its own attach");
        };
        assert_eq!(argv[1..], ["attach", "ab12cd34"]);
        let elsewhere = Session {
            relay_name: Some("the fix".into()),
            ..session("claude", Owner::Elsewhere { listed: true }, RunState::Idle, &dir)
        };
        let Attach::CarryOn(start) = c.attach(&elsewhere) else {
            panic!("carried on in its background");
        };
        assert_eq!(
            (start.resume.as_deref(), start.name.as_deref()),
            (Some("s-1"), Some("the fix"))
        );
        let unlisted = Session {
            owner: Owner::Elsewhere { listed: false },
            ..elsewhere
        };
        assert_eq!(c.attach(&unlisted), Attach::No, "it may run in its background already");
        assert_eq!(
            c.caller(&|k| (k == "CLAUDE_CODE_SESSION_ID").then(|| "c-1".to_string())),
            Some("c-1".into())
        );
        assert!(c.info().mail_by_hooks);
        let words = Start {
            words: Some(vec!["--model".into(), "x".into()]),
            ..Start::default()
        };
        assert_eq!(
            Harness::start(c.as_ref(), &words).unwrap(),
            Started::Held(vec!["--model".into(), "x".into()]),
            "words as given: held"
        );
        Harness::stop(c.as_ref(), &own).unwrap();
        Harness::remove(c.as_ref(), &own).unwrap();
        assert_eq!(calls(&dir), [vec!["stop", "ab12cd34"], vec!["rm", "ab12cd34"]]);
    }

    #[test]
    fn claude_carries_a_conversation_on_named_with_dash_n() {
        let r = Registry::builtin();
        let c = crate::claude_native::Claude::of(r.profile("claude").unwrap(), "claude".into()).unwrap();
        assert_eq!(
            c.bg_argv(Some("s-1"), Some("the fix"), Some("go on")).unwrap(),
            ["claude", "--resume", "s-1", "--bg", "-n", "the fix", "go on"]
        );
        assert_eq!(
            c.bg_argv(None, Some("the fix"), None).unwrap(),
            ["claude", "--bg", "-n", "the fix"]
        );
    }

    /// A Codex on a copy of the stand-in daemon in `dir`.
    fn stand_in_codex(dir: &Path) -> (Box<dyn Harness>, PathBuf) {
        let bin = dir.join("codex");
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-codex-daemon"),
            &bin,
        )
        .unwrap();
        (
            of(Registry::builtin().profile("codex").unwrap(), Some(bin.clone())),
            bin,
        )
    }

    fn sent(bin: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(bin.with_extension("log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    #[test]
    fn codex_binds_its_intents_on_its_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let (c, bin) = stand_in_codex(&dir);
        let start = Start {
            cwd: dir.clone(),
            name: Some("login".into()),
            prompt: Some("fix the login".into()),
            ..Start::default()
        };
        let Started::Background(id) = Harness::start(c.as_ref(), &start).unwrap() else {
            panic!("a thread on its daemon");
        };
        let methods = |bin: &Path| -> Vec<String> {
            sent(bin)
                .iter()
                .filter_map(|m| m["method"].as_str().map(String::from))
                .filter(|m| !m.starts_with("initialize"))
                .collect()
        };
        assert_eq!(methods(&bin), ["thread/start", "thread/name/set", "turn/start"]);
        let mut own = Session {
            id: id.clone(),
            session_id: id.clone(),
            ..session("codex", Owner::Own, RunState::Working, &dir)
        };
        assert_eq!(
            c.actions(&own),
            Actions {
                stop: true,
                remove: true,
                rename: RenameHow::Now
            }
        );
        Harness::stop(c.as_ref(), &own).unwrap();
        assert_eq!(methods(&bin)[3..], ["thread/read", "turn/interrupt"]);
        own.state = RunState::Idle;
        assert!(!c.actions(&own).stop, "nothing running to stop");
        assert_eq!(Harness::rename(c.as_ref(), &own, "auth"), Renamed::Now);
        let renamed = sent(&bin);
        let last = renamed.last().unwrap();
        assert_eq!(
            (last["method"].as_str(), &last["params"]),
            (
                Some("thread/name/set"),
                &serde_json::json!({"threadId": id, "name": "auth"})
            )
        );
        let mail = Mail {
            from: "api".into(),
            from_harness: "claude".into(),
        };
        assert_eq!(c.notify(&own, &mail), Notified::Queued);
        assert_eq!(
            std::fs::read_to_string(bin.with_extension("argv")).unwrap(),
            format!(
                "queue\n--thread\n{id}\n--message\nrelay mail from api: call the relay inbox tool to read it\n--resume\n"
            )
        );
        let Attach::Argv(argv) = c.attach(&own) else {
            panic!("resumed through its daemon");
        };
        assert_eq!(argv[1..], ["resume", id.as_str()]);
        let resume = Start {
            resume: Some(id.clone()),
            ..Start::default()
        };
        assert!(
            matches!(Harness::start(c.as_ref(), &resume).unwrap(), Started::Here(a) if a[1..] == ["resume", id.as_str()])
        );
        assert_eq!(
            Harness::start(c.as_ref(), &Start::default()).unwrap(),
            Started::Here(vec![bin.display().to_string()]),
            "no prompt: its TUI, here"
        );
        let flag = Start {
            words: Some(vec!["-m".into(), "o3".into()]),
            ..Start::default()
        };
        let e = Harness::start(c.as_ref(), &flag).unwrap_err();
        assert!(e.to_string().contains("`-m` can't go"), "{e}");
        Harness::remove(c.as_ref(), &own).unwrap();
        assert_eq!(methods(&bin).last().map(String::as_str), Some("thread/delete"));
        let elsewhere = session("codex", Owner::Elsewhere { listed: true }, RunState::Idle, &dir);
        assert_eq!(Harness::rename(c.as_ref(), &elsewhere, "x"), Renamed::RelayOnly(None));
        assert_eq!(c.actions(&elsewhere), Actions::NONE);
        let env = |k: &str| (k == "HOME").then(|| OsString::from("/h"));
        let parts: Vec<String> = c.setup(&env).unwrap().iter().map(|p| p.what().to_string()).collect();
        assert_eq!(parts, ["hooks", "mcp relay"]);
        assert_eq!(
            c.check().off,
            Vec::<(String, String)>::new(),
            "no schema to tell by: nothing off"
        );
    }
}
