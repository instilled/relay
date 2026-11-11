//! A harness — a CLI Relay runs or lists: everything Relay does with one, in
//! one place. One implementation per kind:
//!
//! - [`crate::claude_native::Claude`]: Claude Code, which has a background
//!   of its own (`claude --bg`, `claude attach`, `claude stop`, `claude
//!   agents --json`); its MCP server goes through `claude mcp`; a caller of
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
use crate::sessions::{ID, ScreenMark, TEXT, UserHooks};
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

/// Everything Relay does with one harness. `list`, `start`, `carry_on_there`
/// and `stop` run the harness and may block; they are asked from a
/// background task. Nothing else runs anything.
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
pub fn run(argv: &[String], cwd: Option<&Path>, asking: bool, wait: Duration) -> Result<String> {
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
}
