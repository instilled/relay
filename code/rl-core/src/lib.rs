//! `~/.relay/` — one place for everything user-level. `RELAY_HOME` overrides
//! it, which is also how tests stay out of the real one.

pub mod atomic;
pub mod state;
pub mod time;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const HOME_ENV: &str = "RELAY_HOME";

/// Conservative `sun_path` limit (macOS is the smallest at 104).
#[cfg(test)]
const SUN_PATH_MAX: usize = 104;
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
    pub fn plugins_dir(&self) -> PathBuf {
        self.0.join("plugins")
    }
    /// User overrides for the detection registry (`<cli>.yaml`).
    pub fn detect_dir(&self) -> PathBuf {
        self.0.join("detect")
    }
    /// Settings files `rl new` writes so CLIs report back (`claude.json`).
    pub fn hooks_dir(&self) -> PathBuf {
        self.0.join("hooks")
    }
    /// `$TMPDIR/rl-{uid}-{fnv1a(home)}`: short and per-home, for a socket
    /// path `sockaddr_un` would not take (~104 bytes on macOS, 108 on Linux).
    /// It is derived from the home path with a fixed hash (not
    /// `DefaultHasher`, whose output isn't stable across toolchains), so
    /// nothing needs to be written down.
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
        let long = PathBuf::from("/tmp").join("z".repeat(120));
        let home = Home::at(&long);
        let sock = home.session_socket_for("a");
        assert!(sock.as_os_str().len() < SUN_PATH_MAX, "{}", sock.display());
        assert!(sock.starts_with(home.short_socket_dir()));
        assert_eq!(sock.file_name().unwrap(), "a.sock");
        assert!(sock.parent().unwrap().is_dir());
        assert_eq!(sock, Home::at(&long).session_socket_for("a"), "must be deterministic");
        let other = Home::at(long.join("y")).session_socket_for("a");
        assert_ne!(sock.parent(), other.parent(), "must differ per home");
        let _ = std::fs::remove_dir(sock.parent().unwrap());
        let _ = std::fs::remove_dir(other.parent().unwrap());
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
