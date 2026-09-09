//! The scheduler against a real daemon: `schedules.yaml` fires runs with a
//! cause, the policies hold, pause/resume/run-now/reload do what they say,
//! and a firing missed while the daemon was away follows the declared policy.

mod common;

use common::{Conn, Harness};
use jiff::Timestamp;
use relay_core::Home;
use relay_ledger::{Ledger, schedules};
use relay_proto::schedule::{ReloadReport, Schedule};
use relay_proto::types::{Event, Run};
use serde_json::json;
use std::time::Duration;

fn write(home: &Home, yaml: &str) {
    std::fs::write(home.schedules_file(), yaml).unwrap();
}

async fn runs_of(c: &mut Conn, name: &str) -> Vec<Run> {
    c.ok("run.list", json!({ "cause": format!("schedule:{name}"), "limit": 50 }))
        .await
}

async fn events_of_kind(c: &mut Conn, kind: &str) -> Vec<Event> {
    c.ok("event.list", json!({ "kinds": [kind] })).await
}

async fn by_name(c: &mut Conn, name: &str) -> Schedule {
    let list: Vec<Schedule> = c.ok("schedule.list", json!({ "all": true })).await;
    list.into_iter().find(|s| s.name == name).expect("schedule listed")
}

#[tokio::test]
async fn fires_on_time_and_obeys_pause_resume_and_run_now() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    home.ensure_dirs().unwrap();
    write(
        &home,
        &format!(
            "schedules:\n  tick:\n    every: 1s\n    run: [sh, -c, 'echo tick']\n    cwd: {}\n    tags: [t]\n    ref: gh:1\n",
            tmp.path().display()
        ),
    );
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    tokio::time::sleep(Duration::from_millis(2500)).await;
    let runs = runs_of(&mut c, "tick").await;
    assert!(!runs.is_empty(), "nothing fired");
    let r = &runs[0];
    assert_eq!(r.cause.to_string(), "schedule:tick");
    assert_eq!(r.flow, r.id, "a scheduled run starts its own flow");
    assert_eq!(r.title, "tick");
    assert_eq!(r.tags, ["t"]);
    assert_eq!(r.ref_.as_deref(), Some("gh:1"));
    let fired = events_of_kind(&mut c, "schedule.fired").await;
    assert!(!fired.is_empty());
    assert_eq!(fired[0].data.as_ref().unwrap()["schedule"], "tick");
    assert!(
        runs.iter().any(|r| r.id == fired[0].run.clone().unwrap_or_default()),
        "the fired event names one of the runs"
    );
    let s = by_name(&mut c, "tick").await;
    assert!(s.last_run.is_some() && s.next_at.is_some() && !s.paused);
    assert_eq!(s.spec.when(), "every 1s");

    // Paused: nothing new. Resumed: it goes on.
    let s: Schedule = c.ok("schedule.pause", json!({ "name": "tick" })).await;
    assert!(s.paused);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = runs_of(&mut c, "tick").await.len();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(runs_of(&mut c, "tick").await.len(), before, "fired while paused");
    let s: Schedule = c.ok("schedule.resume", json!({ "name": "tick" })).await;
    assert!(!s.paused && s.next_at.is_some());
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(runs_of(&mut c, "tick").await.len() > before, "did not resume");

    // Run now, even while paused.
    c.ok::<Schedule>("schedule.pause", json!({ "name": "tick" })).await;
    let now: Run = c.ok("schedule.run", json!({ "name": "tick" })).await;
    assert_eq!(now.cause.to_string(), "schedule:tick");
    let res = c.call("schedule.run", json!({ "name": "nope" })).await;
    assert!(res.error.is_some());

    for r in runs_of(&mut c, "tick").await {
        c.wait_terminal(&r.id).await;
    }
    h.stop().await;
}

#[tokio::test]
async fn overlap_skip_keeps_one_run_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    home.ensure_dirs().unwrap();
    write(
        &home,
        &format!(
            "schedules:\n  long:\n    every: 1s\n    run: [sleep, '30']\n    cwd: {}\n",
            tmp.path().display()
        ),
    );
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let runs = runs_of(&mut c, "long").await;
    assert_eq!(runs.len(), 1, "overlap: skip should keep one run: {runs:?}");
    let skipped = events_of_kind(&mut c, "schedule.skipped").await;
    assert!(!skipped.is_empty(), "no skipped event");
    assert_eq!(skipped[0].data.as_ref().unwrap()["reason"], "overlap");
    assert_eq!(skipped[0].group_key, tmp.path().to_string_lossy());
    c.ok::<Run>("run.stop", json!({ "id": runs[0].id })).await;
    c.wait_terminal(&runs[0].id).await;
    h.stop().await;
}

#[tokio::test]
async fn a_missed_firing_follows_its_policy_on_start() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    home.ensure_dirs().unwrap();
    write(
        &home,
        &format!(
            "schedules:\n  late-skip:\n    every: 1h\n    run: [true]\n    cwd: {d}\n  late-catch:\n    every: 1h\n    run: [true]\n    cwd: {d}\n    missed: catch-up-once\n",
            d = tmp.path().display()
        ),
    );
    // As the previous daemon left them: due ten minutes ago.
    let path = home.schedules_file();
    let (loaded, errors) = rl_daemon::schedule::load_file(&path).unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let now = Timestamp::now();
    let due = now - jiff::SignedDuration::from_mins(10);
    ledger
        .call_blocking(move |c| {
            schedules::sync(c, "global", &path, &loaded, now)?;
            schedules::set_next(c, "global", "late-skip", Some(due))?;
            schedules::set_next(c, "global", "late-catch", Some(due))
        })
        .unwrap();
    drop(ledger);

    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(runs_of(&mut c, "late-catch").await.len(), 1, "catch-up-once fires once");
    assert!(runs_of(&mut c, "late-skip").await.is_empty(), "skip does not fire");
    let skipped = events_of_kind(&mut c, "schedule.skipped").await;
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].data.as_ref().unwrap()["schedule"], "late-skip");
    assert_eq!(skipped[0].data.as_ref().unwrap()["reason"], "missed");
    for name in ["late-skip", "late-catch"] {
        let s = by_name(&mut c, name).await;
        let next = s.next_at.expect("planned again");
        let ahead = next.duration_since(Timestamp::now()).as_secs();
        assert!((3500..=3600).contains(&ahead), "{name}: next in {ahead}s");
    }
    for r in runs_of(&mut c, "late-catch").await {
        c.wait_terminal(&r.id).await;
    }
    h.stop().await;
}

#[tokio::test]
async fn reload_is_a_diff_and_reports_what_did_not_load() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    home.ensure_dirs().unwrap();
    let d = tmp.path().display().to_string();
    write(
        &home,
        &format!(
            "schedules:\n  good:\n    every: 1h\n    run: [true]\n    cwd: {d}\n  bad:\n    every: 1h\n    run: [true]\n"
        ),
    );
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    let r: ReloadReport = c.ok("schedule.reload", json!(null)).await;
    assert_eq!(r.loaded, 1);
    assert_eq!(r.errors.len(), 1);
    assert!(
        r.errors[0].starts_with("bad: ") && r.errors[0].contains("cwd"),
        "{:?}",
        r.errors
    );
    let good = by_name(&mut c, "good").await;
    assert!(good.next_at.is_some());
    assert_eq!(good.source, home.schedules_file());

    write(
        &home,
        &format!("schedules:\n  good:\n    every: 2h\n    run: [true]\n    cwd: {d}\n"),
    );
    let r: ReloadReport = c.ok("schedule.reload", json!(null)).await;
    assert_eq!(r.changed, ["good"]);
    let changed = by_name(&mut c, "good").await;
    assert_ne!(changed.hash, good.hash);
    assert_eq!(changed.spec.every.as_deref(), Some("2h"));

    write(&home, "schedules: {}\n");
    let r: ReloadReport = c.ok("schedule.reload", json!(null)).await;
    assert_eq!(r.removed, ["good"]);
    let visible: Vec<Schedule> = c.ok("schedule.list", json!({})).await;
    assert!(visible.is_empty());
    assert!(by_name(&mut c, "good").await.removed_at.is_some());
    let res = c.call("schedule.run", json!({ "name": "good" })).await;
    assert!(res.error.is_some(), "a removed schedule does not fire");

    // A file that does not parse at all is an error, not a wipe.
    write(&home, "schedules: [not, a, map]\n");
    let res = c.call("schedule.reload", json!(null)).await;
    assert!(res.error.is_some());
    h.stop().await;
}
