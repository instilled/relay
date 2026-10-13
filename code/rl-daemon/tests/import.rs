//! `import.list` / `import.move`: agents' own sessions, and moving one into
//! Relay — a Claude background process stopped, an idle one in a terminal
//! left there, the conversation resumed as a Relay run in a folder that
//! becomes a workspace. The `claude` here is the fixture player and the
//! `codex` a script that waits: no real agent, no model.

mod common;

use common::{Conn, Harness, session_bin};
use rl_core::Home;
use rl_proto::methods::{Digest, ImportReport};
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
             "name": "ground work", "status": "busy"},
            {"pid": 3, "cwd": cwd, "kind": "interactive", "sessionId": "sid-three",
             "name": "quiet one", "status": "idle"}
        ]))
        .unwrap(),
    )
    .unwrap();
    file
}

#[tokio::test]
async fn agents_sessions_move_into_relay() {
    let Some(_) = session_bin() else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let fixtures = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures");
    let fixtures = std::fs::canonicalize(fixtures).unwrap();
    let old = std::env::var_os("PATH").unwrap_or_default();
    let tmp = tempfile::tempdir().unwrap();
    // A codex that only waits: what Relay would start to carry its session on.
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexec sleep 30\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut paths = vec![fixtures.clone(), bin];
    paths.extend(std::env::split_paths(&old));
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
    assert_eq!(r.sessions.len(), 3);
    let one = &r.sessions[0];
    assert_eq!((one.name.as_str(), one.id.as_deref()), ("asset list", Some("a1")));
    assert!(one.new && one.run.is_none() && !one.in_a_terminal());
    assert!(r.sessions[1].in_a_terminal(), "the one open in a terminal");

    // One at work in its terminal is left to it, with the way to bring it.
    let e = c
        .call("import.move", json!({ "session_id": "sid-two" }))
        .await
        .error
        .expect("refused");
    assert!(
        e.message
            .contains("ground work is working in its terminal; /bg there to open it here"),
        "{}",
        e.message
    );

    // An idle one in a terminal carries on here, nothing stopped: that
    // terminal keeps its own.
    let quiet: Run = c
        .ok(
            "import.move",
            json!({ "agent": "claude", "session_id": "sid-three", "cols": 100, "rows": 30 }),
        )
        .await;
    assert_eq!(quiet.title, "quiet one");
    assert_eq!(&quiet.argv[quiet.argv.len() - 2..], ["--resume", "sid-three"]);
    assert!(
        quiet.tags.iter().any(|t| t == "claude-session:sid-three"),
        "{:?}",
        quiet.tags
    );
    assert!(!stopped.exists(), "nothing of Claude's stopped");
    let _ = c.call("run.stop", json!({ "id": quiet.id })).await;

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

    // That run sent to Claude's background (`/background`): it ends here,
    // and the conversation goes on there, started after the run. It is
    // Claude's again, and moves back.
    c.wait_terminal(&run.id).await;
    let later = jiff::Timestamp::now().as_millisecond() + 1_000;
    let went_on = serde_json::to_string(&json!([
        {"pid": 1, "id": "a2", "cwd": cwd.to_string_lossy(), "kind": "background", "sessionId": "sid-one",
         "name": "asset list", "status": "busy", "state": "working", "startedAt": later}
    ]))
    .unwrap();
    std::fs::write(&agents, &went_on).unwrap();
    let r: ImportReport = c.ok("import.list", json!({})).await;
    let went = &r.sessions[0];
    assert_eq!(went.run, None, "the run it went on from has ended");
    assert_eq!(went.was.as_ref().map(|w| w.run.as_str()), Some(run.id.as_str()));
    assert!(!went.new, "Relay's own conversation is not news");
    let back: Run = c.ok("import.move", json!({ "session_id": "sid-one" })).await;
    assert_ne!(back.id, run.id);
    assert!(
        back.tags.iter().any(|t| t == "claude-session:sid-one"),
        "{:?}",
        back.tags
    );
    assert_eq!(std::fs::read_to_string(&stopped).unwrap().lines().last(), Some("a2"));
    // Listed once more as it was, the newest run carrying it is live.
    std::fs::write(&agents, &went_on).unwrap();
    let r: ImportReport = c.ok("import.list", json!({})).await;
    assert_eq!(
        (r.sessions[0].run.as_deref(), r.sessions[0].was.as_ref()),
        (Some(back.id.as_str()), None),
        "carried on again: Relay's"
    );
    let _ = c.call("run.stop", json!({ "id": back.id })).await;

    // Codex's, as its hook told: carried on as codex resumes, in its folder,
    // once it is not at work and has been quiet a moment.
    let transcript = tmp.path().join("cx-1.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    let told = |event: &str| {
        json!({ "agent": "codex", "session_id": "cx-1", "cwd": cwd, "event": event,
                "transcript_path": transcript })
    };
    let _: serde_json::Value = c.ok("outside.hook", told("UserPromptSubmit")).await;
    let e = c
        .call("import.move", json!({ "agent": "codex", "session_id": "cx-1" }))
        .await
        .error
        .expect("refused");
    assert!(
        e.message.contains("wait for it to finish or close it there"),
        "{}",
        e.message
    );
    let _: serde_json::Value = c.ok("outside.hook", told("Stop")).await;
    let just = |e: Option<rl_proto::rpc::RpcError>| {
        let e = e.expect("refused");
        assert!(e.message.contains("just moved in its terminal"), "{}", e.message);
    };
    // Its word is too fresh: another may be on its way.
    just(
        c.call("import.move", json!({ "agent": "codex", "session_id": "cx-1" }))
            .await
            .error,
    );
    tokio::time::sleep(rl_daemon::import::HEARD_QUIET + std::time::Duration::from_millis(200)).await;
    // Its conversation was written just now: it is at work, whatever it said.
    just(
        c.call("import.move", json!({ "agent": "codex", "session_id": "cx-1" }))
            .await
            .error,
    );
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(&transcript)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    // Two asks at once (two panels, say): one run, not two on one
    // conversation.
    let mut c2 = Conn::open(&home).await;
    let ask = json!({ "agent": "codex", "session_id": "cx-1" });
    let (one, two) = tokio::join!(c.call("import.move", ask.clone()), c2.call("import.move", ask));
    let mut started: Vec<Run> = vec![];
    for r in [one, two] {
        match r.error {
            Some(e) => assert!(
                e.message.contains("being carried on already") || e.message.contains("in Relay already"),
                "{}",
                e.message
            ),
            None => started.push(serde_json::from_value(r.result.unwrap()).unwrap()),
        }
    }
    assert_eq!(started.len(), 1, "{started:?}");
    let cx = started.remove(0);
    assert_eq!(cx.argv, [codex.to_string_lossy().as_ref(), "resume", "cx-1"]);
    assert!(cx.tags.iter().any(|t| t == "codex-session:cx-1"), "{:?}", cx.tags);
    assert_eq!(cx.cwd, cwd);

    // Left open in its terminal: off the list while idle there, back on it,
    // with the run that carries it, once it works there again.
    let listed = |d: &Digest| d.outside.iter().find(|s| s.session_id == "cx-1").cloned();
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert!(listed(&d).is_none(), "{:?}", d.outside);
    let _: serde_json::Value = c.ok("outside.hook", told("UserPromptSubmit")).await;
    let d: Digest = c.ok("digest.get", json!({})).await;
    let back = listed(&d).expect("at work in its terminal again");
    assert_eq!(back.run.as_deref(), Some(cx.id.as_str()));
    let e = c
        .call("import.move", json!({ "agent": "codex", "session_id": "cx-1" }))
        .await
        .error
        .expect("refused");
    assert!(e.message.contains("in Relay already"), "{}", e.message);
    let e = c
        .call("import.move", json!({ "agent": "gemini", "session_id": "nope" }))
        .await
        .error
        .expect("refused");
    assert!(e.message.contains("gemini has no session nope"), "{}", e.message);
    let _ = c.call("run.stop", json!({ "id": cx.id })).await;
    drop(h);
}
