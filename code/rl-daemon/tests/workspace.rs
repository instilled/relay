//! Workspaces against a real daemon: `rl up` registers a folder and loads its
//! `.relay/schedules.yaml`; nothing fires until approved; an edit asks again;
//! runs started inside the folder belong to it; unregistering stops it all.

mod common;

use common::{Conn, Harness};
use relay_core::Home;
use relay_proto::schedule::{Schedule, UpReport, Workspace};
use relay_proto::types::Run;
use serde_json::json;
use std::path::Path;
use std::time::Duration;

fn write_ws(dir: &Path, yaml: &str) {
    std::fs::create_dir_all(dir.join(".relay")).unwrap();
    std::fs::write(dir.join(".relay/schedules.yaml"), yaml).unwrap();
}

async fn runs_of(c: &mut Conn, name: &str) -> Vec<Run> {
    c.ok("run.list", json!({ "cause": format!("schedule:{name}"), "limit": 50 }))
        .await
}

#[tokio::test]
async fn a_workspace_schedule_fires_only_once_approved() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let ws = tmp.path().join("proj");
    std::fs::create_dir_all(ws.join("sub")).unwrap();
    let ws = std::fs::canonicalize(&ws).unwrap();
    write_ws(
        &ws,
        "schedules:\n  tick:\n    every: 1s\n    run: [sh, -c, 'pwd > seen.txt']\n",
    );
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    // Registered and loaded, not approved: listed, never fires.
    let r: UpReport = c.ok("workspace.up", json!({ "path": ws })).await;
    assert_eq!(r.workspace.name, "proj");
    assert_eq!(r.workspace.path, ws);
    assert_eq!(r.report.added, ["tick"]);
    assert_eq!(r.schedules.len(), 1);
    assert!(!r.schedules[0].approved());
    assert!(r.approved.is_empty());
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(runs_of(&mut c, "tick").await.is_empty(), "fired without approval");
    let res = c.call("schedule.run", json!({ "name": "tick" })).await;
    assert!(res.error.is_some(), "run-now needs approval too");
    let list: Vec<Schedule> = c.ok("schedule.list", json!({})).await;
    assert_eq!(list[0].scope, ws.to_string_lossy());
    assert_eq!(list[0].scope_label(), "proj");
    assert_eq!(list[0].next_at, None);

    // Approved: fires, in the folder, and the run belongs to the workspace.
    let r: UpReport = c.ok("workspace.up", json!({ "path": ws, "approve": true })).await;
    assert_eq!(r.approved, ["tick"]);
    assert!(r.schedules[0].approved());
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let runs = runs_of(&mut c, "tick").await;
    assert!(!runs.is_empty(), "did not fire after approval");
    assert_eq!(runs[0].workspace, ws);
    assert_eq!(runs[0].cwd, ws);
    let seen = std::fs::read_to_string(ws.join("seen.txt")).unwrap();
    assert_eq!(seen.trim(), ws.to_string_lossy());

    // Any run started inside the folder is the workspace's, git or not.
    let manual: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["true"], "cwd": ws.join("sub"), "title": "inside" }),
        )
        .await;
    assert_eq!(manual.workspace, ws);
    c.wait_terminal(&manual.id).await;

    // An edit is a new version: it waits for approval again.
    write_ws(
        &ws,
        "schedules:\n  tick:\n    every: 2s\n    run: [sh, -c, 'pwd > seen.txt']\n",
    );
    let r: UpReport = c.ok("workspace.up", json!({ "path": ws })).await;
    assert_eq!(r.report.changed, ["tick"]);
    assert!(!r.schedules[0].approved());
    assert_eq!(
        r.schedules[0].approved_hash,
        Some(list[0].hash.clone()),
        "the old approval is remembered"
    );
    let before = runs_of(&mut c, "tick").await.len();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(runs_of(&mut c, "tick").await.len(), before, "a changed version fired");

    // Unregistered: gone from the lists, marked removed.
    let ws_list: Vec<Workspace> = c.ok("workspace.list", json!(null)).await;
    assert_eq!(ws_list.len(), 1);
    let gone: bool = c.ok("workspace.remove", json!({ "path": ws })).await;
    assert!(gone);
    let ws_list: Vec<Workspace> = c.ok("workspace.list", json!(null)).await;
    assert!(ws_list.is_empty());
    let list: Vec<Schedule> = c.ok("schedule.list", json!({})).await;
    assert!(list.is_empty());
    let all: Vec<Schedule> = c.ok("schedule.list", json!({ "all": true })).await;
    assert!(all[0].removed_at.is_some());

    for r in runs_of(&mut c, "tick").await {
        c.wait_terminal(&r.id).await;
    }
    h.stop().await;
}

#[tokio::test]
async fn the_same_name_in_two_scopes_needs_saying_which() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let ws = tmp.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let ws = std::fs::canonicalize(&ws).unwrap();
    std::fs::write(
        home.schedules_file(),
        format!(
            "schedules:\n  nightly:\n    every: 1h\n    run: [true]\n    cwd: {}\n",
            ws.display()
        ),
    )
    .unwrap();
    write_ws(&ws, "schedules:\n  nightly:\n    every: 1h\n    run: [true]\n");
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    c.ok::<UpReport>("workspace.up", json!({ "path": ws, "approve": true }))
        .await;

    let res = c.call("schedule.pause", json!({ "name": "nightly" })).await;
    let msg = res.error.map(|e| e.message).unwrap_or_default();
    assert!(
        msg.contains("global") && msg.contains("proj") && msg.contains("--in"),
        "{msg}"
    );
    let s: Schedule = c
        .ok("schedule.pause", json!({ "name": "nightly", "scope": "global" }))
        .await;
    assert!(s.paused && s.scope == "global");
    let s: Schedule = c.ok("schedule.pause", json!({ "name": "nightly", "scope": ws })).await;
    assert!(s.paused && s.scope == ws.to_string_lossy());
    let res = c
        .call("schedule.pause", json!({ "name": "nightly", "scope": "/nowhere" }))
        .await;
    assert!(res.error.is_some());
    h.stop().await;
}
