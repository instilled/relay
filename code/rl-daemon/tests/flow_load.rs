//! Flows load from ~/.relay/flows, say what did not, and wait for a yes.

mod common;
use common::{Conn, Harness};
use rl_core::Home;
use rl_proto::flow::{Flow, FlowStatus, NotLoaded, Reloaded};
use serde_json::json;

fn write(home: &Home, name: &str, files: &[(&str, &str)]) {
    let dir = home.flow_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    for (f, text) in files {
        let path = dir.join(f);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

#[tokio::test]
async fn a_flow_loads_needs_a_yes_and_a_broken_one_says_where() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    write(
        &home,
        "pulse",
        &[("flow.md", "---\nwhen: every 1h\nsteps:\n  beat:\n    run: date\n---\n")],
    );
    write(
        &home,
        "broken",
        &[("flow.md", "---\nsteps:\n  a:\n    after: nope\n    run: x\n---\n")],
    );
    let Some(h) = Harness::on(home.clone()) else { return };
    let mut c = Conn::open(&h.home).await;
    let flows: Vec<Flow> = c.ok("flow.list", json!({})).await;
    assert_eq!(flows.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["pulse"]);
    assert_eq!(flows[0].status(), FlowStatus::NeedsApproval);
    assert_eq!(flows[0].when(), "every 1h");
    assert_eq!(flows[0].source, home.flow_dir("pulse"));
    let errs: Vec<NotLoaded> = c.ok("flow.errors", json!(null)).await;
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].flow, "broken");
    assert_eq!(errs[0].line, Some(3));
    assert!(errs[0].message.contains("no step"), "{:?}", errs[0]);
    assert_eq!(errs[0].file, home.flow_dir("broken").join("flow.md"));
    let f: Flow = c.ok("flow.approve", json!({ "name": "pulse" })).await;
    assert_eq!(f.status(), FlowStatus::On);
    assert!(home.flows_dir().join(".gitignore").exists());
    assert_eq!(
        std::fs::read_to_string(home.flows_dir().join(".gitignore")).unwrap(),
        "runs/\n"
    );
    let f: Flow = c.ok("flow.pause", json!({ "name": "pulse" })).await;
    assert_eq!(f.status(), FlowStatus::Paused);
    let f: Flow = c.ok("flow.resume", json!({ "name": "pulse" })).await;
    assert_eq!(f.status(), FlowStatus::On);
    let res = c.call("flow.approve", json!({ "name": "broken" })).await;
    assert!(res.error.is_some(), "a flow that did not load takes no yes");
    h.stop().await;
}

#[tokio::test]
async fn a_changed_script_asks_again() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    write(
        &home,
        "pulse",
        &[
            ("flow.md", "---\nsteps:\n  beat:\n    run: beat.sh\n---\n"),
            ("beat.sh", "#!/bin/sh\ndate\n"),
        ],
    );
    let Some(h) = Harness::on(home.clone()) else { return };
    let mut c = Conn::open(&h.home).await;
    let f: Flow = c.ok("flow.approve", json!({ "name": "pulse" })).await;
    assert_eq!(f.status(), FlowStatus::On);
    assert_eq!(f.when(), "by hand");

    // What a firing leaves in runs/ is not part of what was said yes to.
    write(&home, "pulse", &[("runs/f1/beat.out", "Tue Oct 7")]);
    let r: Reloaded = c.ok("flow.reload", json!(null)).await;
    assert_eq!((r.loaded, r.changed.len()), (1, 0), "{r:?}");

    write(&home, "pulse", &[("beat.sh", "#!/bin/sh\ndate -u\n")]);
    let r: Reloaded = c.ok("flow.reload", json!(null)).await;
    assert_eq!(r.changed, ["pulse"]);
    assert!(r.not_loaded.is_empty(), "{r:?}");
    let flows: Vec<Flow> = c.ok("flow.list", json!({})).await;
    assert_eq!(flows[0].status(), FlowStatus::NeedsApproval);
    assert_ne!(flows[0].approved_hash.as_deref(), Some(flows[0].hash.as_str()));

    // A folder gone is a flow removed, still there to list with --all.
    std::fs::remove_dir_all(home.flow_dir("pulse")).unwrap();
    let r: Reloaded = c.ok("flow.reload", json!(null)).await;
    assert_eq!(r.removed, ["pulse"]);
    let flows: Vec<Flow> = c.ok("flow.list", json!({})).await;
    assert!(flows.is_empty());
    let flows: Vec<Flow> = c.ok("flow.list", json!({ "all": true })).await;
    assert_eq!(flows[0].status(), FlowStatus::Removed);

    // Back, then broken: said as not loaded, never as removed.
    write(
        &home,
        "pulse",
        &[("flow.md", "---\nsteps:\n  beat:\n    run: date\n---\n")],
    );
    let r: Reloaded = c.ok("flow.reload", json!(null)).await;
    assert_eq!(r.added, ["pulse"]);
    write(&home, "pulse", &[("flow.md", "---\nsteps: {}\n---\n")]);
    let r: Reloaded = c.ok("flow.reload", json!(null)).await;
    assert!(r.removed.is_empty(), "{r:?}");
    assert_eq!(r.not_loaded.len(), 1, "{r:?}");
    assert!(r.not_loaded[0].message.contains("at least one step"), "{r:?}");
    h.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_pipe_in_a_flow_folder_does_not_hold_the_daemon_up() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    write(
        &home,
        "pulse",
        &[("flow.md", "---\nsteps:\n  beat:\n    run: date\n---\n")],
    );
    let dir = home.flow_dir("pulse");
    nix::unistd::mkfifo(&dir.join("pipe"), nix::sys::stat::Mode::S_IRWXU).unwrap();
    std::os::unix::fs::symlink(&dir, dir.join("loop")).unwrap();
    let Some(h) = Harness::on(home.clone()) else { return };
    // The daemon answers only once the flows are loaded.
    let flows: Vec<Flow> = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut c = Conn::open(&h.home).await;
        c.ok("flow.list", json!({})).await
    })
    .await
    .expect("rld came up");
    assert_eq!(flows.len(), 1);
    h.stop().await;
}

#[tokio::test]
async fn an_in_relay_does_not_know_does_not_load() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let text = format!("---\nin: {}\nsteps:\n  a:\n    run: x\n---\n", elsewhere.display());
    write(&home, "there", &[("flow.md", &text)]);
    let Some(h) = Harness::on(home.clone()) else { return };
    let mut c = Conn::open(&h.home).await;
    let flows: Vec<Flow> = c.ok("flow.list", json!({})).await;
    assert!(flows.is_empty(), "{flows:?}");
    let errs: Vec<NotLoaded> = c.ok("flow.errors", json!(null)).await;
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!((errs[0].flow.as_str(), errs[0].line), ("there", Some(2)));
    assert!(errs[0].message.contains("rl add"), "{:?}", errs[0]);

    // Once Relay knows the folder, by whatever spelling, the flow loads.
    let _: serde_json::Value = c.ok("workspace.up", json!({ "path": elsewhere })).await;
    let r: Reloaded = c.ok("flow.reload", json!(null)).await;
    assert_eq!(r.added, ["there"], "{r:?}");
    assert!(r.not_loaded.is_empty(), "{r:?}");
    let errs: Vec<NotLoaded> = c.ok("flow.errors", json!(null)).await;
    assert!(errs.is_empty(), "{errs:?}");
    h.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_folder_that_cannot_be_read_stays_as_it_last_loaded() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    write(
        &home,
        "pulse",
        &[
            ("flow.md", "---\nsteps:\n  beat:\n    run: lib/beat.sh\n---\n"),
            ("lib/beat.sh", "date\n"),
        ],
    );
    let Some(h) = Harness::on(home.clone()) else { return };
    let mut c = Conn::open(&h.home).await;
    let was: Flow = c.ok("flow.approve", json!({ "name": "pulse" })).await;
    let lib = home.flow_dir("pulse").join("lib");
    std::fs::set_permissions(&lib, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads it all the same, and then there is nothing to see.
    let readable = std::fs::read_dir(&lib).is_ok();
    let r: Reloaded = c.ok("flow.reload", json!(null)).await;
    std::fs::set_permissions(&lib, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !readable {
        assert!(r.removed.is_empty() && r.changed.is_empty(), "{r:?}");
        assert_eq!(r.not_loaded.len(), 1, "{r:?}");
        assert!(r.not_loaded[0].message.contains("cannot be read"), "{r:?}");
        let flows: Vec<Flow> = c.ok("flow.list", json!({})).await;
        assert_eq!(flows.len(), 1);
        assert_eq!((flows[0].status(), &flows[0].hash), (FlowStatus::On, &was.hash));
    }
    h.stop().await;
}
