//! `rl hook codex|gemini <Event>`, and Claude's `rl hook claude <Event>
//! --user`, through the real binaries: outside any run it puts the agent's
//! own session on the digest, and takes it off when it ends; under a run of
//! that agent's it is the run's signal and tags the run (Claude's `--user`
//! copy says nothing there, the run's own hook having said it), and no
//! session of the agent's shows. No real agent runs: the hooks are called as
//! the agents would call them, JSON on stdin.

use rl_cli::client::Client;
use rl_core::Home;
use rl_proto::methods::{Digest, DigestGet, DigestParams, Outside};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

fn rl(home: &Path, rld: &Path, session: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_rl"));
    c.args(args)
        .current_dir(home)
        .env("RELAY_HOME", home)
        .env("RELAY_RLD_BIN", rld)
        .env("RELAY_SESSION_BIN", session)
        // Not the real agents' homes, whatever this machine has.
        .env("HOME", home)
        .env("CODEX_HOME", home.join("codex-home"))
        .env("GEMINI_CLI_HOME", home)
        .env_remove("RELAY_RUN")
        .env_remove("RELAY_REF")
        .env_remove("RELAY_TAGS")
        .env_remove("GEMINI_SESSION_ID")
        .env_remove("GEMINI_CWD");
    c
}

/// Runs `rl hook <words>` with `stdin`: whether it exited 0, and its stdout.
fn hook(mut c: Command, stdin: &str) -> (bool, String, Duration) {
    let started = Instant::now();
    let mut child = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        started.elapsed(),
    )
}

async fn outside(home: &Path) -> Vec<Outside> {
    let d: Digest = Client::connect(&Home::at(home))
        .await
        .unwrap()
        .call::<DigestGet>(DigestParams::default())
        .await
        .unwrap();
    d.outside
}

#[tokio::test]
async fn an_agents_own_session_is_told_by_its_hook_and_a_runs_is_not() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let app = home.join("app");
    std::fs::create_dir_all(&app).unwrap();
    let cmd = |args: &[&str]| rl(&home, &rld, &session, args);
    let payload = |sid: &str, event: &str| {
        format!(
            r#"{{"session_id":"{sid}","cwd":"{}","hook_event_name":"{event}","transcript_path":null,"tool_name":"shell","model":"m"}}"#,
            app.display()
        )
    };

    // No daemon: nothing, quickly, and no daemon started.
    let (ok, out, took) = hook(
        cmd(&["hook", "codex", "PermissionRequest"]),
        &payload("c1", "PermissionRequest"),
    );
    assert!(
        ok && out.is_empty() && took < Duration::from_secs(2),
        "{out:?} {took:?}"
    );
    assert!(Client::connect(&Home::at(&home)).await.is_err(), "no daemon started");
    // An agent that never closes stdin is not waited on past the hook's time.
    let mut held = cmd(&["hook", "gemini", "AfterAgent"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let open = held.stdin.take();
    let started = Instant::now();
    let status = held.wait().unwrap();
    assert!(status.success() && started.elapsed() < Duration::from_secs(3));
    drop(open);

    assert!(cmd(&["daemon", "start"]).status().unwrap().success());

    // Outside any run: the session is on the digest, wanting you.
    let (ok, out, _) = hook(
        cmd(&["hook", "codex", "PermissionRequest"]),
        &payload("c1", "PermissionRequest"),
    );
    assert!(ok && out.is_empty(), "{out:?}");
    let list = outside(&home).await;
    let c1 = list.iter().find(|s| s.key() == "o:codex:c1").expect("told");
    assert!(c1.wants_you() && c1.in_a_terminal(), "{c1:?}");
    assert_eq!((c1.cwd.as_path(), c1.kind.as_str()), (app.as_path(), "interactive"));
    assert_eq!(c1.waiting_for.as_deref(), Some("permission prompt"));

    // Gemini's too, its words its own.
    let (ok, _, _) = hook(
        cmd(&["hook", "gemini", "Notification"]),
        &format!(
            r#"{{"session_id":"g1","cwd":"{}","hook_event_name":"Notification","notification_type":"ToolPermission","message":"Allow?"}}"#,
            app.display()
        ),
    );
    assert!(ok);
    assert!(
        outside(&home)
            .await
            .iter()
            .any(|s| s.key() == "o:gemini:g1" && s.wants_you())
    );

    // Its end takes it off.
    let (ok, out, _) = hook(cmd(&["hook", "codex", "SessionEnd"]), &payload("c1", "SessionEnd"));
    assert!(ok && out.is_empty());
    assert!(!outside(&home).await.iter().any(|s| s.key() == "o:codex:c1"));

    // Garbage: no session, a stdin that is not JSON, a word too many.
    for (words, stdin) in [
        (vec!["hook", "codex", "Stop"], "not json"),
        (vec!["hook", "codex", "Stop"], r#"{"cwd":"/w"}"#),
        (vec!["hook", "codex", "Stop", "extra"], "{}"),
        (vec!["hook", "claude", "Stop"], r#"{"session_id":"x","cwd":"/w"}"#),
    ] {
        let (ok, out, _) = hook(cmd(&words), stdin);
        assert!(ok && out.is_empty(), "{words:?}");
    }
    let keys: Vec<String> = outside(&home).await.iter().map(Outside::key).collect();
    assert_eq!(keys, ["o:gemini:g1"], "none of it told of a session");

    // A codex run of Relay's (the stand-in, called `codex`): its hooks are
    // its signals, the run is tagged, and no session of codex's shows.
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-cli");
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&fake, bin.join("codex")).unwrap();
    let start = |argv0: &Path| {
        let out = cmd(&["run", "--agent", argv0.to_str().unwrap()]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let under = |id: &str, sid: &str, event: &str| {
        let mut c = cmd(&["hook", "codex", event]);
        c.env("RELAY_RUN", id);
        let (ok, out, _) = hook(c, &payload(sid, event));
        assert!(ok && out.is_empty());
    };
    let run_of = |id: &str| {
        let ls = cmd(&["ls", "--all", "--json"]).output().unwrap();
        let runs: serde_json::Value = serde_json::from_slice(&ls.stdout).unwrap();
        runs.as_array().unwrap().iter().find(|r| r["id"] == id).unwrap().clone()
    };
    let id = start(&bin.join("codex"));
    assert_eq!(run_of(&id)["cli"], "codex");
    under(&id, "r1", "SessionStart");
    under(&id, "r1", "PermissionRequest");
    let run = run_of(&id);
    assert!(
        run["tags"].as_array().unwrap().iter().any(|t| t == "codex-session:r1"),
        "{run}"
    );
    assert_eq!(
        (run["state"].as_str(), run["source"].as_str()),
        (Some("needs_input"), Some("hook"))
    );
    // Approved, and the tool ran: working again.
    under(&id, "r1", "PostToolUse");
    assert_eq!(run_of(&id)["state"], "working");
    // The turn over, then a tool's word late (codex's background hooks):
    // the run stays idle.
    under(&id, "r1", "Stop");
    under(&id, "r1", "PostToolUse");
    assert_eq!(run_of(&id)["state"], "idle");
    assert!(!outside(&home).await.iter().any(|s| s.session_id == "r1"));

    // An agent run of a CLI Relay does not know (a wrapper, `npx
    // @openai/codex`): codex's hooks are its own, and it is not shown twice.
    let wrapper = start(&fake);
    under(&wrapper, "w1", "SessionStart");
    under(&wrapper, "w1", "PermissionRequest");
    let w = run_of(&wrapper);
    assert!(
        w["tags"].as_array().unwrap().iter().any(|t| t == "codex-session:w1"),
        "{w}"
    );
    assert_eq!(w["state"], "needs_input");
    assert!(!outside(&home).await.iter().any(|s| s.session_id == "w1"));

    // A codex started inside a run of another kind — a command's, a Claude
    // run's: that run is left as it was, and the session is codex's own.
    let out = cmd(&["run", "--", fake.to_str().unwrap()]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let command = String::from_utf8_lossy(&out.stdout).trim().to_string();
    std::os::unix::fs::symlink(&fake, bin.join("claude")).unwrap();
    let claude = start(&bin.join("claude"));
    assert_eq!(run_of(&claude)["cli"], "claude");
    for (other, sid) in [(&command, "n1"), (&claude, "n2")] {
        let before = run_of(other);
        under(other, sid, "SessionStart");
        under(other, sid, "PermissionRequest");
        let after = run_of(other);
        assert_eq!(
            (&after["state"], &after["tags"], &after["title"]),
            (&before["state"], &before["tags"], &before["title"]),
            "{sid}"
        );
        let nested = outside(&home).await;
        assert!(
            nested.iter().any(|s| s.session_id == sid && s.wants_you()),
            "{sid}: {nested:?}"
        );
    }

    for id in [&id, &wrapper, &command, &claude] {
        assert!(cmd(&["stop", id]).status().unwrap().success());
    }
    assert!(cmd(&["daemon", "stop"]).status().unwrap().success());
}

#[tokio::test]
async fn claudes_user_level_hook_tells_of_its_own_sessions_and_never_a_claude_runs() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let app = home.join("app");
    std::fs::create_dir_all(&app).unwrap();
    let cmd = |args: &[&str]| {
        let mut c = rl(&home, &rld, &session, args);
        c.env("CLAUDE_CONFIG_DIR", home.join("claude-config"));
        c
    };
    let payload = |sid: &str, event: &str| {
        format!(
            r#"{{"session_id":"{sid}","cwd":"{}","hook_event_name":"{event}","transcript_path":"/t/{sid}.jsonl"}}"#,
            app.display()
        )
    };
    let user = |run: Option<&str>, sid: &str, event: &str| {
        let mut c = cmd(&["hook", "claude", event, "--user"]);
        if let Some(id) = run {
            c.env("RELAY_RUN", id);
        }
        let (ok, out, took) = hook(c, &payload(sid, event));
        assert!(
            ok && out.is_empty() && took < Duration::from_secs(2),
            "{out:?} {took:?}"
        );
    };
    assert!(cmd(&["daemon", "start"]).status().unwrap().success());

    // Outside any run: an idle session of Claude's, until it ends.
    user(None, "c1", "Stop");
    let list = outside(&home).await;
    let c1 = list.iter().find(|s| s.key() == "o:claude:c1").expect("told");
    assert_eq!(
        (c1.status.as_str(), c1.kind.as_str(), c1.cwd.as_path()),
        ("idle", "interactive", app.as_path())
    );
    user(None, "c1", "SessionEnd");
    assert!(outside(&home).await.is_empty());

    // Under a Claude run of Relay's (the stand-in, called `claude`): its
    // own hook is its signal; the user-level copy changes nothing.
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-cli");
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&fake, bin.join("claude")).unwrap();
    let start = |args: &[&str]| {
        let out = cmd(args).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let run_of = |id: &str| {
        let ls = cmd(&["ls", "--all", "--json"]).output().unwrap();
        let runs: serde_json::Value = serde_json::from_slice(&ls.stdout).unwrap();
        runs.as_array().unwrap().iter().find(|r| r["id"] == id).unwrap().clone()
    };
    let claude = start(&["run", "--agent", bin.join("claude").to_str().unwrap()]);
    assert_eq!(run_of(&claude)["cli"], "claude");
    let before = run_of(&claude);
    for event in ["SessionStart", "UserPromptSubmit", "Stop"] {
        user(Some(&claude), "r1", event);
    }
    let after = run_of(&claude);
    assert_eq!(
        (&after["state"], &after["tags"], &after["title"]),
        (&before["state"], &before["tags"], &before["title"])
    );
    assert!(outside(&home).await.is_empty(), "the run's own, not Claude's");

    // A claude started in a command run of Relay's (a shell's is the same
    // to it): one of Claude's own.
    let command = start(&["run", "--", "sh", "-c", "sleep 30"]);
    let before = run_of(&command);
    user(Some(&command), "n1", "UserPromptSubmit");
    let nested = outside(&home).await;
    assert!(
        nested.iter().any(|s| s.key() == "o:claude:n1" && s.status == "busy"),
        "{nested:?}"
    );
    assert_eq!(run_of(&command)["state"], before["state"]);

    for id in [&claude, &command] {
        assert!(cmd(&["stop", id]).status().unwrap().success());
    }
    assert!(cmd(&["daemon", "stop"]).status().unwrap().success());
}
