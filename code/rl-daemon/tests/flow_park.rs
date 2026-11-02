//! Parking a flow: its folder renamed `_<name>`, its runs archived, its yes taken; and bringing
//! it back, needing a yes again.

mod common;
use common::{Conn, Harness};
use rl_core::Home;
use rl_ledger::Ledger;
use rl_proto::flow::{Firing, FiringStatus, Flow, Parked, Reloaded, StepStatus, Unparked};
use rl_proto::types::Run;
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

const HELLO: &str = "---\nsteps:\n  a:\n    run: echo hi\n---\n";

fn write(home: &Home, name: &str, flow_md: &str) {
    let dir = home.flow_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("flow.md"), flow_md).unwrap();
}

/// A daemon on a fresh home with each of `flows` written; `None` when `rl-session` is not built.
async fn start(tmp: &Path, flows: &[(&str, &str)]) -> Option<(Harness, Conn)> {
    let home = Home::at(tmp.join("home"));
    for (name, md) in flows {
        write(&home, name, md);
    }
    let h = Harness::on(home)?;
    let c = Conn::open(&h.home).await;
    Some((h, c))
}

/// Polls `flow.firings` until the newest firing of `flow` is as `done` says.
async fn wait_firing(c: &mut Conn, flow: &str, done: impl Fn(&Firing) -> bool) -> Firing {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": flow })).await;
        if let Some(f) = firings.into_iter().next().filter(&done) {
            return f;
        }
        assert!(Instant::now() < deadline, "`{flow}` never got there");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Says yes to `flow`, fires it, and waits for the firing to end: the run of its step `a`.
async fn run_to_its_end(c: &mut Conn, flow: &str) -> String {
    common::approve(c, flow).await;
    let _: Firing = c.ok("flow.run", json!({ "name": flow })).await;
    let f = wait_firing(c, flow, |f| f.ended_at.is_some()).await;
    let run = f.steps[0].run.clone().expect("a ran");
    c.wait_terminal(&run).await;
    run
}

async fn listed(c: &mut Conn) -> Vec<Flow> {
    c.ok("flow.list", json!({})).await
}

async fn parked(c: &mut Conn) -> Vec<Parked> {
    c.ok("flow.parked", json!(null)).await
}

/// The error a call gives, as said.
async fn refused(c: &mut Conn, method: &str, params: serde_json::Value) -> String {
    let res = c.call(method, params).await;
    res.error.unwrap_or_else(|| panic!("{method} was not refused")).message
}

#[tokio::test]
async fn park_renames_archives_and_removes() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[("hello", HELLO)]).await else {
        return;
    };
    let run = run_to_its_end(&mut c, "hello").await;

    let p: Parked = c.ok("flow.park", json!({ "name": "hello" })).await;
    assert_eq!((p.name.as_str(), p.folder.as_str(), p.archived), ("hello", "_hello", 1));
    let flows = h.home.flows_dir();
    assert!(flows.join("_hello/flow.md").is_file());
    assert!(!flows.join("hello").exists());
    assert!(listed(&mut c).await.iter().all(|f| f.name != "hello"));
    assert!(c.get(&run).await.archived_at.is_some(), "its run archived");
    let all = parked(&mut c).await;
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!((all[0].folder.as_str(), all[0].archived), ("_hello", 0));
    assert_eq!(all[0].at, p.at, "when it was parked, as parking said");
    h.stop().await;
}

#[tokio::test]
async fn park_twice_takes_the_next_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[("hello", HELLO)]).await else {
        return;
    };
    let first: Parked = c.ok("flow.park", json!({ "name": "hello" })).await;
    assert_eq!(first.folder, "_hello");
    write(&h.home, "hello", "---\nsteps:\n  b:\n    run: echo again\n---\n");
    let r: Reloaded = c.ok("flow.reload", json!(null)).await;
    assert_eq!(r.added, ["hello"], "{r:?}");
    let second: Parked = c.ok("flow.park", json!({ "name": "hello" })).await;
    assert_eq!(second.folder, "_hello.2");

    let flows = h.home.flows_dir();
    assert!(
        std::fs::read_to_string(flows.join("_hello/flow.md"))
            .unwrap()
            .contains("echo hi")
    );
    assert!(
        std::fs::read_to_string(flows.join("_hello.2/flow.md"))
            .unwrap()
            .contains("echo again")
    );
    let folders: Vec<String> = parked(&mut c).await.into_iter().map(|p| p.folder).collect();
    assert_eq!(folders, ["_hello", "_hello.2"], "the same name, by when");
    h.stop().await;
}

#[tokio::test]
async fn park_refuses_while_firing() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: sleep 30\n---\n";
    let Some((h, mut c)) = start(tmp.path(), &[("hello", md)]).await else {
        return;
    };
    common::approve(&mut c, "hello").await;
    let _: Firing = c.ok("flow.run", json!({ "name": "hello" })).await;
    let f = wait_firing(&mut c, "hello", |f| f.steps[0].status == StepStatus::Running).await;
    let run = f.steps[0].run.clone().unwrap();

    let said = refused(&mut c, "flow.park", json!({ "name": "hello" })).await;
    assert!(
        said.ends_with("hello is firing · /stop its run, then park it"),
        "{said}"
    );
    let flows = h.home.flows_dir();
    assert!(flows.join("hello/flow.md").is_file(), "nothing renamed");
    assert!(!flows.join("_hello").exists());
    assert!(listed(&mut c).await.iter().any(|f| f.name == "hello" && f.approved()));
    assert!(parked(&mut c).await.is_empty());

    let _: Run = c.ok("run.stop", json!({ "id": run })).await;
    wait_firing(&mut c, "hello", |f| f.ended_at.is_some()).await;
    h.stop().await;
}

#[tokio::test]
async fn park_refuses_while_waiting_on_a_question() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    ask: go on?\n    options: { ok: {} }\n---\n";
    let Some((h, mut c)) = start(tmp.path(), &[("hello", md)]).await else {
        return;
    };
    common::approve(&mut c, "hello").await;
    let _: Firing = c.ok("flow.run", json!({ "name": "hello" })).await;
    wait_firing(&mut c, "hello", |f| f.status == FiringStatus::Waiting).await;

    let said = refused(&mut c, "flow.park", json!({ "name": "hello" })).await;
    assert!(
        said.ends_with("hello is waiting on a question · answer or /stop it, then park it"),
        "{said}"
    );
    assert!(h.home.flow_dir("hello").join("flow.md").is_file(), "nothing renamed");
    h.stop().await;
}

/// A step lost while `rld` was down leaves its firing waiting too: the refusal says so, not that
/// it is waiting on a question.
#[cfg(unix)]
#[tokio::test]
async fn park_refuses_while_a_step_is_lost() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: sleep 30\n---\n";
    let Some((h, mut c)) = start(tmp.path(), &[("hello", md)]).await else {
        return;
    };
    common::approve(&mut c, "hello").await;
    let _: Firing = c.ok("flow.run", json!({ "name": "hello" })).await;
    let f = wait_firing(&mut c, "hello", |f| f.steps[0].run.is_some()).await;
    let old = f.steps[0].run.clone().unwrap();
    drop(c);
    let home = h.home.clone();
    h.stop().await;

    common::kill_holder(&home, &old, true).await;

    let h = Harness::on(home).unwrap();
    let mut c = Conn::open(&h.home).await;
    wait_firing(&mut c, "hello", |f| f.steps[0].status == StepStatus::Lost).await;

    let said = refused(&mut c, "flow.park", json!({ "name": "hello" })).await;
    assert!(
        said.ends_with("hello is waiting on its lost step a · /resume its run or /stop it, then park it"),
        "{said}"
    );
    assert!(h.home.flow_dir("hello").join("flow.md").is_file(), "nothing renamed");
    assert!(!h.home.flows_dir().join("_hello").exists());
    h.stop().await;
}

/// The ledger refuses what parking writes after the rename: the flow is read again all the
/// same, so it is not left loaded with its folder gone, and the error says it is parked.
#[tokio::test]
async fn park_reads_the_flows_again_when_the_ledger_fails_after_the_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[("hello", HELLO)]).await else {
        return;
    };
    run_to_its_end(&mut c, "hello").await;
    let ledger = Ledger::open(&h.home.ledger()).unwrap();
    ledger
        .call_blocking(|db| {
            db.execute_batch(
                "CREATE TRIGGER no_archive BEFORE UPDATE ON runs BEGIN SELECT RAISE(ABORT, 'injected'); END",
            )?;
            Ok(())
        })
        .unwrap();

    let said = refused(&mut c, "flow.park", json!({ "name": "hello" })).await;
    assert!(said.contains("hello is parked as _hello"), "{said}");
    assert!(h.home.flows_dir().join("_hello/flow.md").is_file());
    assert!(
        listed(&mut c).await.iter().all(|f| f.name != "hello"),
        "not left loaded"
    );

    ledger
        .call_blocking(|db| Ok(db.execute_batch("DROP TRIGGER no_archive")?))
        .unwrap();
    let back: Unparked = c.ok("flow.unpark", json!({ "folder": "_hello" })).await;
    assert!(
        !back.flow.unwrap().approved(),
        "the yes was taken before the archive failed"
    );
    h.stop().await;
}

#[tokio::test]
async fn park_archives_only_its_own_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[("nightly", HELLO), ("nightly-2", HELLO)]).await else {
        return;
    };
    let mine = run_to_its_end(&mut c, "nightly").await;
    let theirs = run_to_its_end(&mut c, "nightly-2").await;
    let p: Parked = c.ok("flow.park", json!({ "name": "nightly" })).await;
    assert_eq!(p.archived, 1);
    assert!(c.get(&mine).await.archived_at.is_some());
    assert!(
        c.get(&theirs).await.archived_at.is_none(),
        "nightly-2's runs are its own"
    );
    assert!(
        listed(&mut c)
            .await
            .iter()
            .any(|f| f.name == "nightly-2" && f.approved())
    );
    h.stop().await;
}

#[tokio::test]
async fn unpark_brings_it_back_needing_a_yes() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[("hello", HELLO)]).await else {
        return;
    };
    let yes = common::approve(&mut c, "hello").await;
    assert!(yes.approved());
    let _: Parked = c.ok("flow.park", json!({ "name": "hello" })).await;

    let u: Unparked = c.ok("flow.unpark", json!({ "folder": "_hello" })).await;
    assert_eq!(u.name, "hello");
    assert_eq!(u.problem, None);
    let back = u.flow.expect("it loaded");
    assert_eq!(back.name, "hello");
    assert_eq!(back.hash, yes.hash, "the very folder said yes to");
    assert!(!back.approved(), "and still it needs a yes: {back:?}");
    let flows = h.home.flows_dir();
    assert!(flows.join("hello/flow.md").is_file());
    assert!(!flows.join("_hello").exists());
    let hello = listed(&mut c).await.into_iter().find(|f| f.name == "hello").unwrap();
    assert!(!hello.approved());
    assert!(parked(&mut c).await.is_empty());
    h.stop().await;
}

#[tokio::test]
async fn unpark_refuses_a_taken_name() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[("hello", HELLO)]).await else {
        return;
    };
    let _: Parked = c.ok("flow.park", json!({ "name": "hello" })).await;
    write(&h.home, "hello", "---\nsteps:\n  b:\n    run: echo again\n---\n");
    let _: Reloaded = c.ok("flow.reload", json!(null)).await;
    let _: Parked = c.ok("flow.park", json!({ "name": "hello" })).await;
    write(&h.home, "hello", "---\nsteps:\n  c:\n    run: echo third\n---\n");
    let _: Reloaded = c.ok("flow.reload", json!(null)).await;

    for folder in ["_hello", "_hello.2"] {
        let said = refused(&mut c, "flow.unpark", json!({ "folder": folder })).await;
        assert!(
            said.ends_with("a flow named hello is there now · park or rename it first"),
            "{said}"
        );
    }
    let flows = h.home.flows_dir();
    assert!(
        std::fs::read_to_string(flows.join("hello/flow.md"))
            .unwrap()
            .contains("echo third")
    );
    assert!(flows.join("_hello/flow.md").is_file());
    assert!(flows.join("_hello.2/flow.md").is_file());

    let said = refused(&mut c, "flow.unpark", json!({ "folder": "hello" })).await;
    assert!(said.ends_with("hello is not a parked flow"), "{said}");
    h.stop().await;
}

#[tokio::test]
async fn park_unknown_flow_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[]).await else {
        return;
    };
    let said = refused(&mut c, "flow.park", json!({ "name": "nope" })).await;
    assert!(said.contains("nope"), "{said}");
    for name in ["../nope", "_hello"] {
        let said = refused(&mut c, "flow.park", json!({ "name": name })).await;
        assert!(said.contains(name), "{said}");
    }
    assert!(parked(&mut c).await.is_empty());
    h.stop().await;
}

#[tokio::test]
async fn parked_lists_in_from_the_frontmatter() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let ws = std::fs::canonicalize(&ws).unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[]).await else {
        return;
    };
    let _: serde_json::Value = c.ok("workspace.up", json!({ "path": ws })).await;
    write(
        &h.home,
        "hello",
        &format!("---\nin: {}\nsteps:\n  a:\n    run: echo hi\n---\n", ws.display()),
    );
    let r: Reloaded = c.ok("flow.reload", json!(null)).await;
    assert_eq!(r.added, ["hello"], "{r:?}");
    let p: Parked = c.ok("flow.park", json!({ "name": "hello" })).await;
    assert_eq!(p.in_.as_deref(), Some(ws.as_path()));
    let all = parked(&mut c).await;
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].in_.as_deref(), Some(ws.as_path()));
    let wire = serde_json::to_value(&all[0]).unwrap();
    assert_eq!(wire["in"], json!(ws), "said as `in`, as a flow file says it");
    h.stop().await;
}

#[tokio::test]
async fn unpark_of_a_folder_that_does_not_load_says_why_in_its_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[("hello", HELLO)]).await else {
        return;
    };
    let _: Parked = c.ok("flow.park", json!({ "name": "hello" })).await;
    let flows = h.home.flows_dir();
    std::fs::write(flows.join("_hello/flow.md"), "---\nsteps: [\n---\n").unwrap();

    let u: Unparked = c.ok("flow.unpark", json!({ "folder": "_hello" })).await;
    assert_eq!(u.name, "hello");
    assert!(u.flow.is_none(), "{u:?}");
    let why = u.problem.expect("why it did not load");
    assert!(why.contains("flow.md:"), "{why}");
    assert!(flows.join("hello/flow.md").is_file(), "it is back all the same");
    h.stop().await;
}

/// Reloads from another connection never sync a folder list read before a park's rename after
/// the park's own reload: once parked, the flow is not listed.
#[tokio::test]
async fn a_reload_racing_a_park_never_leaves_it_listed() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[("hello", HELLO)]).await else {
        return;
    };
    let stop = Arc::new(AtomicBool::new(false));
    let mut other = Conn::open(&h.home).await;
    let looping = {
        let stop = stop.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let _: Reloaded = other.ok("flow.reload", json!(null)).await;
            }
        })
    };
    for i in 0..25 {
        let _: Parked = c.ok("flow.park", json!({ "name": "hello" })).await;
        let names: Vec<String> = listed(&mut c).await.into_iter().map(|f| f.name).collect();
        assert!(!names.iter().any(|n| n == "hello"), "round {i}: {names:?}");
        let _: Unparked = c.ok("flow.unpark", json!({ "folder": "_hello" })).await;
    }
    stop.store(true, Ordering::Relaxed);
    looping.await.unwrap();
    h.stop().await;
}
