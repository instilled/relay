//! `rl hook` and `rl mark` through the real binaries: a hook with `RELAY_RUN`
//! in its environment moves the run; without it, or without a daemon, it
//! exits 0 and says nothing.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

#[test]
fn hooks_and_marks_move_the_run() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let base = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rl"));
        c.args(args)
            .current_dir(tmp.path())
            .env("RELAY_HOME", tmp.path())
            .env("RELAY_RLD_BIN", &rld)
            .env("RELAY_SESSION_BIN", &session)
            .env_remove("RELAY_RUN")
            .env_remove("RELAY_REF")
            .env_remove("RELAY_TAGS");
        c
    };
    let run = |args: &[&str]| {
        let out = base(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let hook = |event: &str, run_id: Option<&str>, stdin: &str| {
        let mut c = base(&["hook", event]);
        if let Some(id) = run_id {
            c.env("RELAY_RUN", id);
        }
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string())
    };
    let state = |id: &str| {
        let (_, out, _) = run(&["ls", "--all", "--json"]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let me = v.as_array().unwrap().iter().find(|r| r["id"] == id).unwrap().clone();
        (
            me["state"].as_str().unwrap().to_string(),
            me["source"].as_str().unwrap().to_string(),
        )
    };

    // No daemon, no run id: a hook is a silent no-op either way.
    assert_eq!(hook("Stop", None, "{}"), (true, String::new()));
    assert_eq!(hook("Stop", Some("zzzzzzzz"), "{}"), (true, String::new()));

    assert!(run(&["daemon", "start"]).0);
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-cli");
    let (ok, out, err) = run(&["run", "--agent", fake.to_str().unwrap()]);
    assert!(ok, "{err}");
    let id = out.trim().to_string();

    let payload = r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Edit"}"#;
    assert_eq!(hook("Notification", Some(&id), payload), (true, String::new()));
    assert_eq!(state(&id), ("needs_input".into(), "hook".into()));
    let (_, out, _) = run(&["ls"]);
    assert!(out.contains("needs_input  agent"), "exact states carry no mark: {out}");
    let (_, out, _) = run(&["events", "--run", &id, "--kind", "run.needs_input"]);
    assert!(
        out.contains("needs input: Claude needs your permission to use Edit"),
        "{out}"
    );

    // Garbage on stdin is still a signal-less no-op, not a failure.
    assert_eq!(hook("Notification", Some(&id), "not json"), (true, String::new()));
    assert_eq!(state(&id).0, "needs_input");

    let (ok, out, err) = run(&["mark", &id, "done", "--note", "reviewed"]);
    assert!(ok, "{err}");
    assert_eq!(out.trim(), format!("{id} · idle"));
    assert_eq!(state(&id), ("idle".into(), "manual".into()));
    let (ok, _, err) = run(&["mark", &id, "later"]);
    assert!(!ok && err.contains("invalid value 'later'"), "{err}");
    let (ok, _, err) = run(&["mark", "nope0000", "blocked"]);
    assert!(!ok && err.contains("not found"), "{err}");

    assert!(run(&["stop", &id]).0);
    assert!(run(&["daemon", "stop"]).0);
}
