//! `~/.relay/` — one place for everything user-level. `RELAY_HOME` overrides
//! it, which is also how tests stay out of the real one.

pub mod id;
pub mod rules_dir;
pub mod time;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const HOME_ENV: &str = "RELAY_HOME";

/// Conservative `sun_path` limit (macOS is the smallest at 104).
const SUN_PATH_MAX: usize = 104;
/// Room for `/session-xxxxxxxx.sock` style names on top of the directory.
const SOCKET_NAME_BUDGET: usize = 24;

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// FNV-1a, 32-bit: deterministic across Rust toolchains/versions, unlike
/// `std::hash::DefaultHasher`.
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0x811c9dc5u32, |h, &b| (h ^ b as u32).wrapping_mul(0x01000193))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home(PathBuf);

impl Home {
    /// `$RELAY_HOME`, else `$HOME/.relay`.
    pub fn resolve() -> Result<Self> {
        if let Some(p) = std::env::var_os(HOME_ENV) {
            return Ok(Self(PathBuf::from(p)));
        }
        let home = std::env::var_os("HOME").context("neither RELAY_HOME nor HOME is set")?;
        Ok(Self(PathBuf::from(home).join(".relay")))
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
    pub fn run_dir(&self) -> PathBuf {
        self.0.join("run")
    }
    pub fn log_dir(&self) -> PathBuf {
        self.0.join("log")
    }
    pub fn plugins_dir(&self) -> PathBuf {
        self.0.join("plugins")
    }
    /// User overrides for the detection registry (`<cli>.yaml`).
    pub fn detect_dir(&self) -> PathBuf {
        self.0.join("detect")
    }
    /// Settings files `rld` writes so CLIs report back (`claude.json`).
    pub fn hooks_dir(&self) -> PathBuf {
        self.0.join("hooks")
    }
    /// Where sockets live. Normally `run/`, but `sockaddr_un` caps paths at
    /// ~104 bytes on macOS (108 on Linux), so a long `RELAY_HOME` gets a short
    /// deterministic directory under the temp dir instead. Both sides derive
    /// it from the home path with a fixed hash (not `DefaultHasher`, whose
    /// output isn't stable across toolchains), so nothing needs to be written
    /// down.
    pub fn socket_dir(&self) -> PathBuf {
        let run = self.run_dir();
        if run.as_os_str().len() + SOCKET_NAME_BUDGET < SUN_PATH_MAX {
            return run;
        }
        let hash = fnv1a(self.0.as_os_str().as_encoded_bytes());
        let uid = unsafe { libc_getuid() };
        std::env::temp_dir().join(format!("rl-{uid}-{hash:08x}"))
    }
    pub fn daemon_socket(&self) -> PathBuf {
        self.socket_dir().join("rld.sock")
    }
    pub fn daemon_pid(&self) -> PathBuf {
        self.run_dir().join("rld.pid")
    }
    pub fn daemon_log(&self) -> PathBuf {
        self.log_dir().join("rld.log")
    }
    pub fn ledger(&self) -> PathBuf {
        self.0.join("ledger.db")
    }
    /// Your defaults, human-edited; absent means all defaults.
    pub fn config_file(&self) -> PathBuf {
        self.0.join("config.yaml")
    }
    /// User-level schedules, human-edited.
    pub fn schedules_file(&self) -> PathBuf {
        self.0.join("schedules.yaml")
    }
    /// The user's own reactions: events → actions.
    pub fn reactions_file(&self) -> PathBuf {
        self.0.join("reactions.yaml")
    }
    /// Flows, a folder each: `<name>/flow.md` and the flow's own scripts.
    pub fn flows_dir(&self) -> PathBuf {
        self.0.join("flows")
    }
    pub fn flow_dir(&self, name: &str) -> PathBuf {
        self.flows_dir().join(name)
    }
    /// One firing's state: what its steps left for the steps after them.
    pub fn firing_dir(&self, name: &str, id: &str) -> PathBuf {
        self.flow_dir(name).join("runs").join(id)
    }
    /// Where a removed project's rule files are kept, a folder each: moved
    /// here rather than deleted, so taking a project away loses nothing.
    pub fn removed_dir(&self) -> PathBuf {
        self.0.join("removed")
    }

    pub fn session_socket(&self, run: &str) -> PathBuf {
        self.socket_dir().join(format!("session-{run}.sock"))
    }
    pub fn session_exit_file(&self, run: &str) -> PathBuf {
        self.run_dir().join(format!("session-{run}.exit"))
    }
    pub fn session_log(&self, run: &str) -> PathBuf {
        self.log_dir().join(format!("session-{run}.log"))
    }
    /// The child's environment, handed to the holder at start and removed once
    /// it has read it.
    pub fn session_env_file(&self, run: &str) -> PathBuf {
        self.run_dir().join(format!("session-{run}.env"))
    }

    /// Creates the directory tree. Idempotent. `run/` is 0700 since it holds sockets.
    pub fn ensure_dirs(&self) -> Result<()> {
        for d in [
            self.0.clone(),
            self.run_dir(),
            self.log_dir(),
            self.plugins_dir(),
            self.detect_dir(),
            self.hooks_dir(),
            self.socket_dir(),
        ] {
            std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for d in [self.run_dir(), self.socket_dir()] {
                std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }
}

/// Finds a sibling Relay binary: `$<env_var>`, else next to this executable,
/// else bare `name` for `PATH` lookup.
pub fn locate_bin(name: &str, env_var: &str) -> PathBuf {
    if let Some(p) = std::env::var_os(env_var) {
        return PathBuf::from(p);
    }
    if let Ok(me) = std::env::current_exe() {
        let sibling = me.with_file_name(name);
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from(name)
}

/// Renders a path with `$HOME` collapsed to `~`, for humans.
pub fn display_path(p: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rest) = p.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    p.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_dirs_builds_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("relay"));
        home.ensure_dirs().unwrap();
        assert!(home.run_dir().is_dir());
        assert!(home.log_dir().is_dir());
        assert_eq!(home.daemon_socket(), home.run_dir().join("rld.sock"));
    }

    #[test]
    fn a_firing_lives_in_its_flow_folder() {
        let home = Home::at("/h");
        assert_eq!(home.flows_dir(), Path::new("/h/flows"));
        assert_eq!(home.flow_dir("pulse"), Path::new("/h/flows/pulse"));
        assert_eq!(home.firing_dir("pulse", "f1"), Path::new("/h/flows/pulse/runs/f1"));
    }

    #[test]
    fn long_home_gets_short_socket_dir() {
        let long = std::path::PathBuf::from("/tmp").join("x".repeat(120));
        let home = Home::at(&long);
        let sock = home.daemon_socket();
        assert!(sock.as_os_str().len() < SUN_PATH_MAX, "{}", sock.display());
        assert!(!sock.starts_with(&long));
        home.ensure_dirs().unwrap();
        assert!(sock.parent().unwrap().is_dir(), "socket dir must be created");
        let _ = std::fs::remove_dir(sock.parent().unwrap());
        assert_eq!(sock, Home::at(&long).daemon_socket(), "must be deterministic");
        assert_ne!(sock, Home::at(long.join("y")).daemon_socket(), "must differ per home");
    }
}
