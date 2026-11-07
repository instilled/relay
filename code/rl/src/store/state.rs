//! What agents' hooks say of their own sessions, as plain files:
//! `state/<agent>/<session_id>.json`, written by `rl hook`. A session held by
//! Relay keeps its word in its own folder (`sessions/<name>/hook.json`)
//! instead; both are a `HookState`.

use super::sessions::pid_alive;
use jiff::{SignedDuration, Timestamp};
use rl_core::Home;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// A session with no process known and no word for this long is gone.
const STALE: SignedDuration = SignedDuration::from_hours(12);
/// A session with a process known and no word for this long is gone, though
/// the pid may live on (reused, or a hung agent).
const QUIET: SignedDuration = SignedDuration::from_hours(48);

/// How a hooked session stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Said {
    Idle,
    Busy,
    Waiting,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookState {
    pub agent: String,
    pub session_id: String,
    pub cwd: PathBuf,
    pub state: Said,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<PathBuf>,
    /// When the last word came.
    pub at: Timestamp,
}

/// `state/<agent>/<session_id>.json`; None where either would leave `state/`.
pub fn path(home: &Home, agent: &str, session_id: &str) -> Option<PathBuf> {
    let safe = |s: &str| !s.is_empty() && !s.contains(['/', '\\', '\0']) && !s.starts_with('.');
    (safe(agent) && safe(session_id)).then(|| home.state_dir().join(agent).join(format!("{session_id}.json")))
}

/// `Ok(None)` when the file does not exist.
pub fn read(path: &Path) -> io::Result<Option<HookState>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn write(path: &Path, s: &HookState) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    rl_core::atomic::write_json(path, s)
}

/// Whether `s` is still there at `now`: its folder is, and its process lives
/// and spoke within `QUIET`, or (none known) it spoke within `STALE`.
fn stands(s: &HookState, now: Timestamp, alive: impl Fn(u32) -> bool) -> bool {
    let quiet = now.duration_since(s.at);
    s.cwd.is_dir()
        && match s.pid {
            Some(pid) => quiet < QUIET && alive(pid),
            None => quiet < STALE,
        }
}

/// Every hooked session that still stands, by agent and id. Files of the
/// ones that do not are deleted; a file that does not read is left alone.
pub fn list(home: &Home, now: Timestamp) -> Vec<HookState> {
    list_with(home, now, pid_alive)
}

fn list_with(home: &Home, now: Timestamp, alive: impl Fn(u32) -> bool) -> Vec<HookState> {
    let mut out = Vec::new();
    let Ok(agents) = std::fs::read_dir(home.state_dir()) else {
        return out;
    };
    for agent in agents.flatten().filter(|a| a.path().is_dir()) {
        let Ok(files) = std::fs::read_dir(agent.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            match read(&path) {
                Ok(Some(s)) if stands(&s, now, &alive) => out.push(s),
                Ok(Some(_)) => {
                    let _ = std::fs::remove_file(&path);
                }
                Ok(None) => {}
                Err(e) => tracing::debug!(path = %path.display(), "skipping state file: {e}"),
            }
        }
    }
    out.sort_by(|a, b| (&a.agent, &a.session_id).cmp(&(&b.agent, &b.session_id)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(home: &Home, id: &str, cwd: &Path, pid: Option<u32>, at: Timestamp) -> PathBuf {
        let p = path(home, "claude", id).unwrap();
        let s = HookState {
            agent: "claude".into(),
            session_id: id.into(),
            cwd: cwd.into(),
            state: Said::Busy,
            detail: None,
            pid,
            transcript_path: None,
            at,
        };
        write(&p, &s).unwrap();
        p
    }

    #[test]
    fn list_prunes_what_is_gone_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("relay"));
        let cwd = tmp.path();
        let now = Timestamp::now();
        let ago = |h: i64| now - SignedDuration::from_hours(h);
        let live = |pid: u32| pid == 1;

        let kept = put(&home, "kept", cwd, Some(1), ago(47));
        let kept_no_pid = put(&home, "kept-no-pid", cwd, None, ago(11));
        let dead = put(&home, "dead", cwd, Some(2), now);
        let quiet = put(&home, "quiet", cwd, Some(1), ago(49));
        let stale = put(&home, "stale", cwd, None, ago(13));
        let no_folder = put(&home, "no-folder", &tmp.path().join("gone"), Some(1), now);

        let ids: Vec<_> = list_with(&home, now, live).into_iter().map(|s| s.session_id).collect();
        assert_eq!(ids, ["kept", "kept-no-pid"]);
        assert!(kept.exists() && kept_no_pid.exists());
        for p in [dead, quiet, stale, no_folder] {
            assert!(!p.exists(), "{}", p.display());
        }
    }

    #[test]
    fn a_dead_pid_is_pruned_for_real() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("relay"));
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let dead = c.id();
        c.wait().unwrap();
        let p = put(&home, "d", tmp.path(), Some(dead), Timestamp::now());
        let me = put(&home, "me", tmp.path(), Some(std::process::id()), Timestamp::now());
        let ids: Vec<_> = list(&home, Timestamp::now())
            .into_iter()
            .map(|s| s.session_id)
            .collect();
        assert_eq!(ids, ["me"]);
        assert!(!p.exists() && me.exists());
    }

    #[test]
    fn a_path_stays_under_state() {
        let home = Home::at("/h");
        assert!(path(&home, "claude", "s1").is_some());
        assert!(path(&home, "claude", "../x").is_none());
        assert!(path(&home, "claude", "").is_none());
        assert!(path(&home, "a/b", "s").is_none());
        assert!(path(&home, "claude", ".hidden").is_none());
    }
}
