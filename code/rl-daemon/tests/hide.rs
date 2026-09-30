//! `run.hide` and `digest.hide`: a run, and a whole project, taken off the
//! panel's list and brought back — never stopped, and kept across a restart.

mod common;

use common::{Conn, Harness};
use rl_core::Home;
use rl_proto::methods::{Digest, Hidden};
use rl_proto::rpc::RpcError;
use rl_proto::types::{Run, RunState};
use serde_json::json;

fn digest_run<'a>(d: &'a Digest, id: &str) -> &'a Run {
    &d.items.iter().find(|i| i.run.id == id).expect("on the digest").run
}

#[tokio::test]
async fn a_hidden_run_and_project_stay_hidden_across_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let (a, b) = (tmp.path().join("proj-a"), tmp.path().join("proj-b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let mut c = Conn::open(&h.home).await;

    // One that fails a moment after it is hidden, and one in a project.
    let failing: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["/bin/sh", "-c", "sleep 1; exit 4"], "cwd": a.to_string_lossy() }),
        )
        .await;
    let hi: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["/bin/sh", "-c", "echo hi"], "cwd": b.to_string_lossy() }),
        )
        .await;
    let hidden: Run = c.ok("run.hide", json!({ "id": failing.id, "hidden": true })).await;
    let since = hidden.hidden_at.expect("hidden, and since when");
    assert!(!hidden.state.is_terminal(), "hiding never stops it");
    let key = format!("g:{}", hi.workspace.display());
    let now: Vec<Hidden> = c.ok("digest.hide", json!({ "key": key, "hidden": true })).await;
    assert_eq!(now.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), [key.as_str()]);

    // It fails after it was hidden: that is what comes through to the list.
    let done = c.wait_terminal(&failing.id).await;
    assert_eq!(done.state, RunState::Failed);
    assert_eq!(done.hidden_at, Some(since), "ending leaves it hidden");
    assert!(done.state_at > since, "it came to need you after it was hidden");
    c.wait_terminal(&hi.id).await;

    // A restart keeps both.
    h.stop().await;
    let h = Harness::on(home.clone()).unwrap();
    let mut c = Conn::open(&h.home).await;
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert_eq!(digest_run(&d, &failing.id).hidden_at, Some(since));
    assert_eq!(
        digest_run(&d, &hi.id).hidden_at,
        None,
        "hidden with its project, not on its own"
    );
    assert_eq!(
        d.hidden.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(),
        [key.as_str()]
    );

    // Brought back, and a restart keeps that too.
    let back: Run = c.ok("run.hide", json!({ "id": failing.id, "hidden": false })).await;
    assert_eq!(back.hidden_at, None);
    let now: Vec<Hidden> = c.ok("digest.hide", json!({ "key": key, "hidden": false })).await;
    assert!(now.is_empty());
    h.stop().await;
    let h = Harness::on(home).unwrap();
    let mut c = Conn::open(&h.home).await;
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert_eq!(digest_run(&d, &failing.id).hidden_at, None);
    assert!(d.hidden.is_empty());

    // What cannot be hidden says so.
    let missing = c.call("run.hide", json!({ "id": "zzzzzzzz", "hidden": true })).await;
    assert_eq!(missing.error.unwrap().code, RpcError::NOT_FOUND);
    for bad in ["", "g:", "q:x", "/w/a"] {
        let res = c.call("digest.hide", json!({ "key": bad, "hidden": true })).await;
        assert_eq!(res.error.unwrap().code, RpcError::INVALID_PARAMS, "{bad:?}");
    }
    h.stop().await;
}
