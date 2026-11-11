//! A harness's own background sessions, through its adapter: what lists
//! them, attaches to one, starts one in the harness's background and
//! carries one on there. One adapter per harness that has a background of
//! its own (Claude's `claude --bg`: `claude_native`; Codex's app-server
//! daemon: `codex_native`); a harness without one has its sessions held by
//! Relay.
//!
//! A native row says only what the harness says of it, in its words: Relay
//! re-labels nothing, and a field whose source is missing stays empty.

use crate::Registry;
use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use rl_core::state::RunState;
use std::collections::HashSet;
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

/// One of a harness's own background sessions, as its adapter lists it.
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
}

/// What Relay does with a harness's own background sessions. `list`,
/// `start`, `carry_on` and `stop` run the harness and may block; they are
/// asked from a background task.
pub trait Adapter: Send + Sync {
    /// The profile it is the adapter of: `claude`.
    fn harness(&self) -> &str;
    /// Its background sessions, as it lists them.
    fn list(&self) -> Result<Vec<NativeRow>> {
        self.list_within(LIST_WAIT)
    }
    /// The same, for no longer than `wait`.
    fn list_within(&self, wait: Duration) -> Result<Vec<NativeRow>>;
    /// What attaches this terminal to the session `id`.
    fn attach_argv(&self, id: &str) -> Vec<String>;
    /// Starts a session in its background in `cwd`, named and prompted if
    /// given: what it began. `before`: the ids its last listing had, which
    /// the new one is not. None: it has no background, and Relay holds
    /// what it starts.
    fn start(
        &self,
        cwd: &Path,
        name: Option<&str>,
        prompt: Option<&str>,
        before: &HashSet<String>,
    ) -> Option<Result<Started>>;
    /// Whether Relay never holds a session of it: what starts one starts it
    /// in its background or in this terminal (Codex: its daemon runs every
    /// thread).
    fn never_held(&self) -> bool {
        false
    }
    /// What carries the conversation `session_id` on in its background.
    /// None: it has no background to carry it on in.
    fn carry_on_argv(&self, session_id: &str) -> Option<Vec<String>>;
    /// Runs `argv`, what `carry_on_argv` gave, in `cwd`: the id of the
    /// session it began there.
    fn carry_on(&self, argv: &[String], cwd: &Path, before: &HashSet<String>) -> Result<String> {
        let _ = (argv, cwd, before);
        bail!("{} carries nothing on in its background", self.harness())
    }
    /// Whether `stop` would stop `row`, as it stands.
    fn stops(&self, row: &NativeRow) -> bool;
    /// Stops its session `id`, in `cwd` (Claude: `claude stop`; Codex:
    /// interrupts its running turn).
    fn stop(&self, id: &str, cwd: &Path) -> Result<()> {
        let _ = (id, cwd);
        bail!("nothing stops a {} session from here", self.harness())
    }
    /// What tells its session `id` `text` from outside it (codex: queues it
    /// on the thread). None: nothing outside it can.
    fn ping_argv(&self, id: &str, text: &str) -> Option<Vec<String>>;
}

/// How long a list may take.
pub const LIST_WAIT: Duration = Duration::from_secs(10);
/// How long a start or a carry-on may take to say its id.
pub const START_WAIT: Duration = Duration::from_secs(30);

/// Whether two paths name one folder, through links.
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// The adapters of every profile in `registry` whose `sessions.list` names
/// a reader there is an adapter for. Its binary is the one `find` finds of
/// the profile's `bins` (`registry::find_bin`: on this machine's `PATH`),
/// else its first name, so what it would run reads the same everywhere;
/// `found` says whether it is here to run.
pub fn adapters(registry: &Registry, find: impl Fn(&[String]) -> Option<PathBuf>) -> Vec<(Box<dyn Adapter>, bool)> {
    registry
        .listing()
        .filter_map(|p| {
            let reader = p.sessions.as_ref()?.list.as_ref()?.reader.as_str();
            let found = find(&p.bins);
            let bin = found.clone().or_else(|| p.bins.first().map(PathBuf::from))?;
            let adapter: Box<dyn Adapter> = match reader {
                crate::claude_native::READER => Box::new(crate::claude_native::Claude::of(p, bin)?),
                crate::codex_native::READER => Box::new(crate::codex_native::Codex::of(p, bin)?),
                _ => return None,
            };
            Some((adapter, found.is_some()))
        })
        .collect()
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
    fn claudes_and_codexs_profiles_have_an_adapter() {
        let all = adapters(&Registry::builtin(), crate::registry::find_bin);
        let names: Vec<&str> = all.iter().map(|(a, _)| a.harness()).collect();
        assert_eq!(names, ["claude", "codex"]);
    }
}
