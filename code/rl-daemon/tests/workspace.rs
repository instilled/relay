//! Workspaces against a real daemon: `rl up` registers a folder and loads its
//! `.relay/schedules.yaml`; nothing fires until approved; an edit asks again;
//! runs started inside the folder belong to it; unregistering stops it all;
//! removing a project takes its runs, questions and approvals with it, and
//! moves its rule files aside when asked — never anyone else's.

mod common;

use common::{Conn, Harness, fake_cli};
use rl_core::Home;
use rl_proto::decision::{Decision, DecisionStatus};
use rl_proto::methods::{Digest, WorkspaceRemoved};
use rl_proto::reaction::Reaction;
use rl_proto::schedule::{Schedule, UpReport, Workspace};
use rl_proto::types::{Run, RunState};
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
    let gone: WorkspaceRemoved = c.ok("workspace.remove", json!({ "path": ws })).await;
    assert!(gone.removed);
    assert_eq!(
        (gone.stopped, gone.archived),
        (0, 0),
        "nothing asked beyond unregistering"
    );
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

const RULES: &str = "schedules:\n  nightly:\n    every: 1h\n    run: [true]\n";
const REACTS: &str = "reactions:\n  tell:\n    on: run.failed\n    run: 'true'\n";

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
/// SIGTERM, an agent — archives what has ended, dismisses what it asked and
/// forgets the yes given to its rules, so the header goes and registering it
/// again asks again. Another folder's runs and questions are left be.
#[tokio::test]
async fn removing_a_project_stops_and_archives_its_runs_and_lets_its_questions_go() {
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
    write_ws(&ws, RULES);
    std::fs::write(ws.join(".relay/reactions.yaml"), REACTS).unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    let up: UpReport = c.ok("workspace.up", json!({ "path": ws, "approve": true })).await;
    assert_eq!(up.approved, ["nightly"]);
    assert_eq!(up.approved_reactions, ["tell"]);

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
    assert_eq!(r.rules_moved_to, None);
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

    // The rules are removed and their yes forgotten: back again, they ask.
    let all: Vec<Schedule> = c.ok("schedule.list", json!({ "all": true })).await;
    assert!(all[0].removed_at.is_some() && all[0].approved_hash.is_none(), "{all:?}");
    let all: Vec<Reaction> = c.ok("reaction.list", json!({ "all": true })).await;
    assert!(all[0].removed_at.is_some() && all[0].approved_hash.is_none(), "{all:?}");
    assert!(ws.join(".relay/schedules.yaml").is_file(), "not asked to move, so kept");
    let up: UpReport = c.ok("workspace.up", json!({ "path": ws })).await;
    assert!(!up.schedules[0].approved(), "registered again, it asks again");
    assert!(up.reactions.iter().all(|r| r.approved_hash.is_none()));

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
        write_ws(&ws, RULES);
        let up: UpReport = c.ok("workspace.up", json!({ "path": ws })).await;
        std::fs::remove_dir_all(&ws).unwrap();
        // The temp dir's own spelling (`/var` for `/private/var` on macOS)
        // is not the registered one; only the parent can say so now.
        let path = if spell { ws.clone() } else { up.workspace.path.clone() };
        let r: WorkspaceRemoved = c.ok("workspace.remove", json!({ "path": path })).await;
        assert!(r.removed, "{name}: {r:?}");
        assert_eq!(r.path, up.workspace.path);
    }
    let list: Vec<Workspace> = c.ok("workspace.list", json!(null)).await;
    assert!(list.is_empty(), "{list:?}");
    let live: Vec<Schedule> = c.ok("schedule.list", json!({})).await;
    assert!(live.is_empty());
    // A folder never registered is no error: nothing was, nothing is.
    let r: WorkspaceRemoved = c
        .ok("workspace.remove", json!({ "path": tmp.path().join("never") }))
        .await;
    assert!(!r.removed);
    h.stop().await;
}

/// Asked to, removal moves the two rule files into the Relay home's
/// `removed/` and leaves anything else the project keeps in `.relay`. A
/// `.relay` that is a symlink is refused before anything is done.
#[tokio::test]
async fn the_rules_move_aside_and_a_symlink_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let ws = tmp.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let ws = std::fs::canonicalize(&ws).unwrap();
    write_ws(&ws, RULES);
    std::fs::write(ws.join(".relay/reactions.yaml"), REACTS).unwrap();
    std::fs::write(ws.join(".relay/notes.md"), "ours").unwrap();
    let linked = tmp.path().join("linked");
    let target = tmp.path().join("shared-rules");
    std::fs::create_dir_all(&linked).unwrap();
    write_ws(&target, RULES);
    std::os::unix::fs::symlink(target.join(".relay"), linked.join(".relay")).unwrap();
    let linked = std::fs::canonicalize(&linked).unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    let _: UpReport = c.ok("workspace.up", json!({ "path": ws, "approve": true })).await;
    let _: UpReport = c.ok("workspace.up", json!({ "path": linked })).await;

    let r: WorkspaceRemoved = c
        .ok("workspace.remove", json!({ "path": ws, "move_rules": true }))
        .await;
    let dest = r.rules_moved_to.expect("moved");
    assert!(dest.starts_with(home.removed_dir()), "{}", dest.display());
    assert!(
        dest.file_name().unwrap().to_string_lossy().starts_with("proj-"),
        "{}",
        dest.display()
    );
    assert_eq!(std::fs::read_to_string(dest.join("schedules.yaml")).unwrap(), RULES);
    assert_eq!(std::fs::read_to_string(dest.join("reactions.yaml")).unwrap(), REACTS);
    assert!(!ws.join(".relay/schedules.yaml").exists());
    assert!(!ws.join(".relay/reactions.yaml").exists());
    assert_eq!(std::fs::read_to_string(ws.join(".relay/notes.md")).unwrap(), "ours");
    let all: Vec<Schedule> = c.ok("schedule.list", json!({ "all": true })).await;
    let mine: Vec<&Schedule> = all.iter().filter(|s| s.scope == ws.to_string_lossy()).collect();
    assert!(mine.iter().all(|s| s.removed_at.is_some()), "{mine:?}");

    let res = c
        .call("workspace.remove", json!({ "path": linked, "move_rules": true }))
        .await;
    let msg = res.error.map(|e| e.message).unwrap_or_default();
    assert!(msg.contains("symlink"), "{msg}");
    let list: Vec<Workspace> = c.ok("workspace.list", json!(null)).await;
    assert_eq!(list.len(), 1, "refused before anything was done: {list:?}");
    assert!(target.join(".relay/schedules.yaml").is_file());
    h.stop().await;
}

/// Someone registered their home folder, whose `.relay` is the Relay home:
/// its files are the user's own global rules, so they are never moved.
#[tokio::test]
async fn the_relay_home_is_never_moved_aside() {
    let tmp = tempfile::tempdir().unwrap();
    let me = std::fs::canonicalize(tmp.path()).unwrap().join("me");
    let home = Home::at(me.join(".relay"));
    home.ensure_dirs().unwrap();
    std::fs::write(home.schedules_file(), RULES).unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    let _: UpReport = c.ok("workspace.up", json!({ "path": me })).await;
    let res = c
        .call("workspace.remove", json!({ "path": me, "move_rules": true }))
        .await;
    let msg = res.error.map(|e| e.message).unwrap_or_default();
    assert!(msg.contains("Relay home"), "{msg}");
    assert_eq!(std::fs::read_to_string(home.schedules_file()).unwrap(), RULES);
    let list: Vec<Workspace> = c.ok("workspace.list", json!(null)).await;
    assert_eq!(list.len(), 1);
    // Without the move it goes like any other.
    let r: WorkspaceRemoved = c.ok("workspace.remove", json!({ "path": me })).await;
    assert!(r.removed);
    assert!(home.schedules_file().is_file());
    h.stop().await;
}
