//! A held Codex learns its thread from Codex's first hook: the daemon runs
//! the hook, without the held session's environment, so the hook finds the
//! session by its folder and start (`rl hook` with `detached_env`).

use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Env {
    tmp: tempfile::TempDir,
    session: PathBuf,
}

impl Env {
    fn new() -> Option<Self> {
        let session = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name("rl-session");
        if !session.is_file() {
            eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
            return None;
        }
        let env = Self {
            tmp: tempfile::tempdir().unwrap(),
            session,
        };
        // `codex` is an `sh` here, and the agent a stand-in loop.
        let detect = env.home().join("detect");
        std::fs::create_dir_all(&detect).unwrap();
        std::fs::write(detect.join("codex.yaml"), "bins: [sh]\n").unwrap();
        Some(env)
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

    /// A held codex (an `sh` loop) in `cwd`; its name.
    fn codex(&self, cwd: &Path) -> String {
        let (ok, name, err) = self.rl(
            cwd,
            &[
                "new",
                "--name-only",
                "--harness",
                "codex",
                "--",
                "-c",
                "while :; do sleep 1; done",
            ],
        );
        assert!(ok, "{err}");
        let sock = self.home().join("sessions").join(&name).join("sock");
        wait_for("the holder's socket", || sock.exists());
        name
    }

    fn session(&self, name: &str) -> PathBuf {
        self.home().join("sessions").join(name)
    }

    fn meta(&self, name: &str) -> Value {
        read(&self.session(name).join("meta.json"))
    }

    /// Codex's hook, as its daemon runs it: no `RL_SESSION`.
    fn hook(&self, event: &str, sid: &str, cwd: &Path) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rl"))
            .args(["hook", "codex", event])
            .env_remove("RL_SESSION")
            .env_remove("RL_SESSION_AGENT")
            .env("RELAY_HOME", self.home())
            .env("HOME", self.tmp.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let payload = json!({"session_id": sid, "cwd": cwd, "hook_event_name": event});
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty());
    }

    fn state(&self, sid: &str) -> PathBuf {
        self.home().join("state/codex").join(format!("{sid}.json"))
    }

    fn ls(&self) -> Vec<Value> {
        let (ok, out, err) = self.rl(self.tmp.path(), &["ls", "--json"]);
        assert!(ok, "{err}");
        serde_json::from_str(&out).unwrap()
    }

    fn stop(&self, name: &str) {
        let _ = self.rl(self.tmp.path(), &["stop", name]);
    }
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_first_hook_claims_the_held_codex_in_its_folder() {
    let Some(env) = Env::new() else { return };
    let cwd = env.dir("proj");
    let name = env.codex(&cwd);
    // A leftover from before it was claimed.
    std::fs::create_dir_all(env.home().join("state/codex")).unwrap();
    std::fs::write(env.state("t-1"), "{}").unwrap();

    env.hook("SessionStart", "t-1", &cwd);
    assert_eq!(env.meta(&name)["native_id"], "t-1");
    let hook = read(&env.session(&name).join("hook.json"));
    assert_eq!(
        (hook["session_id"].as_str(), hook["agent"].as_str()),
        (Some("t-1"), Some("codex"))
    );
    assert!(!env.state("t-1").exists(), "not in state/");

    // A later hook for the same thread goes to the same file.
    env.hook("UserPromptSubmit", "t-1", &cwd);
    assert_eq!(read(&env.session(&name).join("hook.json"))["state"], "busy");
    assert!(!env.state("t-1").exists());

    // The held row stands once, whatever the daemon lists or hooks say.
    let rows = env.ls();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["name"], name.as_str());
    env.stop(&name);
}

#[test]
fn a_hook_with_no_such_held_codex_takes_the_outside_path() {
    let Some(env) = Env::new() else { return };
    let cwd = env.dir("proj");
    let elsewhere = env.dir("elsewhere");
    let name = env.codex(&cwd);

    env.hook("SessionStart", "t-2", &elsewhere);
    assert!(env.state("t-2").is_file());
    assert!(env.meta(&name).get("native_id").is_none());
    assert!(!env.session(&name).join("hook.json").exists());

    // Started too long ago to be the one that just began.
    let mut meta = env.meta(&name);
    meta["started"] = json!(
        jiff::Timestamp::now()
            .checked_sub(jiff::SignedDuration::from_secs(300))
            .unwrap()
    );
    std::fs::write(env.session(&name).join("meta.json"), meta.to_string()).unwrap();
    env.hook("SessionStart", "t-3", &cwd);
    assert!(env.state("t-3").is_file());
    assert!(env.meta(&name).get("native_id").is_none());
    env.stop(&name);
}

#[test]
fn a_thread_claimed_is_not_claimed_by_another_and_the_next_session_gets_the_next_thread() {
    let Some(env) = Env::new() else { return };
    let cwd = env.dir("proj");
    let (a, b) = (env.codex(&cwd), env.codex(&cwd));
    env.hook("SessionStart", "t-a", &cwd);
    env.hook("SessionStart", "t-b", &cwd);
    env.hook("Stop", "t-a", &cwd);
    assert_eq!(env.meta(&a)["native_id"], "t-a", "the earliest first");
    assert_eq!(env.meta(&b)["native_id"], "t-b");
    assert_eq!(read(&env.session(&a).join("hook.json"))["session_id"], "t-a");
    env.stop(&a);
    env.stop(&b);
}
