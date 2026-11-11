//! `rl new --harness codex` starts the session on Codex's own app-server
//! daemon, never in a holder: a thread in the folder, its first turn on the
//! prompt, the thread's id printed. With no prompt, `codex` runs here. The
//! daemon is a stand-in (`fixtures/stand-in-codex-daemon`), first on PATH.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Env {
    tmp: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let env = Self {
            tmp: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(env.bin_dir()).unwrap();
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-codex-daemon"),
            env.codex(),
        )
        .unwrap();
        env
    }

    fn bin_dir(&self) -> PathBuf {
        self.tmp.path().join("bin")
    }

    fn codex(&self) -> PathBuf {
        self.bin_dir().join("codex")
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("relay")
    }

    fn dir(&self, name: &str) -> PathBuf {
        let d = self.tmp.path().join(name);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn rl(&self, cwd: &Path, args: &[&str]) -> (bool, String, String) {
        let path = format!(
            "{}:{}",
            self.bin_dir().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new(env!("CARGO_BIN_EXE_rl"))
            .args(args)
            .current_dir(cwd)
            .env("RELAY_HOME", self.home())
            .env("HOME", self.tmp.path())
            .env("PATH", path)
            .env_remove("RL_SESSION")
            .env_remove("RL_SESSION_AGENT")
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }

    /// What the stand-in daemon was sent, in order.
    fn sent(&self) -> Vec<Value> {
        std::fs::read_to_string(self.codex().with_extension("log"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn held(&self) -> usize {
        std::fs::read_dir(self.home().join("sessions")).map_or(0, |d| d.count())
    }
}

fn methods(sent: &[Value]) -> Vec<&str> {
    sent.iter()
        .filter_map(|m| m["method"].as_str().or(m["run"].as_str()))
        .collect()
}

#[test]
fn rl_new_starts_codex_on_its_daemon_and_prints_the_thread() {
    let env = Env::new();
    let proj = env.dir("proj");
    let (ok, out, err) = env.rl(&proj, &["new", "--harness", "codex", "--", "fix", "the", "login"]);
    assert!(ok, "{err}");
    assert_eq!(out, "019f0000-0000-7000-8000-000000000001", "the thread's id");
    assert!(
        err.contains("codex resume 019f0000-0000-7000-8000-000000000001"),
        "{err}"
    );
    let sent = env.sent();
    assert_eq!(
        methods(&sent),
        ["initialize", "initialized", "thread/start", "turn/start"]
    );
    assert_eq!(sent[0]["params"]["clientInfo"]["name"], "relay");
    assert_eq!(sent[2]["params"]["cwd"], proj.to_str().unwrap(), "the folder only");
    assert_eq!(sent[3]["params"]["input"][0]["text"], "fix the login");
    assert_eq!(env.held(), 0, "no holder");

    // Named by what it runs, the same.
    let (ok, out, err) = env.rl(&proj, &["new", "--name-only", "--", "codex", "again"]);
    assert!(ok, "{err}");
    assert_eq!(out, "019f0000-0000-7000-8000-000000000002");
    assert!(err.is_empty(), "{err}");
    assert_eq!(env.held(), 0);
}

#[test]
fn with_no_daemon_running_rl_new_starts_one_first() {
    let env = Env::new();
    let proj = env.dir("proj");
    std::fs::write(env.codex().with_extension("down"), "").unwrap();
    let (ok, out, err) = env.rl(&proj, &["new", "--harness", "codex", "--", "hello"]);
    assert!(ok, "{err}");
    assert_eq!(out, "019f0000-0000-7000-8000-000000000001");
    assert_eq!(
        methods(&env.sent()),
        [
            "daemon start",
            "initialize",
            "initialized",
            "thread/start",
            "turn/start"
        ]
    );
}

#[test]
fn rl_new_codex_with_no_prompt_runs_codex_here_and_flags_are_refused() {
    let env = Env::new();
    let proj = env.dir("proj");
    let (ok, out, err) = env.rl(&proj, &["new", "--harness", "codex"]);
    assert!(ok, "{err}");
    assert_eq!(out, "", "nothing started in the background to name");
    assert_eq!(
        std::fs::read_to_string(env.codex().with_extension("argv")).unwrap(),
        "",
        "codex, bare"
    );
    assert_eq!(
        std::fs::read_to_string(env.codex().with_extension("cwd"))
            .unwrap()
            .trim(),
        proj.to_str().unwrap(),
        "in the folder"
    );
    assert!(env.sent().is_empty(), "the daemon is not asked");
    assert_eq!(env.held(), 0);

    let (ok, _, err) = env.rl(&proj, &["new", "--harness", "codex", "--", "--model", "x"]);
    assert!(!ok);
    assert!(err.contains("`--model` can't go"), "{err}");
}
