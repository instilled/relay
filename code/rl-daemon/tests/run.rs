//! `run.start` / `run.stop` end to end: an in-process daemon spawning the real
//! `rl-session` binary, with the outcome read back through the socket and
//! the ledger.

mod common;

use common::{Conn, Harness, fake_cli, journal};
use rl_core::Home;
use rl_ledger::{Ledger, sessions};
use rl_proto::rpc::RpcError;
use rl_proto::types::{Event, Run, RunState};
use serde_json::json;
use std::time::Duration;

fn start() -> Option<(tempfile::TempDir, Harness)> {
    let tmp = tempfile::tempdir().unwrap();
    let h = Harness::on(Home::at(tmp.path()))?;
    Some((tmp, h))
}

#[tokio::test]
async fn command_run_records_output_exit_and_events() {
    let Some((tmp, h)) = start() else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();

    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "echo hello from the run; exit 3"], "cwd": cwd, "tags": ["e2e"], "ref": "gh:1" }),
        )
        .await;
    assert_eq!(run.id.len(), 8);
    assert_eq!(run.state, RunState::Working);
    assert_eq!(run.title, "sh -c echo hello from the run; exit 3");
    assert_eq!(run.group_key(), "gh:1");
    assert!(run.workspace.is_absolute());

    let done = c.wait_terminal(&run.id).await;
    assert_eq!(done.state, RunState::Failed);
    assert_eq!(done.exit_code, Some(3));
    assert!(done.ended_at.is_some());
    assert_eq!(done.last_line.as_deref(), Some("hello from the run"));

    let evs: Vec<Event> = c.ok("event.list", json!({ "run": run.id })).await;
    let kinds: Vec<&str> = evs.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["run.started", "run.failed"]);
    assert!(evs[1].summary.starts_with("failed after"), "{}", evs[1].summary);
    assert_eq!(evs[1].data.as_ref().unwrap()["exit_code"], 3);
    assert_eq!(evs[1].group_key, "gh:1");

    assert!(journal(&h.home, &run.id).contains("hello from the run"));
    let ledger = Ledger::open(&h.home.ledger()).unwrap();
    let id = run.id.clone();
    let sess = ledger.call_blocking(move |db| sessions::get(db, &id)).unwrap().unwrap();
    assert!(sess.closed_at.is_some(), "session should be closed once the run ended");
    assert!(sess.tty);

    // The holder was told to close; its socket goes away.
    for _ in 0..100 {
        if !sess.socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!sess.socket.exists(), "holder socket lingered");

    let bad = c
        .call("run.start", json!({ "argv": ["/nonexistent/nope"], "cwd": cwd }))
        .await;
    let err = bad.error.unwrap();
    assert_eq!(err.code, RpcError::INTERNAL);
    assert!(err.message.contains("rl-session failed to start"), "{}", err.message);

    h.stop().await;
}

#[tokio::test]
async fn stop_ends_in_stopped() {
    let Some((tmp, h)) = start() else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent" }),
        )
        .await;
    assert_eq!(run.cli.as_deref(), Some("fake-cli"));

    let listed: Vec<Run> = c.ok("run.list", json!({ "active_only": true })).await;
    assert_eq!(listed.len(), 1);

    let after: Run = c.ok("run.stop", json!({ "id": run.id })).await;
    assert!(!after.state.is_terminal(), "stop returns before the exit lands");
    let done = c.wait_terminal(&run.id).await;
    assert_eq!(done.state, RunState::Stopped);
    assert_eq!(done.exit_code, Some(143));

    let evs: Vec<Event> = c
        .ok("event.list", json!({ "run": run.id, "kinds": ["run.stopped"] }))
        .await;
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].data.as_ref().unwrap()["signal"], "SIGTERM");

    let again = c.call("run.stop", json!({ "id": run.id })).await;
    assert_eq!(again.error.unwrap().code, RpcError::NOT_FOUND);
    h.stop().await;
}

#[tokio::test]
async fn no_tty_run_captures_both_streams() {
    let Some((tmp, h)) = start() else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "echo out; echo err 1>&2"], "cwd": cwd, "tty": false, "title": "both" }),
        )
        .await;
    assert_eq!(run.title, "both");
    let done = c.wait_terminal(&run.id).await;
    assert_eq!((done.state, done.exit_code), (RunState::Finished, Some(0)));
    let out = journal(&h.home, &run.id);
    assert!(out.contains("out") && out.contains("err"), "{out:?}");
    h.stop().await;
}
