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
use rl_core::state::RunState;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
    fn list(&self) -> Result<Vec<NativeRow>>;
    /// What attaches this terminal to the session `id`.
    fn attach_argv(&self, id: &str) -> Vec<String>;
    /// What starts a session in its background, named and prompted if
    /// given. None: it has no background, and Relay holds what it starts.
    fn start_argv(&self, name: Option<&str>, prompt: Option<&str>) -> Option<Vec<String>>;
    /// What carries the conversation `session_id` on in its background.
    /// None: it has no background to carry it on in.
    fn carry_on_argv(&self, session_id: &str) -> Option<Vec<String>>;
    /// The id of the session a start or a carry-on said it began, in what
    /// it printed.
    fn started_id(&self, printed: &str) -> Option<String>;
}

/// How long a list may take.
pub const LIST_WAIT: Duration = Duration::from_secs(10);
/// How long a start or a carry-on may take to say its id.
pub const START_WAIT: Duration = Duration::from_secs(30);

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
    let mut child = cmd.spawn().with_context(|| format!("running `{what}`"))?;
    let read = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut s);
            }
            String::from_utf8_lossy(&s).into_owned()
        })
    };
    let out = read(child.stdout.take().map(|o| Box::new(o) as _));
    let err = read(child.stderr.take().map(|e| Box::new(e) as _));
    let deadline = Instant::now() + wait;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("`{what}` did not answer in {}s", wait.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
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
    fn claudes_and_codexs_profiles_have_an_adapter() {
        let all = adapters(&Registry::builtin(), crate::registry::find_bin);
        let names: Vec<&str> = all.iter().map(|(a, _)| a.harness()).collect();
        assert_eq!(names, ["claude", "codex"]);
    }
}
