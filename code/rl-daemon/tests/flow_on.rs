//! A flow fired by `on`: an event it hears, narrowed by `where`, fires it where
//! the event happened, with the event in hand — and never one of its own.

mod common;
use common::{Conn, Harness, wait_journal};
use rl_core::Home;
use rl_proto::decision::Decision;
use rl_proto::flow::{Firing, FiringStatus, Flow, StepState, StepStatus};
use rl_proto::types::{Event, Run};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A daemon on a fresh home with each flow's `flow.md` written and approved; `None` when
/// `rl-session` is not built.
async fn start(tmp: &Path, flows: &[(&str, &str)]) -> Option<(Harness, Conn)> {
    let home = Home::at(tmp.join("home"));
    for (name, md) in flows {
        let dir = home.flow_dir(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("flow.md"), md).unwrap();
    }
    let h = Harness::on(home)?;
    let mut c = Conn::open(&h.home).await;
    for (name, _) in flows {
        let _: Flow = common::approve(&mut c, name).await;
    }
    Some((h, c))
}

/// Polls `flow.firings` every 50 ms for 10 s until the newest firing has ended.
async fn wait_ended(c: &mut Conn, flow: &str) -> Firing {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": flow })).await;
        if let Some(f) = firings.into_iter().next() {
            if f.ended_at.is_some() {
                return f;
            }
            assert!(Instant::now() < deadline, "the firing never ended: {f:?}");
        } else {
            assert!(Instant::now() < deadline, "`{flow}` never fired");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Every firing of `flow`, after giving the loop time to fire one more if it were going to.
async fn settled(c: &mut Conn, flow: &str) -> Vec<Firing> {
    tokio::time::sleep(Duration::from_millis(1500)).await;
    c.ok("flow.firings", json!({ "flow": flow })).await
}

fn step<'a>(f: &'a Firing, name: &str) -> &'a StepState {
    f.steps.iter().find(|s| s.name == name).unwrap()
}

async fn emit(c: &mut Conn, kind: &str, ws: &Path, summary: &str, data: serde_json::Value) {
    let res = c
        .call(
            "event.emit",
            json!({ "kind": kind, "workspace": ws, "summary": summary, "data": data }),
        )
        .await;
    assert!(res.error.is_none(), "{:?}", res.error);
}

#[tokio::test]
async fn on_fires_where_the_event_happened_with_the_event_in_hand() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let ws = std::fs::canonicalize(&ws).unwrap();
    let md = "---\non: logs.issue\nwhere: { data: { sev: [high] } }\nsteps:\n  \
              a:\n    run: echo \"$RELAY_EVENT_KIND $RELAY_EVENT_ID\"\n---\n";
    let Some((h, mut c)) = start(tmp.path(), &[("watch", md)]).await else {
        return;
    };
    let _: serde_json::Value = c.ok("workspace.up", json!({ "path": ws })).await;
    emit(&mut c, "logs.issue", &ws, "a little", json!({ "sev": "low" })).await;
    emit(&mut c, "logs.issue", &ws, "a lot", json!({ "sev": "high" })).await;
    let f = wait_ended(&mut c, "watch").await;
    assert_eq!((f.trigger.as_str(), f.status), ("on", FiringStatus::Done), "{f:?}");
    let heard: Vec<Event> = c.ok("event.list", json!({ "kinds": ["logs.issue"] })).await;
    let high = heard.iter().find(|e| e.summary == "a lot").unwrap();
    assert_eq!(f.event, Some(high.id), "the high one fired it, the low one did not");
    let run: Run = c.get(step(&f, "a").run.as_deref().unwrap()).await;
    assert_eq!(run.cwd, ws, "where the event happened");
    wait_journal(&h.home, &run.id, &format!("logs.issue {}", high.id)).await;
    assert_eq!(settled(&mut c, "watch").await.len(), 1);
    h.stop().await;
}

#[tokio::test]
async fn a_flow_never_fires_on_its_own_doing() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\non: run.finished\nthrottle: 0\nsteps:\n  a:\n    run: \"true\"\n---\n";
    let Some((h, mut c)) = start(tmp.path(), &[("echoes", md)]).await else {
        return;
    };
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["true"], "cwd": tmp.path(), "title": "plain" }),
        )
        .await;
    let f = wait_ended(&mut c, "echoes").await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    let fired_by: Event = c
        .ok::<Vec<Event>>("event.list", json!({ "kinds": ["run.finished"] }))
        .await
        .into_iter()
        .find(|e| Some(e.id) == f.event)
        .expect("a run.finished fired it");
    assert_eq!(fired_by.run.as_deref(), Some(run.id.as_str()));
    assert_eq!(
        settled(&mut c, "echoes").await.len(),
        1,
        "its own step's run.finished, and its flow.* events, do not fire it"
    );
    h.stop().await;
}

#[tokio::test]
async fn a_failure_in_one_flow_fires_another() {
    let tmp = tempfile::tempdir().unwrap();
    let nightly = "---\nsteps:\n  a:\n    run: exit 1\n---\n";
    let notify = "---\non: run.failed\nwhere: { cause: \"flow:nightly\" }\nsteps:\n  \
                  tell:\n    run: echo told\n---\n";
    let Some((h, mut c)) = start(tmp.path(), &[("nightly", nightly), ("notify", notify)]).await else {
        return;
    };
    let none: Vec<Firing> = c.ok("flow.firings", json!({ "flow": "notify" })).await;
    assert!(none.is_empty());
    let _: Firing = c.ok("flow.run", json!({ "name": "nightly" })).await;
    let f = wait_ended(&mut c, "notify").await;
    assert_eq!((f.trigger.as_str(), f.status), ("on", FiringStatus::Done), "{f:?}");
    wait_journal(&h.home, step(&f, "tell").run.as_deref().unwrap(), "told").await;
    let n = wait_ended(&mut c, "nightly").await;
    assert_eq!(n.status, FiringStatus::Failed);
    assert_eq!(settled(&mut c, "notify").await.len(), 1);
    h.stop().await;
}

#[tokio::test]
async fn an_unapproved_or_paused_flow_does_not_hear() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\non: logs.issue\nthrottle: 0\nsteps:\n  a:\n    run: \"true\"\n---\n";
    let Some((h, mut c)) = start(tmp.path(), &[("deaf", md)]).await else {
        return;
    };
    let _: Flow = c.ok("flow.pause", json!({ "name": "deaf" })).await;
    emit(&mut c, "logs.issue", tmp.path(), "paused", json!({})).await;
    assert!(settled(&mut c, "deaf").await.is_empty());
    let _: Flow = c.ok("flow.resume", json!({ "name": "deaf" })).await;
    let dir = h.home.flow_dir("deaf");
    std::fs::write(dir.join("flow.md"), md.replace("true", "echo changed")).unwrap();
    let _: serde_json::Value = c.ok("flow.reload", json!(null)).await;
    emit(&mut c, "logs.issue", tmp.path(), "unapproved", json!({})).await;
    assert!(settled(&mut c, "deaf").await.is_empty());
    let _: Flow = common::approve(&mut c, "deaf").await;
    emit(&mut c, "logs.issue", tmp.path(), "heard", json!({})).await;
    let f = wait_ended(&mut c, "deaf").await;
    let heard: Vec<Event> = c.ok("event.list", json!({ "kinds": ["logs.issue"] })).await;
    assert_eq!(f.event, heard.iter().find(|e| e.summary == "heard").map(|e| e.id));
    h.stop().await;
}

/// `{event.*}` fills an `ask:`'s text, and an `if:` reads the event too.
#[tokio::test]
async fn a_question_says_what_the_event_said() {
    let tmp = tempfile::tempdir().unwrap();
    let md = "---\non: logs.issue\nsteps:\n  \
              loud:\n    if: event.data.sev == high\n    run: \"true\"\n  \
              quiet:\n    if: event.data.sev == low\n    run: \"true\"\n  \
              review:\n    after: loud\n    ask: \"{event.summary} ({event.data.sev}, {event.kind})?\"\n    \
              options: { ok: {} }\n---\n";
    let Some((h, mut c)) = start(tmp.path(), &[("asker", md)]).await else {
        return;
    };
    emit(&mut c, "logs.issue", tmp.path(), "disk full", json!({ "sev": "high" })).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    let d = loop {
        let open: Vec<Decision> = c.ok("decision.list", json!({})).await;
        if let Some(d) = open.into_iter().next() {
            break d;
        }
        assert!(Instant::now() < deadline, "never asked");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(d.title, "disk full (high, logs.issue)?");
    let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": "asker" })).await;
    assert_eq!(step(&firings[0], "loud").status, StepStatus::Done);
    assert_eq!(step(&firings[0], "quiet").status, StepStatus::Skipped);
    h.stop().await;
}

/// The `rl` binary sits beside `rld` in the target dir.
fn rl_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rld")).with_file_name("rl");
    p.is_file().then_some(p)
}

/// The wiring end to end on Relay's own primitives: a probe `rl emit`s an
/// alert through the real CLI, a flow narrows by `where.data` and asks, and
/// the yes acts where the alert was raised. The probe's alert is canned, so
/// no network is needed.
#[tokio::test]
async fn a_probe_its_event_a_flow_that_asks_and_the_yes_that_acts() {
    let Some(rl) = rl_bin() else {
        eprintln!("skipping: rl not built (run `cargo build --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let ws = std::fs::canonicalize(&ws).unwrap();
    let probe = format!(
        "---\nin: {}\nsteps:\n  look:\n    run: {} emit weather.alert --data level=storm\n---\n",
        ws.display(),
        rl.display()
    );
    let alert = "---\non: weather.alert\nwhere: { data: { level: [storm] } }\nsteps:\n  \
                 review:\n    ask: \"storm — tell everyone?\"\n    \
                 options: { tell: { run: \"echo told > told.txt\" } }\n---\n";
    let home = Home::at(tmp.path().join("home"));
    for (name, md) in [("probe", probe.as_str()), ("alert", alert)] {
        let dir = home.flow_dir(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("flow.md"), md).unwrap();
    }
    let Some(h) = Harness::on(home) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&h.home).await;
    // The probe runs in a folder Relay knows, so it loads once that is so.
    let _: serde_json::Value = c.ok("workspace.up", json!({ "path": ws })).await;
    let _: serde_json::Value = c.ok("flow.reload", json!(null)).await;
    for name in ["probe", "alert"] {
        let _: Flow = common::approve(&mut c, name).await;
    }

    let _: Firing = c.ok("flow.run", json!({ "name": "probe" })).await;
    let p = wait_ended(&mut c, "probe").await;
    assert_eq!(p.status, FiringStatus::Done, "the probe (rl emit) ran clean: {p:?}");

    // The alert reached the flow, which asked.
    let deadline = Instant::now() + Duration::from_secs(10);
    let q = loop {
        let open: Vec<Decision> = c.ok("decision.list", json!({})).await;
        if let Some(q) = open.into_iter().next() {
            break q;
        }
        assert!(Instant::now() < deadline, "the alert flow never asked");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(q.title, "storm — tell everyone?");
    let waiting: Vec<Firing> = c.ok("flow.firings", json!({ "flow": "alert" })).await;
    assert_eq!(
        (waiting[0].trigger.as_str(), waiting[0].status),
        ("on", FiringStatus::Waiting)
    );

    // The yes acts, in the folder the alert was raised in.
    let _: Decision = c.ok("decision.take", json!({ "id": q.id, "options": ["tell"] })).await;
    let f = wait_ended(&mut c, "alert").await;
    assert_eq!(f.status, FiringStatus::Done, "{f:?}");
    assert_eq!(std::fs::read_to_string(ws.join("told.txt")).unwrap().trim(), "told");
    h.stop().await;
}
