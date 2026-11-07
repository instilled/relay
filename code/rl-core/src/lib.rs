//! `~/.relay/` — one place for everything user-level. `RELAY_HOME` overrides
//! it, which is also how tests stay out of the real one.

pub mod atomic;
pub mod folder;
pub mod id;
pub mod state;
pub mod time;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const HOME_ENV: &str = "RELAY_HOME";

/// Conservative `sun_path` limit (macOS is the smallest at 104).
const SUN_PATH_MAX: usize = 104;
/// Room for `/session-xxxxxxxx.sock` style names on top of the directory.
const SOCKET_NAME_BUDGET: usize = 24;
/// A session socket path must be shorter than this to stay inline.
const SOCKET_PATH_LIMIT: usize = 100;

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
    /// `rld`'s own, put on each flow step's `PATH`: only a link to the `rl` beside `rld`.
    pub fn bin_dir(&self) -> PathBuf {
        self.run_dir().join("bin")
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
        self.short_socket_dir()
    }
    /// `$TMPDIR/rl-{uid}-{fnv1a(home)}`: short and per-home.
    fn short_socket_dir(&self) -> PathBuf {
        let hash = fnv1a(self.0.as_os_str().as_encoded_bytes());
        let uid = unsafe { libc_getuid() };
        std::env::temp_dir().join(format!("rl-{uid}-{hash:08x}"))
    }
    pub fn sessions_dir(&self) -> PathBuf {
        self.0.join("sessions")
    }
    pub fn session_dir(&self, name: &str) -> PathBuf {
        self.sessions_dir().join(name)
    }
    pub fn state_dir(&self) -> PathBuf {
        self.0.join("state")
    }
    pub fn mail_dir(&self) -> PathBuf {
        self.0.join("mail")
    }
    pub fn config_json(&self) -> PathBuf {
        self.0.join("config.json")
    }
    /// `sessions/<name>/sock`, unless that is too long for `sockaddr_un`:
    /// then `<short socket dir>/<name>.sock` (the directory is created, 0700).
    pub fn session_socket_for(&self, name: &str) -> PathBuf {
        let inline = self.session_dir(name).join("sock");
        if inline.as_os_str().len() < SOCKET_PATH_LIMIT {
            return inline;
        }
        let dir = self.short_socket_dir();
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        dir.join(format!("{name}.sock"))
    }
    /// Creates `sessions/`, `state/` and `mail/` (0700). Idempotent.
    pub fn ensure_store(&self) -> std::io::Result<()> {
        for d in [self.sessions_dir(), self.state_dir(), self.mail_dir()] {
            std::fs::create_dir_all(&d)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
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
    /// Where a run's terminal bytes are journaled now: an append-only file per
    /// direction under `io/<run>/`, out of the ledger so `ledger.db` stays
    /// small. The ledger keeps only where each chunk sits in these files.
    pub fn io_dir(&self) -> PathBuf {
        self.0.join("io")
    }
    pub fn session_io_dir(&self, run: &str) -> PathBuf {
        self.io_dir().join(run)
    }
    /// Your defaults, human-edited; absent means all defaults.
    pub fn config_file(&self) -> PathBuf {
        self.0.join("config.yaml")
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
            self.io_dir(),
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
    fn a_runs_terminal_bytes_live_under_io() {
        let home = Home::at("/h");
        assert_eq!(home.io_dir(), Path::new("/h/io"));
        assert_eq!(home.session_io_dir("r1"), Path::new("/h/io/r1"));
    }

    #[test]
    fn ensure_dirs_makes_the_io_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("relay"));
        home.ensure_dirs().unwrap();
        assert!(home.io_dir().is_dir());
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

    #[test]
    fn a_short_home_keeps_the_socket_in_the_session_dir() {
        let home = Home::at("/h");
        assert_eq!(home.sessions_dir(), Path::new("/h/sessions"));
        assert_eq!(home.state_dir(), Path::new("/h/state"));
        assert_eq!(home.mail_dir(), Path::new("/h/mail"));
        assert_eq!(home.config_json(), Path::new("/h/config.json"));
        assert_eq!(home.session_socket_for("a"), Path::new("/h/sessions/a/sock"));
    }

    #[test]
    fn a_long_home_gets_a_short_socket_under_the_fallback_dir() {
        // Distinct from the other long-home test so their dirs don't race.
        let long = PathBuf::from("/tmp").join("z".repeat(120));
        let home = Home::at(&long);
        let sock = home.session_socket_for("a");
        assert!(sock.as_os_str().len() < SUN_PATH_MAX, "{}", sock.display());
        assert!(sock.starts_with(home.short_socket_dir()));
        assert_eq!(sock.file_name().unwrap(), "a.sock");
        assert!(sock.parent().unwrap().is_dir());
        let _ = std::fs::remove_dir(sock.parent().unwrap());
    }

    #[test]
    fn ensure_store_makes_the_three_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("relay"));
        home.ensure_store().unwrap();
        home.ensure_store().unwrap();
        for d in [home.sessions_dir(), home.state_dir(), home.mail_dir()] {
            assert!(d.is_dir());
        }
    }
}
