//! `import.list` / `import.move`: Claude's own sessions, and moving one
//! into Relay — its background process stopped, the conversation resumed as
//! a Relay run in a folder that becomes a workspace. The `claude` here is the
//! fixture player: no real agent, no model.

mod common;

use common::{Conn, Harness, session_bin};
use rl_core::Home;
use rl_proto::methods::ImportReport;
use rl_proto::types::Run;
use serde_json::json;

fn sessions_file(dir: &std::path::Path, cwd: &str) -> std::path::PathBuf {
    let file = dir.join("agents.json");
    std::fs::write(
        &file,
        serde_json::to_string(&json!([
            {"pid": 1, "id": "a1", "cwd": cwd, "kind": "background", "sessionId": "sid-one",
             "name": "asset list", "status": "idle", "state": "done"},
            {"pid": 2, "cwd": cwd, "kind": "interactive", "sessionId": "sid-two",
             "name": "ground work", "status": "busy"}
        ]))
        .unwrap(),
    )
    .unwrap();
    file
}

#[tokio::test]
async fn a_claude_session_moves_into_relay() {
    let Some(_) = session_bin() else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let fixtures = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
    let fixtures = std::fs::canonicalize(fixtures).unwrap();
    let old = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![fixtures.clone()];
    paths.extend(std::env::split_paths(&old));
    let tmp = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(tmp.path()).unwrap();
    let agents = sessions_file(tmp.path(), &cwd.to_string_lossy());
    let stopped = tmp.path().join("stopped");
    // SAFETY: this test binary is the only thing in this process.
    unsafe {
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        std::env::set_var("CLAUDE_PLAYER_AGENTS", &agents);
        std::env::set_var("CLAUDE_PLAYER_STOPPED", &stopped);
        std::env::set_var("CLAUDE_PLAYER_SPEED", "0");
    }

    let home = Home::at(tmp.path().join("home"));
    std::fs::create_dir_all(home.path()).unwrap();
    let h = Harness::on(home.clone()).unwrap();
    let mut c = Conn::open(&home).await;

    let r: ImportReport = c.ok("import.list", json!({})).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.sessions.len(), 2);
    let one = &r.sessions[0];
    assert_eq!((one.name.as_str(), one.id.as_deref()), ("asset list", Some("a1")));
    assert!(one.new && one.run.is_none() && !one.in_a_terminal());
    assert!(r.sessions[1].in_a_terminal(), "the one open in a terminal");

    // The one in a terminal is Claude's to keep.
    let e = c
        .call("import.move", json!({ "session_id": "sid-two" }))
        .await
        .error
        .expect("refused");
    assert!(e.message.contains("open in a terminal"), "{}", e.message);

    // The background one moves: stopped there, resumed here.
    let run: Run = c.ok("import.move", json!({ "session_id": "sid-one" })).await;
    assert_eq!(run.title, "asset list");
    assert!(run.argv.iter().any(|a| a == "--resume") && run.argv.iter().any(|a| a == "sid-one"));
    assert!(run.tags.iter().any(|t| t == "claude-session:sid-one"), "{:?}", run.tags);
    assert_eq!(run.cwd, cwd);
    assert_eq!(std::fs::read_to_string(&stopped).unwrap().trim(), "a1");

    // Claude no longer lists it; the run that carries it on is named.
    std::fs::write(
        &agents,
        serde_json::to_string(&json!([
            {"pid": 1, "id": "a1", "cwd": cwd.to_string_lossy(), "kind": "background", "sessionId": "sid-one",
             "name": "asset list", "status": "idle", "state": "done"}
        ]))
        .unwrap(),
    )
    .unwrap();
    let r: ImportReport = c.ok("import.list", json!({ "mark": true })).await;
    assert_eq!(r.sessions[0].run.as_deref(), Some(run.id.as_str()));
    assert!(!r.sessions[0].new, "one already here is not news");

    // Moving it again is refused, not done twice.
    let e = c
        .call("import.move", json!({ "session_id": "sid-one" }))
        .await
        .error
        .expect("refused");
    assert!(e.message.contains("in Relay already"), "{}", e.message);

    let _ = c.call("run.stop", json!({ "id": run.id })).await;
    drop(h);
}
