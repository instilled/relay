//! Workspaces against a real daemon: `rl up` registers a folder, a name it
//! can be given and runs started inside it belong to it; removing a project
//! takes its runs and questions with it, and its flows stop loading — never
//! anyone else's.

mod common;

use common::{Conn, Harness, fake_cli};
use rl_core::Home;
use rl_proto::decision::{Decision, DecisionStatus};
use rl_proto::flow::{Flow, NotLoaded};
use rl_proto::methods::{Digest, WorkspaceRemoved};
use rl_proto::types::Workspace;
use rl_proto::types::{Run, RunState};
use serde_json::json;
use std::path::Path;

#[tokio::test]
async fn a_folder_is_registered_named_and_its_runs_are_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let ws = tmp.path().join("proj");
    std::fs::create_dir_all(ws.join("sub")).unwrap();
    let ws = std::fs::canonicalize(&ws).unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    let w: Workspace = c.ok("workspace.up", json!({ "path": ws.join("sub/..") })).await;
    assert_eq!((w.name.as_str(), w.path.as_path()), ("proj", ws.as_path()));
    let again: Workspace = c.ok("workspace.up", json!({ "path": ws })).await;
    assert_eq!(again, w, "registered already, it is as it was");
    let res = c.call("workspace.up", json!({ "path": tmp.path().join("nope") })).await;
    assert!(res.error.is_some(), "a folder that is not there");

    // Any run started inside the folder is the workspace's, git or not.
    let manual: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["true"], "cwd": ws.join("sub"), "title": "inside" }),
        )
        .await;
    assert_eq!(manual.workspace, ws);
    c.wait_terminal(&manual.id).await;

    let named: Workspace = c
        .ok("workspace.rename", json!({ "path": ws, "name": "the project" }))
        .await;
    assert_eq!(named.name, "the project");
    let list: Vec<Workspace> = c.ok("workspace.list", json!(null)).await;
    assert_eq!(list, [named]);
    let res = c
        .call(
            "workspace.rename",
            json!({ "path": tmp.path().join("home"), "name": "x" }),
        )
        .await;
    assert!(res.error.is_some(), "never registered");
    h.stop().await;
}

async fn digest_has(c: &mut Conn, ws: &Path) -> Vec<Run> {
    let d: Digest = c.ok("digest.get", json!({})).await;
    d.items
        .into_iter()
        .map(|i| i.run)
        .filter(|r| r.workspace == ws)
        .collect()
}

/// A project's header lives as long as its runs are on the list. Removing it
/// stops everything still going there — a warm shell that shrugs off
/// SIGTERM, an agent — archives what has ended and dismisses what it asked,
/// so the header goes; a flow `in` it no longer loads. Another folder's
/// runs and questions are left be.
#[tokio::test]
async fn removing_a_project_stops_and_archives_its_runs_and_lets_its_questions_and_flows_go() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let ws = tmp.path().join("proj");
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let (ws, other) = (
        std::fs::canonicalize(&ws).unwrap(),
        std::fs::canonicalize(&other).unwrap(),
    );
    let dir = home.flow_dir("there");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("flow.md"),
        format!("---\nin: {}\nsteps:\n  a:\n    run: \"true\"\n---\n", ws.display()),
    )
    .unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    // Registering the folder is enough: the flows are read again.
    let _: Workspace = c.ok("workspace.up", json!({ "path": ws })).await;
    let flows: Vec<Flow> = c.ok("flow.list", json!({})).await;
    assert_eq!(flows.len(), 1, "it loads once the folder is known: {flows:?}");

    let shell: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["/bin/sh", "-i"], "kind": "shell", "cwd": ws, "tags": ["shell"] }),
        )
        .await;
    let agent: Run = c
        .ok("run.start", json!({ "argv": [fake_cli()], "cwd": ws, "kind": "agent" }))
        .await;
    let done: Run = c.ok("run.start", json!({ "argv": ["true"], "cwd": ws })).await;
    c.wait_terminal(&done.id).await;
    let elsewhere: Run = c
        .ok(
            "run.start",
            json!({ "argv": [fake_cli()], "cwd": other, "kind": "agent" }),
        )
        .await;
    let ended_elsewhere: Run = c.ok("run.start", json!({ "argv": ["true"], "cwd": other })).await;
    c.wait_terminal(&ended_elsewhere.id).await;
    c.wait_state(&shell.id, RunState::Working).await;
    let mine: Decision = c
        .ok(
            "decision.raise",
            json!({ "run": agent.id, "title": "ship it?", "options": [{ "key": "yes" }] }),
        )
        .await;
    let theirs: Decision = c
        .ok(
            "decision.raise",
            json!({ "run": elsewhere.id, "title": "ship that?", "options": [{ "key": "yes" }] }),
        )
        .await;
    assert_eq!(digest_has(&mut c, &ws).await.len(), 3);

    let r: WorkspaceRemoved = c
        .ok(
            "workspace.remove",
            json!({ "path": ws, "stop_runs": true, "archive_runs": true }),
        )
        .await;
    assert!(r.removed);
    assert_eq!(r.path, ws);
    assert_eq!((r.stopped, r.still_running), (2, 0), "{r:?}");
    assert_eq!(r.archived, 3, "the two stopped and the one that had ended");
    assert_eq!(r.dismissed, 1);
    for id in [&shell.id, &agent.id] {
        let run = c.get(id).await;
        assert_eq!(run.state, RunState::Stopped, "{run:?}");
        assert!(run.archived_at.is_some());
    }

    // The header's runs are gone from the next digest; the other folder's stay.
    assert!(digest_has(&mut c, &ws).await.is_empty());
    let there = digest_has(&mut c, &other).await;
    assert_eq!(there.len(), 2, "{there:?}");
    assert!(!c.get(&elsewhere.id).await.state.is_terminal());
    let q: Decision = c.ok("decision.get", json!({ "id": mine.id })).await;
    assert_eq!(q.status, DecisionStatus::Dismissed);
    let q: Decision = c.ok("decision.get", json!({ "id": theirs.id })).await;
    assert_eq!(q.status, DecisionStatus::Open);

    // Its flow no longer loads, and says why.
    let flows: Vec<Flow> = c.ok("flow.list", json!({})).await;
    assert!(flows.is_empty(), "{flows:?}");
    let errs: Vec<NotLoaded> = c.ok("flow.errors", json!(null)).await;
    assert_eq!(errs.iter().map(|e| e.flow.as_str()).collect::<Vec<_>>(), ["there"]);

    let _: Run = c.ok("run.stop", json!({ "id": elsewhere.id, "force": true })).await;
    c.wait_terminal(&elsewhere.id).await;
    h.stop().await;
}

/// A folder deleted from disk after it was registered can still be removed,
/// by the path it had or any spelling that leads to where it was.
#[tokio::test]
async fn a_folder_gone_from_disk_can_still_be_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    for (name, spell) in [("gone", false), ("vanished", true)] {
        let ws = tmp.path().join(name);
        std::fs::create_dir_all(&ws).unwrap();
        let up: Workspace = c.ok("workspace.up", json!({ "path": ws })).await;
        std::fs::remove_dir_all(&ws).unwrap();
        // The temp dir's own spelling (`/var` for `/private/var` on macOS)
        // is not the registered one; only the parent can say so now.
        let path = if spell { ws.clone() } else { up.path.clone() };
        let r: WorkspaceRemoved = c.ok("workspace.remove", json!({ "path": path })).await;
        assert!(r.removed, "{name}: {r:?}");
        assert_eq!(r.path, up.path);
    }
    let list: Vec<Workspace> = c.ok("workspace.list", json!(null)).await;
    assert!(list.is_empty(), "{list:?}");
    // A folder never registered is no error: nothing was, nothing is.
    let r: WorkspaceRemoved = c
        .ok("workspace.remove", json!({ "path": tmp.path().join("never") }))
        .await;
    assert!(!r.removed);
    h.stop().await;
}
