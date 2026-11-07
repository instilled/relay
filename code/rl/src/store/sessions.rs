//! Relay-held sessions as plain folders: `sessions/<name>/{meta.json, state.json, exit, sock, ..}`.
//! There is no registry: the folders are the truth, and a session's liveness is
//! read from its files and its socket.

use anyhow::{Result, bail};
use jiff::Timestamp;
use rl_core::Home;
use rl_core::state::RunKind;
use rl_session::exit::{Exit, read_exit_file};
use rl_session::state::{StateFile, read_state_file};
use serde::{Deserialize, Serialize};
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

const META: &str = "meta.json";
const EXIT: &str = "exit";
const STATE: &str = "state.json";
/// A folder with no `meta.json` younger than this is a claim in progress.
const CLAIM_GRACE: Duration = Duration::from_secs(10);
/// A lost session whose holder pid was never recorded is swept after this.
const NO_PID_GRACE: Duration = Duration::from_secs(60);
/// Longest folder-name part of a session name.
const FOLDER_PART_MAX: usize = 24;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub name: String,
    /// Registry profile name; `sh` for shells.
    pub harness: String,
    pub kind: RunKind,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub started: Timestamp,
    pub socket: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Live,
    Ended(Exit),
    Lost,
}

#[derive(Debug, Clone)]
pub struct Held {
    pub meta: Meta,
    pub status: Status,
    pub state: Option<StateFile>,
    pub dir: PathBuf,
}

pub fn write_meta(dir: &Path, meta: &Meta) -> io::Result<()> {
    rl_core::atomic::write_json(&dir.join(META), meta)
}

pub fn read_meta(dir: &Path) -> io::Result<Meta> {
    let bytes = std::fs::read(dir.join(META))?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

/// `<folder>-<harness>`, lower-cased, anything outside `[a-z0-9-]` turned into
/// `-` and runs of `-` collapsed; the folder part is capped at 24 characters.
pub fn base_name(folder: &str, harness: &str) -> String {
    let folder = slug(folder);
    let folder = folder[..folder.len().min(FOLDER_PART_MAX)].trim_matches('-');
    let harness = slug(harness);
    match (folder.is_empty(), harness.is_empty()) {
        (true, true) => "session".into(),
        (true, false) => harness,
        (false, true) => folder.to_string(),
        (false, false) => format!("{folder}-{harness}"),
    }
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        let c = if c.is_ascii_alphanumeric() { c } else { '-' };
        if c == '-' && out.ends_with('-') {
            continue;
        }
        out.push(c);
    }
    out.trim_matches('-').to_string()
}

/// Creates `sessions/<name>/` for the first free of `base`, `base-2`, `base-3`…
/// (`create_dir` fails if the folder exists, so two callers never share one).
pub fn claim(home: &Home, base: &str) -> io::Result<(String, PathBuf)> {
    std::fs::create_dir_all(home.sessions_dir())?;
    for n in 1u32.. {
        let name = if n == 1 {
            base.to_string()
        } else {
            format!("{base}-{n}")
        };
        let dir = home.session_dir(&name);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok((name, dir)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("u32 suffixes exhausted")
}

fn age(path: &Path) -> Duration {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .unwrap_or_default()
}

fn status_of(dir: &Path, meta: &Meta) -> Status {
    if let Ok(Some(exit)) = read_exit_file(&dir.join(EXIT)) {
        Status::Ended(exit)
    } else if UnixStream::connect(&meta.socket).is_ok() {
        Status::Live
    } else {
        Status::Lost
    }
}

/// Every session folder with a `meta.json`, by name.
pub fn list(home: &Home) -> Vec<Held> {
    let Ok(rd) = std::fs::read_dir(home.sessions_dir()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let meta = match read_meta(&dir) {
            Ok(m) => m,
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound || age(&dir) >= CLAIM_GRACE {
                    tracing::debug!(dir = %dir.display(), "skipping session folder: {e}");
                }
                continue;
            }
        };
        let status = status_of(&dir, &meta);
        let state = read_state_file(&dir.join(STATE)).ok().flatten();
        out.push(Held {
            meta,
            status,
            state,
            dir,
        });
    }
    out.sort_by(|a, b| a.meta.name.cmp(&b.meta.name));
    out
}

fn pid_alive(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    match kill(Pid::from_raw(pid), None) {
        Ok(()) => true,
        Err(e) => e == Errno::EPERM,
    }
}

/// Removes lost sessions whose holder is gone: its pid is dead, or was never
/// recorded and the session is older than a minute.
pub fn sweep(home: &Home) {
    for held in list(home) {
        if held.status != Status::Lost {
            continue;
        }
        let gone = match held.meta.holder_pid {
            Some(pid) => !pid_alive(pid),
            None => {
                let started = Timestamp::now().duration_since(held.meta.started);
                started.as_secs() >= NO_PID_GRACE.as_secs() as i64
            }
        };
        if gone {
            discard(&held.dir, &held.meta);
        }
    }
}

/// Removes a session's folder; refuses while its holder is live.
pub fn remove(home: &Home, name: &str) -> Result<()> {
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        bail!("no such session: {name}");
    }
    let dir = home.session_dir(name);
    let meta = match read_meta(&dir) {
        Ok(m) => Some(m),
        Err(e) if e.kind() == io::ErrorKind::NotFound && dir.is_dir() => None,
        Err(e) if e.kind() == io::ErrorKind::NotFound => bail!("no such session: {name}"),
        Err(e) => return Err(e.into()),
    };
    if let Some(meta) = &meta
        && status_of(&dir, meta) == Status::Live
    {
        bail!("{name} is still running; stop it first");
    }
    if let Some(m) = &meta {
        discard_socket(&dir, m);
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Deletes a session's folder, and its socket when that lives elsewhere (the
/// long-home fallback from `Home::session_socket_for`).
fn discard(dir: &Path, meta: &Meta) {
    discard_socket(dir, meta);
    let _ = std::fs::remove_dir_all(dir);
}

fn discard_socket(dir: &Path, meta: &Meta) {
    if !meta.socket.starts_with(dir) {
        let _ = std::fs::remove_file(&meta.socket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn home() -> (tempfile::TempDir, Home) {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("relay"));
        home.ensure_store().unwrap();
        (tmp, home)
    }

    fn meta(name: &str, socket: PathBuf, holder_pid: Option<u32>) -> Meta {
        Meta {
            name: name.into(),
            harness: "claude".into(),
            kind: RunKind::Agent,
            cwd: "/tmp".into(),
            argv: vec!["claude".into()],
            started: Timestamp::UNIX_EPOCH,
            socket,
            holder_pid,
            native_id: None,
        }
    }

    fn session(home: &Home, name: &str, pid: Option<u32>) -> (PathBuf, Meta) {
        let (name, dir) = claim(home, name).unwrap();
        let m = meta(&name, dir.join("sock"), pid);
        write_meta(&dir, &m).unwrap();
        (dir, m)
    }

    fn dead_pid() -> u32 {
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    #[test]
    fn claim_dedupes() {
        let (_t, home) = home();
        assert_eq!(claim(&home, "relay-claude").unwrap().0, "relay-claude");
        assert_eq!(claim(&home, "relay-claude").unwrap().0, "relay-claude-2");
        let (n, dir) = claim(&home, "relay-claude").unwrap();
        assert_eq!(n, "relay-claude-3");
        assert!(dir.is_dir());
    }

    #[test]
    fn base_names_are_slugs() {
        assert_eq!(base_name("Relay", "claude"), "relay-claude");
        assert_eq!(base_name("my  proj_x", "sh"), "my-proj-x-sh");
        assert_eq!(base_name(&"a".repeat(40), "codex"), format!("{}-codex", "a".repeat(24)));
        assert_eq!(base_name("", "sh"), "sh");
    }

    #[test]
    fn meta_roundtrip_and_strict() {
        let (_t, home) = home();
        let (dir, m) = session(&home, "a", Some(7));
        assert_eq!(read_meta(&dir).unwrap(), m);
        std::fs::write(dir.join(META), r#"{"name":"a","bogus":1}"#).unwrap();
        assert!(read_meta(&dir).is_err());
    }

    #[test]
    fn list_reports_ended_lost_and_live() {
        let (_t, home) = home();
        let (ended, _) = session(&home, "ended", None);
        let exit = Exit {
            code: 0,
            signal: None,
            at: Timestamp::UNIX_EPOCH,
        };
        rl_core::atomic::write_json(&ended.join(EXIT), &exit).unwrap();
        session(&home, "lost", None);
        let (live_dir, _) = session(&home, "live", None);
        let _listener = UnixListener::bind(live_dir.join("sock")).unwrap();

        let held = list(&home);
        let by = |n: &str| held.iter().find(|h| h.meta.name == n).unwrap();
        assert_eq!(by("ended").status, Status::Ended(exit));
        assert_eq!(by("lost").status, Status::Lost);
        assert_eq!(by("live").status, Status::Live);
        assert!(by("live").state.is_none());
    }

    #[test]
    fn list_skips_a_claim_in_progress() {
        let (_t, home) = home();
        claim(&home, "fresh").unwrap();
        assert!(list(&home).is_empty());
    }

    #[test]
    fn sweep_removes_only_lost_with_a_dead_pid() {
        let (_t, home) = home();
        let (dead, _) = session(&home, "dead", Some(dead_pid()));
        let (alive, _) = session(&home, "alive", Some(std::process::id()));
        let (live, _) = session(&home, "live", Some(dead_pid()));
        let _listener = UnixListener::bind(live.join("sock")).unwrap();
        let (ended, _) = session(&home, "ended", Some(dead_pid()));
        let exit = Exit {
            code: 1,
            signal: None,
            at: Timestamp::UNIX_EPOCH,
        };
        rl_core::atomic::write_json(&ended.join(EXIT), &exit).unwrap();

        sweep(&home);
        assert!(!dead.exists());
        assert!(alive.exists());
        assert!(live.exists());
        assert!(ended.exists());
    }

    #[test]
    fn remove_refuses_live_and_removes_the_rest() {
        let (_t, home) = home();
        let (live, _) = session(&home, "live", None);
        let _listener = UnixListener::bind(live.join("sock")).unwrap();
        assert!(remove(&home, "live").is_err());
        assert!(live.exists());

        let (lost, _) = session(&home, "lost", None);
        remove(&home, "lost").unwrap();
        assert!(!lost.exists());
        assert!(remove(&home, "lost").is_err());
        assert!(remove(&home, "../x").is_err());
    }

    #[test]
    fn remove_and_sweep_delete_a_socket_outside_the_folder() {
        let (t, home) = home();
        let far = |n: &str| t.path().join(format!("{n}.sock"));
        let with_far_socket = |name: &str, pid: Option<u32>| {
            let (name, dir) = claim(&home, name).unwrap();
            let m = meta(&name, far(&name), pid);
            write_meta(&dir, &m).unwrap();
            std::fs::write(&m.socket, b"").unwrap();
            dir
        };
        let removed = with_far_socket("removed", None);
        remove(&home, "removed").unwrap();
        assert!(!removed.exists());
        assert!(!far("removed").exists());

        let swept = with_far_socket("swept", Some(dead_pid()));
        sweep(&home);
        assert!(!swept.exists());
        assert!(!far("swept").exists());
    }
}
