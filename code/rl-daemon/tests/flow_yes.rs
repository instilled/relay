//! A flow runs only on the folder said yes to: what changed since is caught before a firing
//! starts and before each step does, reload or no reload.

mod common;
use common::{Conn, Harness, approve};
use rl_core::Home;
use rl_proto::flow::{Firing, FiringStatus, Flow, FlowStatus, StepStatus};
use rl_proto::types::Event;
use serde_json::json;
use std::time::{Duration, Instant};

fn write(home: &Home, name: &str, files: &[(&str, &str)]) {
    let dir = home.flow_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    for (f, text) in files {
        std::fs::write(dir.join(f), text).unwrap();
    }
}

async fn status(c: &mut Conn, name: &str) -> FlowStatus {
    let flows: Vec<Flow> = c.ok("flow.list", json!({})).await;
    flows.iter().find(|f| f.name == name).unwrap().status()
}

/// Polls `flow.firings` every 50 ms for 10 s until the newest firing has ended.
async fn wait_ended(c: &mut Conn, flow: &str) -> Firing {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": flow })).await;
        if let Some(f) = firings.into_iter().next().filter(|f| f.ended_at.is_some()) {
            return f;
        }
        assert!(Instant::now() < deadline, "`{flow}` never ended a firing");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_script_changed_after_its_yes_does_not_fire() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    write(
        &home,
        "probe",
        &[
            ("flow.md", "---\nsteps:\n  a:\n    run: sh probe.sh\n---\n"),
            ("probe.sh", "echo looking\n"),
        ],
    );
    let Some(h) = Harness::on(home.clone()) else { return };
    let mut c = Conn::open(&h.home).await;
    approve(&mut c, "probe").await;
    // Rewritten with no reload: the ledger still has the yes, the folder does not.
    write(&home, "probe", &[("probe.sh", "echo something else\n")]);
    let res = c.call("flow.run", json!({ "name": "probe" })).await;
    let err = res.error.expect("a changed folder does not fire");
    assert!(err.message.contains("needs a yes"), "{err:?}");
    let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": "probe" })).await;
    assert!(firings.is_empty(), "{firings:?}");
    assert_eq!(status(&mut c, "probe").await, FlowStatus::NeedsApproval, "read again");
    h.stop().await;
}

#[tokio::test]
async fn a_script_changed_mid_firing_does_not_start_its_step() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    let md = "---\nsteps:\n  a:\n    run: sleep 1\n  b:\n    after: a\n    run: sh apply.sh\n---\n";
    write(&home, "slow", &[("flow.md", md), ("apply.sh", "echo as approved\n")]);
    let Some(h) = Harness::on(home.clone()) else { return };
    let mut c = Conn::open(&h.home).await;
    approve(&mut c, "slow").await;
    let _: Firing = c.ok("flow.run", json!({ "name": "slow" })).await;
    write(&home, "slow", &[("apply.sh", "echo not as approved\n")]);
    let f = wait_ended(&mut c, "slow").await;
    assert_eq!(f.status, FiringStatus::Failed, "{f:?}");
    let b = f.steps.iter().find(|s| s.name == "b").unwrap();
    assert_eq!((b.status, b.run.as_deref()), (StepStatus::Failed, None), "{f:?}");
    let skipped: Vec<Event> = c.ok("event.list", json!({ "kinds": ["flow.skipped"] })).await;
    let data = skipped[0].data.clone().unwrap();
    assert_eq!(data["step"], "b");
    assert_eq!(data["reason"], "the flow changed since its yes");
    assert_eq!(status(&mut c, "slow").await, FlowStatus::NeedsApproval, "read again");
    h.stop().await;
}

/// Lost while Relay was away, and its script changed meanwhile: resume does not start it.
#[cfg(unix)]
#[tokio::test]
async fn a_lost_step_is_not_picked_up_on_a_changed_folder() {
    use rl_proto::methods::Resumed;
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    write(
        &home,
        "far",
        &[
            ("flow.md", "---\nsteps:\n  a:\n    run: sh far.sh\n---\n"),
            ("far.sh", "sleep 30\n"),
        ],
    );
    let Some(h) = Harness::on(home.clone()) else { return };
    let mut c = Conn::open(&h.home).await;
    approve(&mut c, "far").await;
    let _: Firing = c.ok("flow.run", json!({ "name": "far" })).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    let old = loop {
        let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": "far" })).await;
        if let Some(run) = firings.first().and_then(|f| f.steps[0].run.clone()) {
            break run;
        }
        assert!(Instant::now() < deadline, "a never started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    drop(c);
    h.stop().await;
    common::kill_holder(&home, &old, true).await;
    write(&home, "far", &[("far.sh", "echo not as approved\n")]);

    let h = Harness::on(home.clone()).unwrap();
    let mut c = Conn::open(&h.home).await;
    let out: Vec<Resumed> = c.ok("run.resume", json!({ "ids": [old.clone()] })).await;
    assert!(out[0].run.is_none(), "{out:?}");
    assert!(
        out[0]
            .why_not
            .as_deref()
            .is_some_and(|e| e.contains("the flow changed since its yes")),
        "{out:?}"
    );
    // Said yes to now, the new version is not the one this firing began on: still not picked up.
    approve(&mut c, "far").await;
    let out: Vec<Resumed> = c.ok("run.resume", json!({ "ids": [old] })).await;
    assert!(out[0].run.is_none(), "{out:?}");
    assert!(
        out[0]
            .why_not
            .as_deref()
            .is_some_and(|e| e.contains("the flow changed since this firing began")),
        "{out:?}"
    );
    h.stop().await;
}

/// A yes is to the version shown: a reload between seeing a flow and saying yes leaves it
/// needing one.
#[tokio::test]
async fn a_yes_to_a_version_gone_by_is_not_given() {
    use rl_proto::flow::Reloaded;
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    write(
        &home,
        "pulse",
        &[
            ("flow.md", "---\nsteps:\n  a:\n    run: sh a.sh\n---\n"),
            ("a.sh", "date\n"),
        ],
    );
    let Some(h) = Harness::on(home.clone()) else { return };
    let mut c = Conn::open(&h.home).await;
    let flows: Vec<Flow> = c.ok("flow.list", json!({})).await;
    let shown = flows[0].hash.clone();
    write(&home, "pulse", &[("a.sh", "rm -rf ~\n")]);
    let r: Reloaded = c.ok("flow.reload", json!(null)).await;
    assert_eq!(r.changed, ["pulse"]);
    let res = c.call("flow.approve", json!({ "name": "pulse", "hash": shown })).await;
    let err = res.error.expect("the version shown is gone");
    assert!(err.message.contains("changed since it was shown"), "{err:?}");
    assert_eq!(status(&mut c, "pulse").await, FlowStatus::NeedsApproval);
    let f = approve(&mut c, "pulse").await;
    assert_eq!(f.status(), FlowStatus::On, "the version shown now");
    let res = c.call("flow.approve", json!({ "name": "nope", "hash": shown })).await;
    assert!(res.error.unwrap().message.contains("nope"), "no such flow");
    h.stop().await;
}
