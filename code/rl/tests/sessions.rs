//! `rl-new new | ls | stop | rm` against the real `rl-session` holder.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn session_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl-new")).with_file_name("rl-session");
    p.is_file().then_some(p)
}

struct Env {
    tmp: tempfile::TempDir,
    session: PathBuf,
}

impl Env {
    fn new() -> Option<Self> {
        let Some(session) = session_bin() else {
            eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
            return None;
        };
        Some(Self {
            tmp: tempfile::tempdir().unwrap(),
            session,
        })
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("relay")
    }

    fn dir(&self, name: &str) -> PathBuf {
        let d = self.tmp.path().join(name);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn rl(&self, cwd: &Path, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_rl-new"))
            .args(args)
            .current_dir(cwd)
            .env("RELAY_HOME", self.home())
            .env("HOME", self.tmp.path())
            .env("RELAY_SESSION_BIN", &self.session)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }

    fn ls(&self) -> Vec<serde_json::Value> {
        let (ok, out, err) = self.rl(self.tmp.path(), &["ls", "--json"]);
        assert!(ok, "{err}");
        serde_json::from_str(&out).unwrap()
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn new_ls_stop_rm() {
    let Some(env) = Env::new() else { return };
    let cwd = env.dir("proj");
    let (ok, name, err) = env.rl(&cwd, &["new", "--", "sh", "-c", "echo hi; sleep 30"]);
    assert!(ok, "{err}");
    assert_eq!(name, "proj-sh");

    let row = || env.ls().into_iter().find(|r| r["name"] == name).unwrap();
    wait_for("a live session with a state", || {
        let r = row();
        r["status"] == "live" && r["state"].is_string()
    });
    let r = row();
    assert_eq!(r["harness"], "sh");
    assert!(r["holder_pid"].as_u64().unwrap() > 0);

    // A live session is not removed.
    let (ok, _, err) = env.rl(&cwd, &["rm", &name]);
    assert!(!ok);
    assert!(err.contains("stop"), "{err}");

    let (ok, _, err) = env.rl(&cwd, &["stop", &name]);
    assert!(ok, "{err}");
    assert!(env.home().join("sessions").join(&name).join("exit").is_file());
    assert_ne!(row()["status"], "live");

    let (ok, _, err) = env.rl(&cwd, &["rm", &name]);
    assert!(ok, "{err}");
    assert!(!env.home().join("sessions").join(&name).exists());
    assert!(env.ls().is_empty());
}

#[test]
fn two_in_one_folder_get_distinct_names() {
    let Some(env) = Env::new() else { return };
    let cwd = env.dir("twin");
    let (ok, a, err) = env.rl(&cwd, &["new", "--name-only", "--", "sleep", "30"]);
    assert!(ok, "{err}");
    let (ok, b, err) = env.rl(&cwd, &["new", "--name-only", "--", "sleep", "30"]);
    assert!(ok, "{err}");
    assert_ne!(a, b);
    for n in [&a, &b] {
        assert!(env.rl(&cwd, &["stop", n]).0);
    }
}

#[test]
fn a_failing_argv_leaves_no_folder() {
    let Some(env) = Env::new() else { return };
    let cwd = env.dir("bad");
    let (ok, _, err) = env.rl(&cwd, &["new", "--", "/nonexistent"]);
    assert!(!ok);
    assert!(!err.is_empty());
    let sessions = env.home().join("sessions");
    let left = std::fs::read_dir(&sessions).map(|d| d.count()).unwrap_or(0);
    assert_eq!(left, 0, "{err}");
}
