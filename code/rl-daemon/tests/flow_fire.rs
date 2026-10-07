//! A flow fired by hand: its steps run in order, in one chain, on the
//! definition the firing started with.

mod common;
use common::{Conn, Harness, wait_journal};
use rl_core::Home;
use rl_proto::flow::{Firing, FiringStatus, Flow, Reloaded, StepState, StepStatus};
use rl_proto::types::{Cause, Event, Run};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

fn write(home: &Home, name: &str, files: &[(&str, &str)]) {
    let dir = home.flow_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    for (f, text) in files {
        std::fs::write(dir.join(f), text).unwrap();
    }
}

/// A daemon on a fresh home with `flow.md` written for `name`; `None` when `rl-session` is not built.
async fn start(tmp: &Path, name: &str, flow_md: &str) -> Option<(Harness, Conn)> {
    let home = Home::at(tmp.join("home"));
    write(&home, name, &[("flow.md", flow_md)]);
    let h = Harness::on(home)?;
    let c = Conn::open(&h.home).await;
    Some((h, c))
}

/// Polls `flow.firings` every 50 ms for 10 s until the newest firing is `done`.
async fn wait_firing(c: &mut Conn, flow: &str, done: impl Fn(&Firing) -> bool) -> Firing {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": flow })).await;
        if let Some(f) = firings.into_iter().next() {
            if done(&f) {
                return f;
            }
            assert!(Instant::now() < deadline, "the firing never got there: {f:?}");
        } else {
            assert!(Instant::now() < deadline, "`{flow}` never fired");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn ended(f: &Firing) -> bool {
    f.ended_at.is_some()
}

fn step<'a>(f: &'a Firing, name: &str) -> &'a StepState {
    f.steps.iter().find(|s| s.name == name).unwrap()
}

async fn approve_and_run(c: &mut Conn, flow: &str) -> Firing {
    let _: Flow = c.ok("flow.approve", json!({ "name": flow })).await;
    c.ok("flow.run", json!({ "name": flow })).await
}

#[tokio::test]
async fn three_steps_run_in_order_in_one_chain_and_the_firing_ends_done() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let home = Home::at(tmp.path().join("home"));
    let Some(h) = Harness::on(home.clone()) else { return };
    let mut c = Conn::open(&h.home).await;
    let _: serde_json::Value = c.ok("workspace.up", json!({ "path": ws })).await;
    let text = format!(
        "---\nin: {}\nsteps:\n  a:\n    run: echo one\n  b:\n    after: a\n    run: echo two\n  \
         c:\n    after: b\n    if: a.out\n    run: echo three\n---\n",
        ws.display()
    );
    write(&home, "chain3", &[("flow.md", &text)]);
    let r: Reloaded = c.ok("flow.reload", json!(null)).await;
    assert_eq!(r.added, ["chain3"], "{r:?}");

    let fired = approve_and_run(&mut c, "chain3").await;
    assert_eq!(fired.trigger, "hand");
    let f = wait_firing(&mut c, "chain3", ended).await;
    assert_eq!(f.id, fired.id);
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    let mut started = Vec::new();
    for name in ["a", "b", "c"] {
        let s = step(&f, name);
        assert_eq!(s.status, StepStatus::Done, "{f:?}");
        let run = c.get(s.run.as_deref().expect("a done step has its run")).await;
        assert_eq!(
            run.cause,
            Cause::Flow {
                flow: "chain3".into(),
                step: name.into()
            }
        );
        assert_eq!(run.chain, f.id, "every step is in the firing's chain");
        assert_eq!(run.cwd, std::fs::canonicalize(&ws).unwrap(), "the flow's `in`");
        started.push(run.started_at);
    }
    assert!(started.is_sorted(), "a, then b, then c: {started:?}");

    let events: Vec<Event> = c
        .ok("event.list", json!({ "kinds": ["flow.fired", "flow.ended"] }))
        .await;
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["flow.fired", "flow.ended"]);
    let data = |i: usize| events[i].data.clone().unwrap();
    assert_eq!(data(0)["flow"], "chain3");
    assert_eq!(data(0)["firing"], f.id.as_str());
    assert_eq!(data(0)["trigger"], "hand");
    assert_eq!(data(1)["status"], "done");
    assert_eq!(events[0].workspace, ws, "the flow's `in`");
    h.stop().await;
}

#[tokio::test]
async fn an_if_that_does_not_hold_skips_and_ends_done() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: \"true\"\n  b:\n    after: a\n    if: a.out\n    run: echo no\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "quiet", md).await else {
        return;
    };
    approve_and_run(&mut c, "quiet").await;
    let f = wait_firing(&mut c, "quiet", ended).await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    assert_eq!(step(&f, "a").status, StepStatus::Done);
    assert_eq!(step(&f, "b").status, StepStatus::Skipped);
    assert_eq!(step(&f, "b").run, None, "a skipped step runs nothing");
    h.stop().await;
}

#[tokio::test]
async fn a_failed_step_fails_the_firing() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: exit 3\n  b:\n    after: a\n    run: echo never\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "sad", md).await else {
        return;
    };
    approve_and_run(&mut c, "sad").await;
    let f = wait_firing(&mut c, "sad", ended).await;
    assert_eq!(f.status, FiringStatus::Failed, "{f:?}");
    assert_eq!(step(&f, "a").status, StepStatus::Failed);
    assert_eq!(step(&f, "b").status, StepStatus::Skipped);
    assert_eq!(step(&f, "b").run, None);
    h.stop().await;
}

#[tokio::test]
async fn an_unapproved_flow_does_not_run() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: echo hi\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "unsure", md).await else {
        return;
    };
    let res = c.call("flow.run", json!({ "name": "unsure" })).await;
    let err = res.error.expect("an unapproved flow does not run");
    assert!(err.message.contains("needs a yes"), "{err:?}");
    let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": "unsure" })).await;
    assert!(firings.is_empty(), "{firings:?}");
    let res = c.call("flow.run", json!({ "name": "nope" })).await;
    assert!(res.error.is_some(), "no such flow");
    h.stop().await;
}

#[tokio::test]
async fn an_edit_mid_firing_leaves_that_firing_as_it_started() {
    let tmp = tempfile::tempdir().unwrap();
    let md = |b: &str| {
        format!("---\nsteps:\n  a:\n    run: sleep 1; echo old\n  b:\n    after: a\n    run: echo {b}\n---\n")
    };
    let Some((h, mut c)) = start(tmp.path(), "edited", &md("b-old")).await else {
        return;
    };
    approve_and_run(&mut c, "edited").await;
    wait_firing(&mut c, "edited", |f| step(f, "a").status == StepStatus::Running).await;
    write(&h.home, "edited", &[("flow.md", &md("b-new"))]);
    let r: Reloaded = c.ok("flow.reload", json!(null)).await;
    assert_eq!(r.changed, ["edited"], "{r:?}");
    let _: Flow = c.ok("flow.approve", json!({ "name": "edited" })).await;
    let f = wait_firing(&mut c, "edited", ended).await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    let b: Run = c.get(step(&f, "b").run.as_deref().unwrap()).await;
    assert!(b.argv.iter().any(|a| a.contains("b-old")), "{:?}", b.argv);
    h.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_step_runs_with_the_flow_folder_on_its_path_and_its_names_in_env() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    write(
        &home,
        "greet",
        &[
            ("flow.md", "---\nsteps:\n  say:\n    run: hello.sh\n---\n"),
            (
                "hello.sh",
                "#!/bin/sh\necho \"$RELAY_FLOW $RELAY_STEP $RELAY_FIRING\"\n",
            ),
        ],
    );
    let script = home.flow_dir("greet").join("hello.sh");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let Some(h) = Harness::on(home.clone()) else { return };
    let mut c = Conn::open(&h.home).await;
    approve_and_run(&mut c, "greet").await;
    let f = wait_firing(&mut c, "greet", ended).await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    let run = step(&f, "say").run.clone().unwrap();
    wait_journal(&home, &run, &format!("greet say {}", f.id)).await;
    h.stop().await;
}

#[tokio::test]
async fn a_step_that_cannot_start_is_said_and_fails_the_firing() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: echo hi\n    cwd: not-there\n  b:\n    after: a\n    run: echo never\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "lost", md).await else {
        return;
    };
    approve_and_run(&mut c, "lost").await;
    let f = wait_firing(&mut c, "lost", ended).await;
    assert_eq!(f.status, FiringStatus::Failed, "{f:?}");
    assert_eq!(
        (step(&f, "a").status, step(&f, "a").run.as_deref()),
        (StepStatus::Failed, None)
    );
    assert_eq!(step(&f, "b").status, StepStatus::Skipped);
    let events: Vec<Event> = c.ok("event.list", json!({ "kinds": ["flow.skipped"] })).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let data = events[0].data.clone().unwrap();
    assert_eq!(
        (data["firing"].as_str(), data["step"].as_str()),
        (Some(f.id.as_str()), Some("a"))
    );
    assert!(data["reason"].as_str().unwrap().contains("not-there"), "{data}");
    h.stop().await;
}

#[tokio::test]
async fn a_flow_still_firing_is_not_fired_again() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: sleep 1\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "busy", md).await else {
        return;
    };
    let first = approve_and_run(&mut c, "busy").await;
    let res = c.call("flow.run", json!({ "name": "busy" })).await;
    let err = res.error.expect("the first is still going");
    assert!(err.message.contains("still firing"), "{err:?}");
    let events: Vec<Event> = c.ok("event.list", json!({ "kinds": ["flow.skipped"] })).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let data = events[0].data.clone().unwrap();
    assert_eq!(
        (data["firing"].as_str(), data["reason"].as_str()),
        (Some(first.id.as_str()), Some("overlap"))
    );
    let f = wait_firing(&mut c, "busy", ended).await;
    assert_eq!((f.id, f.status), (first.id, FiringStatus::Done));
    h.stop().await;
}

#[tokio::test]
async fn overlap_kill_stops_the_last_firing_and_fires() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\noverlap: kill\nsteps:\n  a:\n    run: sleep 30\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "eager", md).await else {
        return;
    };
    let first = approve_and_run(&mut c, "eager").await;
    wait_firing(&mut c, "eager", |f| step(f, "a").run.is_some()).await;
    let second: Firing = c.ok("flow.run", json!({ "name": "eager" })).await;
    assert_ne!(second.id, first.id);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": "eager" })).await;
        let old = firings.iter().find(|f| f.id == first.id).unwrap();
        if old.ended_at.is_some() {
            assert_eq!(old.status, FiringStatus::Stopped, "{old:?}");
            break;
        }
        assert!(Instant::now() < deadline, "the first firing never stopped: {old:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let now: Vec<Firing> = c.ok("flow.firings", json!({ "flow": "eager" })).await;
    assert_eq!(
        (now[0].id.as_str(), now[0].status),
        (second.id.as_str(), FiringStatus::Running)
    );
    let run = step(&now[0], "a").run.clone().unwrap();
    let _: Run = c.ok("run.stop", json!({ "id": run })).await;
    h.stop().await;
}

/// The runs of `flow`'s step `step` in firing `firing`, archived or not.
async fn runs_of(c: &mut Conn, firing: &str, flow: &str, step: &str) -> Vec<Run> {
    let cause = format!("flow:{flow}/{step}");
    let runs: Vec<Run> = c.ok("run.list", json!({ "cause": cause, "archived": true })).await;
    runs.into_iter().filter(|r| r.chain == firing).collect()
}

/// The machine goes: run `id`'s holder, and its child with it if `child`, with no exit on
/// record unless the holder wrote one.
#[cfg(unix)]
async fn kill_holder(home: &Home, id: &str, child: bool) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use rl_ledger::{Ledger, sessions};
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let id = id.to_string();
    let row = ledger.call_blocking(move |db| sessions::get(db, &id)).unwrap().unwrap();
    kill(Pid::from_raw(row.holder_pid as i32), Signal::SIGKILL).unwrap();
    for _ in 0..100 {
        if kill(Pid::from_raw(row.holder_pid as i32), None).is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if child {
        let _ = kill(Pid::from_raw(row.pid as i32), Signal::SIGKILL);
    }
}

/// `a` ends while Relay is away. Coming back, the firing is carried on before the socket
/// answers, and the events loop seeing `a`'s end as well starts nothing more.
#[cfg(unix)]
#[tokio::test]
async fn a_restart_mid_firing_carries_on_once() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: sleep 1\n  b:\n    after: a\n    run: echo b\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "x", md).await else {
        return;
    };
    let fired = approve_and_run(&mut c, "x").await;
    let f = wait_firing(&mut c, "x", |f| step(f, "a").run.is_some()).await;
    drop(c);
    let home = h.home.clone();
    h.stop().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    // Its holder goes too, its exit recorded: the supervisor closes it as it comes back.
    kill_holder(&home, step(&f, "a").run.as_deref().unwrap(), false).await;

    let h = Harness::on(home).unwrap();
    let mut c = Conn::open(&h.home).await;
    let first: Vec<Firing> = c.ok("flow.firings", json!({ "flow": "x" })).await;
    assert_eq!(step(&first[0], "a").status, StepStatus::Done, "{first:?}");
    let f = wait_firing(&mut c, "x", ended).await;
    assert_eq!(
        (f.id.as_str(), f.status),
        (fired.id.as_str(), FiringStatus::Done),
        "{f:?}"
    );
    assert_eq!(step(&f, "b").status, StepStatus::Done);
    // Long enough for the events loop to have seen `a`'s end too.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let b = runs_of(&mut c, &f.id, "x", "b").await;
    assert_eq!(b.len(), 1, "b runs once: {b:?}");
    h.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_lost_step_waits_and_resume_picks_it_up_in_the_same_firing() {
    use rl_proto::methods::Resumed;
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  a:\n    run: \"echo $RELAY_STEP in $RELAY_FIRING; sleep 30\"\n  b:\n    after: a\n    run: echo b\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "far", md).await else {
        return;
    };
    let fired = approve_and_run(&mut c, "far").await;
    let f = wait_firing(&mut c, "far", |f| step(f, "a").run.is_some()).await;
    let old = step(&f, "a").run.clone().unwrap();
    drop(c);
    let home = h.home.clone();
    h.stop().await;

    kill_holder(&home, &old, true).await;

    let h = Harness::on(home).unwrap();
    let mut c = Conn::open(&h.home).await;
    let f = wait_firing(&mut c, "far", |f| step(f, "a").status == StepStatus::Lost).await;
    assert_eq!(
        (f.id.as_str(), f.status),
        (fired.id.as_str(), FiringStatus::Waiting),
        "{f:?}"
    );
    assert_eq!(step(&f, "b").status, StepStatus::Pending);

    let out: Vec<Resumed> = c.ok("run.resume", json!({ "ids": [old] })).await;
    let new = out[0].run.clone().expect("the step is picked back up");
    assert_eq!(new.chain, f.id, "in the same firing's chain");
    assert_eq!(
        new.cause,
        Cause::Flow {
            flow: "far".into(),
            step: "a".into()
        }
    );
    let f = wait_firing(&mut c, "far", |f| step(f, "a").status == StepStatus::Running).await;
    assert_eq!(f.status, FiringStatus::Running);
    assert_eq!(step(&f, "a").run.as_deref(), Some(new.id.as_str()), "{f:?}");
    wait_journal(&h.home, &new.id, &format!("a in {}", f.id)).await;

    // The run that picks it up is the step's: its end carries the firing on.
    let _: Run = c.ok("run.stop", json!({ "id": new.id })).await;
    let f = wait_firing(&mut c, "far", ended).await;
    assert_eq!(f.status, FiringStatus::Stopped, "{f:?}");
    h.stop().await;
}

#[tokio::test]
async fn a_question_never_raised_is_raised_at_a_restart() {
    use rl_ledger::Ledger;
    use rl_proto::decision::Decision;
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\nsteps:\n  review:\n    ask: sure?\n    options: { ok: {} }\n---\n";
    let Some((h, mut c)) = start(tmp.path(), "q", md).await else {
        return;
    };
    approve_and_run(&mut c, "q").await;
    let f = wait_firing(&mut c, "q", |f| step(f, "review").decision.is_some()).await;
    let was = step(&f, "review").decision.clone().unwrap();
    drop(c);
    let home = h.home.clone();
    h.stop().await;

    // As if Relay went between saving the step waiting and raising its question.
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let (firing, d) = (f.id.clone(), was.clone());
    ledger
        .call_blocking(move |db| {
            db.execute("UPDATE steps SET decision = NULL WHERE firing = ?1", [&firing])?;
            db.execute("DELETE FROM decisions WHERE id = ?1", [&d])?;
            Ok(())
        })
        .unwrap();
    drop(ledger);

    let h = Harness::on(home).unwrap();
    let mut c = Conn::open(&h.home).await;
    let f = wait_firing(&mut c, "q", |f| step(f, "review").decision.is_some()).await;
    assert_eq!(f.status, FiringStatus::Waiting, "{f:?}");
    let open: Vec<Decision> = c.ok("decision.list", json!({})).await;
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(step(&f, "review").decision.as_deref(), Some(open[0].id.as_str()));
    assert_ne!(open[0].id, was);
    assert_eq!(open[0].title, "sure?");
    h.stop().await;
}
