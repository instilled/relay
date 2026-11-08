//! A harness's own background sessions, through its adapter: what lists
//! them, attaches to one, starts one in the harness's background and
//! carries one on there. One adapter per harness that has a background of
//! its own (Claude's: `claude_native`); a harness without one has its
//! sessions held by Relay.
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
}

/// What Relay does with a harness's own background sessions. `list` runs
/// the harness's CLI and may block; it is asked from a background task.
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
    /// What starts a session in its background, named and prompted if
    /// given. None: it has no background, and Relay holds what it starts.
    fn start_argv(&self, name: Option<&str>, prompt: Option<&str>) -> Option<Vec<String>>;
    /// What carries the conversation `session_id` on in its background.
    /// None: it has no background to carry it on in.
    fn carry_on_argv(&self, session_id: &str) -> Option<Vec<String>>;
    /// What stops its session `id`. None: nothing here stops one.
    fn stop_argv(&self, id: &str) -> Option<Vec<String>>;
    /// The ids a start or a carry-on may have said it began, in what it
    /// printed, the likeliest first. Only its listing says which it is
    /// (`start`).
    fn started_ids(&self, printed: &str) -> Vec<String>;
}

/// How long a list may take.
pub const LIST_WAIT: Duration = Duration::from_secs(10);
/// How long a start or a carry-on may take to say its id.
pub const START_WAIT: Duration = Duration::from_secs(30);
/// How long a session a start began has to come onto its harness's list.
pub const LISTED_WAIT: Duration = Duration::from_secs(10);
/// How often the harness is asked meanwhile.
const LISTED_EVERY: Duration = Duration::from_millis(250);

/// Runs `argv`, which starts or carries on a session in `a`'s harness's own
/// background, in `cwd`, and says the id of the session it began — once the
/// harness lists it, never on what it printed alone: a background session
/// not among `before` (the ids its last listing had), in `cwd`, by an id it
/// printed. The listing is asked for up to `listed`. Nothing listed so is
/// an error in the words it printed last.
pub fn start(
    a: &dyn Adapter,
    argv: &[String],
    cwd: &Path,
    before: &HashSet<String>,
    listed: Duration,
) -> Result<String> {
    let printed = run(argv, Some(cwd), false, START_WAIT)?;
    let ids = a.started_ids(&printed);
    let deadline = Instant::now() + listed;
    loop {
        // The whole loop, its listings too, is bounded by `listed`.
        let left = deadline.saturating_duration_since(Instant::now());
        if !ids.is_empty()
            && !left.is_zero()
            && let Ok(rows) = a.list_within(left.min(LIST_WAIT))
            && let Some(id) = ids
                .iter()
                .find(|id| !before.contains(*id) && rows.iter().any(|r| r.id == **id && same_dir(&r.cwd, cwd)))
        {
            return Ok(id.clone());
        }
        if ids.is_empty() || Instant::now() >= deadline {
            let last = printed.lines().rev().find(|l| !l.trim().is_empty());
            bail!("{}", last.map_or("it printed nothing", str::trim));
        }
        std::thread::sleep(LISTED_EVERY);
    }
}

/// Whether two paths name one folder, through links.
fn same_dir(a: &Path, b: &Path) -> bool {
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
        .env_remove("RELAY_SESSION")
        .env_remove("RELAY_SESSION_AGENT");
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

    /// Lists what it is told to, one answer per ask, the last kept; takes
    /// every word of eight letters for an id it may have started.
    struct Scripted(std::sync::Mutex<Vec<Result<Vec<NativeRow>, String>>>);

    impl Adapter for Scripted {
        fn harness(&self) -> &str {
            "fake"
        }
        fn list_within(&self, _: Duration) -> Result<Vec<NativeRow>> {
            let mut said = self.0.lock().unwrap();
            let next = if said.len() > 1 {
                said.remove(0)
            } else {
                said[0].clone()
            };
            next.map_err(|e| anyhow!(e))
        }
        fn attach_argv(&self, id: &str) -> Vec<String> {
            vec!["fake".into(), id.into()]
        }
        fn start_argv(&self, _: Option<&str>, _: Option<&str>) -> Option<Vec<String>> {
            None
        }
        fn carry_on_argv(&self, _: &str) -> Option<Vec<String>> {
            None
        }
        fn stop_argv(&self, _: &str) -> Option<Vec<String>> {
            None
        }
        fn started_ids(&self, printed: &str) -> Vec<String> {
            printed
                .split_whitespace()
                .filter(|w| w.len() == 8)
                .map(String::from)
                .collect()
        }
    }

    fn listed(id: &str, cwd: &Path) -> NativeRow {
        NativeRow {
            harness: "fake".into(),
            id: id.into(),
            session_id: format!("s-{id}"),
            name: id.into(),
            cwd: cwd.into(),
            state: NativeState {
                words: "working".into(),
                state: RunState::Working,
            },
            summary: None,
            started: None,
            ended: None,
            at: None,
            pr: None,
        }
    }

    #[test]
    fn a_started_id_is_only_one_the_harness_then_lists_new_in_its_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let here = tmp.path();
        let elsewhere = Path::new("/elsewhere");
        let echo = |s: &str| vec!["echo".to_string(), s.into()];
        let before: HashSet<String> = ["old00001".to_string()].into();
        // What it printed names one already there, one in another folder,
        // and the new one, which shows on the third ask.
        let a = Scripted(std::sync::Mutex::new(vec![
            Ok(vec![listed("old00001", here)]),
            Err("no answer".into()),
            Ok(vec![
                listed("old00001", here),
                listed("away0001", elsewhere),
                listed("new00001", here),
            ]),
        ]));
        let id = start(&a, &echo("old00001 away0001 new00001"), here, &before, LISTED_WAIT).unwrap();
        assert_eq!(id, "new00001");
        // Nothing listed it: the error is what it printed last.
        let a = Scripted(std::sync::Mutex::new(vec![Ok(vec![listed("old00001", here)])]));
        let at = Instant::now();
        let printed = "echo started old00001; echo 'not logged in'";
        let e = start(
            &a,
            &["sh".into(), "-c".into(), printed.into()],
            here,
            &before,
            Duration::from_millis(600),
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "not logged in");
        assert!(at.elapsed() < Duration::from_secs(3));
        // No id in what it printed at all: said at once.
        let e = start(&a, &echo("no such session"), here, &before, LISTED_WAIT).unwrap_err();
        assert_eq!(e.to_string(), "no such session");
    }

    #[test]
    fn a_start_waits_no_longer_than_its_deadline_for_a_slow_listing() {
        struct Slow;
        impl Adapter for Slow {
            fn harness(&self) -> &str {
                "fake"
            }
            fn list_within(&self, wait: Duration) -> Result<Vec<NativeRow>> {
                // Takes whatever it is given, as a hung harness would.
                std::thread::sleep(wait);
                Err(anyhow!("no answer"))
            }
            fn attach_argv(&self, _: &str) -> Vec<String> {
                Vec::new()
            }
            fn start_argv(&self, _: Option<&str>, _: Option<&str>) -> Option<Vec<String>> {
                None
            }
            fn carry_on_argv(&self, _: &str) -> Option<Vec<String>> {
                None
            }
            fn stop_argv(&self, _: &str) -> Option<Vec<String>> {
                None
            }
            fn started_ids(&self, _: &str) -> Vec<String> {
                vec!["abcd1234".into()]
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let at = Instant::now();
        let e = start(
            &Slow,
            &["echo".into(), "abcd1234".into()],
            tmp.path(),
            &HashSet::new(),
            Duration::from_millis(400),
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "abcd1234");
        assert!(at.elapsed() < Duration::from_secs(2), "{:?}", at.elapsed());
    }

    #[test]
    fn claudes_and_codexs_profiles_have_an_adapter() {
        let all = adapters(&Registry::builtin(), crate::registry::find_bin);
        let names: Vec<&str> = all.iter().map(|(a, _)| a.harness()).collect();
        assert_eq!(names, ["claude", "codex"]);
    }
}
