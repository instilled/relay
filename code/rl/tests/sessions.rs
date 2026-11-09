//! `rl new | ls | stop | rm` against the real `rl-session` holder.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn session_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name("rl-session");
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
        let out = Command::new(env!("CARGO_BIN_EXE_rl"))
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

/// Whether `pid` is still running. A holder is detached, so once it exits it
/// can sit as a zombie until something reaps it — on a CI runner whose init
/// doesn't, for good. `kill(pid, 0)` succeeds on a zombie, so on Linux the
/// state in `/proc` decides.
fn running(pid: i32) -> bool {
    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
        return false;
    }
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // The state follows the parenthesised command name.
        Ok(stat) => stat
            .rsplit_once(')')
            .is_none_or(|(_, rest)| !rest.trim_start().starts_with('Z')),
        Err(_) => true,
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
fn a_stopped_session_reads_stopped_whatever_its_code() {
    let Some(env) = Env::new() else { return };
    let cwd = env.dir("hup");
    // As zsh does: a hang-up, and it exits 1.
    let script = "trap 'exit 1' HUP; while :; do sleep 1; done";
    let (ok, name, err) = env.rl(&cwd, &["new", "--name-only", "--", "sh", "-c", script]);
    assert!(ok, "{err}");
    let (ok, _, err) = env.rl(&cwd, &["stop", &name]);
    assert!(ok, "{err}");
    let dir = env.home().join("sessions").join(&name);
    let exit: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("exit")).unwrap()).unwrap();
    assert_eq!(exit["code"], 1, "{exit}");
    assert!(dir.join("stopped").is_file(), "the stop said so in its folder");
    let row = env.ls().into_iter().find(|r| r["name"] == name).unwrap();
    assert_eq!(row["state"], "stopped", "{row}");
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

#[test]
fn meta_is_there_before_the_holder_starts_and_a_failed_start_leaves_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("marker");
    let fake = tmp.path().join("fake-session");
    // Records what the session folder held when the holder started (its
    // `--exit-file` argument sits in that folder), then fails.
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do [ \"$1\" = --exit-file ] && f=\"$2\"; shift; done\n\
             cat \"$(dirname \"$f\")/meta.json\" > {}\necho boom >&2\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cwd = tmp.path().join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rl"))
        .args(["new", "--", "sleep", "30"])
        .current_dir(&cwd)
        .env("RELAY_HOME", tmp.path().join("relay"))
        .env("HOME", tmp.path())
        .env("RELAY_SESSION_BIN", &fake)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
    assert_eq!(meta["name"], "proj-sleep");
    assert!(meta.get("holder_pid").is_none(), "no pid yet: {meta}");
    assert!(meta["started"].is_string());
    let sessions = tmp.path().join("relay/sessions");
    assert_eq!(std::fs::read_dir(&sessions).map(|d| d.count()).unwrap_or(0), 0);
}

#[test]
fn stopping_an_ended_session_lets_its_lingering_holder_go() {
    let Some(env) = Env::new() else { return };
    let cwd = env.dir("done");
    let (ok, name, err) = env.rl(&cwd, &["new", "--name-only", "--", "sh", "-c", "exit 0"]);
    assert!(ok, "{err}");
    let exit = env.home().join("sessions").join(&name).join("exit");
    wait_for("the exit file", || exit.is_file());
    let pid = env.ls().into_iter().find(|r| r["name"] == name).unwrap()["holder_pid"]
        .as_i64()
        .unwrap() as i32;
    let alive = || running(pid);
    assert!(alive(), "the holder lingers after its session ends");
    let (ok, _, err) = env.rl(&cwd, &["stop", &name]);
    assert!(ok, "{err}");
    wait_for("the holder to go", || !alive());
}

#[test]
fn removing_an_ended_session_closes_its_lingering_holder_first() {
    let Some(env) = Env::new() else { return };
    let cwd = env.dir("done");
    let (ok, name, err) = env.rl(&cwd, &["new", "--name-only", "--", "sh", "-c", "exit 0"]);
    assert!(ok, "{err}");
    let dir = env.home().join("sessions").join(&name);
    wait_for("the exit file", || dir.join("exit").is_file());
    let pid = env.ls().into_iter().find(|r| r["name"] == name).unwrap()["holder_pid"]
        .as_i64()
        .unwrap() as i32;
    let alive = || running(pid);
    assert!(alive(), "the holder lingers after its session ends");
    let (ok, _, err) = env.rl(&cwd, &["rm", &name]);
    assert!(ok, "{err}");
    assert!(!dir.exists());
    // Closed by rm, not left to find its socket gone: a moment, not a poll for ever.
    let until = Instant::now() + Duration::from_secs(2);
    while alive() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(), "the holder went with the session");
}

#[test]
fn color_sets_clears_and_errors() {
    let Some(env) = Env::new() else { return };
    let cwd = env.dir("proj");
    let (ok, name, err) = env.rl(&cwd, &["new", "--", "sh", "-c", "sleep 30"]);
    assert!(ok, "{err}");
    let config = || std::fs::read_to_string(env.home().join("config.json")).unwrap_or_default();

    let (ok, out, err) = env.rl(&cwd, &["color", &name, "pink"]);
    assert!(ok, "{err}");
    assert_eq!(out, format!("{name}: pink"));
    assert!(config().contains(&format!("\"r:{name}\": \"pink\"")), "{}", config());
    let (ok, _, err) = env.rl(&cwd, &["color", &name, "#ff8800"]);
    assert!(ok, "{err}");
    assert!(config().contains("#ff8800"));
    // A harness's own session by its key; one that is not there is an error.
    let (ok, _, err) = env.rl(&cwd, &["color", "n:claude:abc", "blue"]);
    assert!(ok, "{err}");
    let (ok, _, err) = env.rl(&cwd, &["color", "o:claude:nope", "blue"]);
    assert!(!ok && err.contains("no such session"), "{err}");
    let (ok, _, err) = env.rl(&cwd, &["color", "nobody", "blue"]);
    assert!(!ok && err.contains("no such session"), "{err}");
    let (ok, _, err) = env.rl(&cwd, &["color", &name, "chartreuse"]);
    assert!(!ok && err.contains("not a colour"), "{err}");
    assert!(config().contains("#ff8800"), "a bad colour changes nothing");

    let (ok, out, err) = env.rl(&cwd, &["color", &name, "none"]);
    assert!(ok, "{err}");
    assert!(out.contains("no colour"));
    assert!(!config().contains(&format!("r:{name}")));

    env.rl(&cwd, &["stop", &name]);
    env.rl(&cwd, &["rm", &name]);
}
