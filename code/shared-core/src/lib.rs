//! `~/.relay/` — one place for everything user-level. `RELAY_HOME` overrides
//! it, which is also how tests stay out of the real one.

pub mod id;
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
    /// Where sockets live. Normally `run/`, but `sockaddr_un` caps paths at
    /// ~104 bytes on macOS (108 on Linux), so a long `RELAY_HOME` gets a short
    /// deterministic directory under the temp dir instead. Both sides derive
    /// it from the home path, so nothing needs to be written down.
    pub fn socket_dir(&self) -> PathBuf {
        let run = self.run_dir();
        if run.as_os_str().len() + SOCKET_NAME_BUDGET < SUN_PATH_MAX {
            return run;
        }
        let mut h = std::hash::DefaultHasher::new();
        std::hash::Hash::hash(&self.0, &mut h);
        let uid = unsafe { libc_getuid() };
        std::env::temp_dir().join(format!("rl-{uid}-{:08x}", std::hash::Hasher::finish(&h) as u32))
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
    pub fn session_socket(&self, run: &str) -> PathBuf {
        self.socket_dir().join(format!("session-{run}.sock"))
    }
    pub fn session_exit_file(&self, run: &str) -> PathBuf {
        self.run_dir().join(format!("session-{run}.exit"))
    }
    pub fn session_log(&self, run: &str) -> PathBuf {
        self.log_dir().join(format!("session-{run}.log"))
    }

    /// Creates the directory tree. Idempotent. `run/` is 0700 since it holds sockets.
    pub fn ensure_dirs(&self) -> Result<()> {
        for d in [
            self.0.clone(),
            self.run_dir(),
            self.log_dir(),
            self.plugins_dir(),
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
