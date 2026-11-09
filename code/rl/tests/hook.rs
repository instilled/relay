//! `rl hook <agent> <Event>` writes state files, with the hook's JSON on stdin.

use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Env {
    tmp: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("work")).unwrap();
        Self { tmp }
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("relay")
    }

    fn work(&self) -> PathBuf {
        self.tmp.path().join("work")
    }

    fn hook(&self, args: &[&str], payload: Value, env: &[(&str, &str)]) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rl"));
        cmd.arg("hook")
            .args(args)
            .env_remove("RL_SESSION")
            .env_remove("RL_SESSION_AGENT")
            .env_remove("RELAY_ASKING")
            .env("RELAY_HOME", self.home())
            .env("HOME", self.tmp.path())
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "a hook never fails");
        assert!(out.stdout.is_empty() && out.stderr.is_empty(), "a hook never prints");
        out
    }

    fn payload(&self, sid: &str) -> Value {
        json!({"session_id": sid, "cwd": self.work()})
    }

    fn state(&self, agent: &str, sid: &str) -> PathBuf {
        self.home().join("state").join(agent).join(format!("{sid}.json"))
    }
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn a_prompt_makes_the_session_busy() {
    let e = Env::new();
    e.hook(&["claude", "UserPromptSubmit", "--user"], e.payload("s1"), &[]);
    let v = read(&e.state("claude", "s1"));
    assert_eq!(v["state"], "busy");
    assert_eq!(v["agent"], "claude");
    assert_eq!(v["session_id"], "s1");
}

#[test]
fn a_permission_notification_waits_with_its_detail() {
    let e = Env::new();
    let mut p = e.payload("s1");
    p["notification_type"] = json!("permission_prompt");
    e.hook(&["claude", "Notification", "--user"], p, &[]);
    let v = read(&e.state("claude", "s1"));
    assert_eq!(v["state"], "waiting");
    assert!(v["detail"].as_str().is_some_and(|d| !d.is_empty()), "{v}");
}

#[test]
fn a_session_end_removes_the_file() {
    let e = Env::new();
    e.hook(&["claude", "UserPromptSubmit", "--user"], e.payload("s1"), &[]);
    assert!(e.state("claude", "s1").exists());
    e.hook(&["claude", "SessionEnd", "--user"], e.payload("s1"), &[]);
    assert!(!e.state("claude", "s1").exists());
}

#[test]
fn a_run_of_claudes_own_hook_outside_relay_says_nothing() {
    let e = Env::new();
    e.hook(&["claude", "UserPromptSubmit"], e.payload("s1"), &[]);
    assert!(!e.home().join("state").exists());
}

#[test]
fn under_a_held_session_the_word_goes_to_its_folder() {
    let e = Env::new();
    let dir = e.home().join("sessions/x");
    std::fs::create_dir_all(&dir).unwrap();
    e.hook(
        &["claude", "UserPromptSubmit"],
        e.payload("s1"),
        &[("RL_SESSION", "x"), ("RL_SESSION_AGENT", "claude")],
    );
    assert_eq!(read(&dir.join("hook.json"))["state"], "busy");
    assert!(!e.home().join("state").exists());
}

#[test]
fn an_agent_inside_a_held_shell_reports_as_its_own_session() {
    let e = Env::new();
    let dir = e.home().join("sessions/x");
    std::fs::create_dir_all(&dir).unwrap();
    // The held session runs a shell; the claude started in it is not the
    // session's: its `--user` copy tells, and its own copy says nothing.
    let held_shell = [("RL_SESSION", "x"), ("RL_SESSION_AGENT", "sh")];
    e.hook(&["claude", "UserPromptSubmit", "--user"], e.payload("s1"), &held_shell);
    assert_eq!(read(&e.state("claude", "s1"))["state"], "busy");
    assert!(!dir.join("hook.json").exists());
    e.hook(&["claude", "Stop"], e.payload("s2"), &held_shell);
    assert!(!e.state("claude", "s2").exists());
    assert!(!dir.join("hook.json").exists());
}

#[test]
fn a_held_session_that_moves_to_another_conversation_starts_afresh() {
    let e = Env::new();
    let dir = e.home().join("sessions/x");
    std::fs::create_dir_all(&dir).unwrap();
    let held = [("RL_SESSION", "x"), ("RL_SESSION_AGENT", "claude")];
    let mut p = e.payload("s1");
    p["transcript_path"] = json!("/t/s1.jsonl");
    e.hook(&["claude", "UserPromptSubmit"], p, &held);
    e.hook(&["claude", "SessionStart"], e.payload("s2"), &held);
    let v = read(&dir.join("hook.json"));
    assert_eq!(v["session_id"], "s2");
    assert_eq!(v["state"], "idle");
    assert!(v.get("transcript_path").is_none(), "{v}");
}

#[test]
fn under_a_held_session_the_user_copy_is_ignored() {
    let e = Env::new();
    let dir = e.home().join("sessions/x");
    std::fs::create_dir_all(&dir).unwrap();
    e.hook(
        &["claude", "UserPromptSubmit", "--user"],
        e.payload("s1"),
        &[("RL_SESSION", "x"), ("RL_SESSION_AGENT", "claude")],
    );
    assert!(!dir.join("hook.json").exists());
    assert!(!e.home().join("state").exists());
}

#[test]
fn a_fresh_ask_makes_the_hook_do_nothing() {
    let e = Env::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    e.hook(
        &["claude", "UserPromptSubmit", "--user"],
        e.payload("s1"),
        &[("RELAY_ASKING", &now)],
    );
    assert!(!e.home().join("state").exists());
}

#[test]
fn a_word_it_does_not_know_is_no_failure() {
    let e = Env::new();
    e.hook(&["--help"], json!({}), &[]);
    e.hook(&["nobody", "Stop"], e.payload("s1"), &[]);
    assert!(!e.home().join("state").exists());
}

#[test]
fn a_later_word_changes_the_state_and_keeps_the_rest() {
    let e = Env::new();
    let mut p = e.payload("s1");
    p["transcript_path"] = json!("/t/s1.jsonl");
    e.hook(&["claude", "UserPromptSubmit", "--user"], p, &[]);
    e.hook(&["claude", "Stop", "--user"], e.payload("s1"), &[]);
    let v = read(&e.state("claude", "s1"));
    assert_eq!(v["state"], "idle");
    assert_eq!(v["transcript_path"], "/t/s1.jsonl");
}

#[test]
fn a_hook_with_nothing_to_do_still_reads_the_whole_payload() {
    let e = Env::new();
    let mut p = e.payload("s1");
    // Bigger than a pipe holds: an unread one would fail the agent's write.
    p["blob"] = json!("x".repeat(512 * 1024));
    e.hook(&["nobody", "Stop"], p.clone(), &[]);
    e.hook(&["claude", "Stop"], p, &[]);
}
