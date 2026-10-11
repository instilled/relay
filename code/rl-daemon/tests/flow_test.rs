//! `flow.test`: what a flow would do now — when it fires, whether an event would fire it, which
//! steps would run — told without starting anything.

mod common;
use common::{Conn, Harness};
use jiff::SignedDuration;
use rl_core::Home;
use rl_proto::decision::Decision;
use rl_proto::flow::{Firing, Held, Runs, Tested};
use rl_proto::types::{Event, Run};
use serde_json::json;
use std::path::Path;
use std::time::Duration;

/// A daemon on a fresh home with each flow's `flow.md` written; `None` when `rl-session` is not built.
async fn start(tmp: &Path, flows: &[(&str, &str)]) -> Option<(Harness, Conn)> {
    let home = Home::at(tmp.join("home"));
    for (name, md) in flows {
        let dir = home.flow_dir(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("flow.md"), md).unwrap();
    }
    let h = Harness::on(home)?;
    let c = Conn::open(&h.home).await;
    Some((h, c))
}

async fn test(c: &mut Conn, name: &str, event: Option<i64>) -> Tested {
    c.ok("flow.test", json!({ "name": name, "event": event })).await
}

/// Nothing ran: no firing of `flow`, no run, no question.
async fn nothing_started(c: &mut Conn, flow: &str) {
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": flow })).await;
    assert!(firings.is_empty(), "{firings:?}");
    let runs: Vec<Run> = c.ok("run.list", json!({ "archived": true })).await;
    assert!(runs.is_empty(), "{runs:?}");
    let asked: Vec<Decision> = c.ok("decision.list", json!({})).await;
    assert!(asked.is_empty(), "{asked:?}");
}

const LOGS: &str = "---\nwhen: every 30m\nsteps:\n  probe:\n    run: new-errors.sh\n  triage:\n    after: probe\n    \
                    if: probe.out\n    run: { agent: claude, prompt: \"look at {probe.out}\" }\n  review:\n    after: triage\n    \
                    ask: \"{triage.out}\"\n    options:\n      apply: { run: { agent: claude, prompt: do it } }\n      ignore: {}\n    \
                    default: ignore\n    expires: 4h\n---\n";

#[tokio::test]
async fn a_test_says_when_and_which_steps_and_starts_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[("logs", LOGS)]).await else {
        return;
    };
    let t = test(&mut c, "logs", None).await;
    assert_eq!(t.held, Some(Held::NeedsYes), "written by hand: it needs a yes");
    assert_eq!((t.flow.as_str(), t.when.as_str()), ("logs", "every 30m"));
    assert_eq!(t.next.len(), 3);
    assert_eq!(t.next[1].duration_since(t.next[0]), SignedDuration::from_mins(30));
    let steps: Vec<(&str, &Runs, &str)> = t
        .steps
        .iter()
        .map(|s| (s.name.as_str(), &s.runs, s.does.as_str()))
        .collect();
    assert_eq!(
        steps,
        [
            ("probe", &Runs::Yes, "new-errors.sh"),
            (
                "triage",
                &Runs::If(vec!["probe.out".into()]),
                "claude · look at {probe.out}"
            ),
            (
                "review",
                &Runs::If(vec!["probe.out".into()]),
                "asks {triage.out} · apply (claude), ignore · ignore in 4h"
            ),
        ]
    );
    assert!(
        t.steps.iter().all(|s| s.folder.as_deref() == Some(h.home.path())),
        "no `in`: the Relay home"
    );
    assert!(t.going.is_none() && t.event.is_none());

    // With its yes, nothing holds it; still nothing starts.
    let _ = common::approve(&mut c, "logs").await;
    let t = test(&mut c, "logs", None).await;
    assert_eq!(t.held, None);
    nothing_started(&mut c, "logs").await;
    h.stop().await;
}

#[tokio::test]
async fn an_event_says_whether_it_fires_the_flow_and_what_its_steps_do() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let ws = std::fs::canonicalize(&ws).unwrap();
    let Some((h, mut c)) = start(tmp.path(), &[]).await else {
        return;
    };
    // The events first, heard by no flow: the loop has read past them before the flow is there.
    for (sev, summary) in [("low", "a little"), ("high", "a lot")] {
        let res = c
            .call(
                "event.emit",
                json!({ "kind": "logs.issue", "workspace": ws, "summary": summary, "data": { "sev": sev } }),
            )
            .await;
        assert!(res.error.is_none(), "{:?}", res.error);
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let md = "---\non: logs.issue\nwhere: { data: { sev: [high, low] } }\nsteps:\n  a:\n    if: event.data.sev == high\n    \
              run: echo a\n  b:\n    after: a\n    run: echo b\n---\n";
    let dir = h.home.flow_dir("watch");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("flow.md"), md).unwrap();
    let _: serde_json::Value = c.ok("flow.reload", json!(null)).await;
    let _ = common::approve(&mut c, "watch").await;

    let heard: Vec<Event> = c.ok("event.list", json!({ "kinds": ["logs.issue"] })).await;
    let id = |summary: &str| heard.iter().find(|e| e.summary == summary).unwrap().id;
    let high = test(&mut c, "watch", Some(id("a lot"))).await;
    let e = high.event.as_ref().unwrap();
    assert!(e.fires && e.why == "it fires on it" && !e.old, "{e:?}");
    assert_eq!(high.steps[0].runs, Runs::Yes);
    assert_eq!(high.steps[1].runs, Runs::Yes);
    assert_eq!(
        high.steps[0].folder.as_deref(),
        Some(ws.as_path()),
        "where the event happened"
    );

    let low = test(&mut c, "watch", Some(id("a little"))).await;
    assert!(low.event.as_ref().unwrap().fires, "where takes low too");
    assert_eq!(
        low.steps[0].runs,
        Runs::No("if event.data.sev == high: not for this event".into())
    );
    assert_eq!(low.steps[1].runs, Runs::No("after a, which would not run".into()));

    // No event: when it fires is not known, nor where.
    let any = test(&mut c, "watch", None).await;
    assert_eq!(any.on, ["logs.issue"]);
    assert_eq!(any.steps[0].runs, Runs::If(vec!["event.data.sev == high".into()]));
    assert_eq!(any.steps[0].folder, None);
    assert!(any.next.is_empty(), "no `when`");

    // Paused, the event would not fire it, and it says why.
    let _: serde_json::Value = c.ok("flow.pause", json!({ "name": "watch" })).await;
    let paused = test(&mut c, "watch", Some(id("a lot"))).await;
    assert_eq!(paused.held, Some(Held::Paused));
    let e = paused.event.unwrap();
    assert!(!e.fires && e.why == "it is paused", "{e:?}");
    nothing_started(&mut c, "watch").await;
    h.stop().await;
}

#[tokio::test]
async fn what_cannot_be_tested_is_said() {
    let tmp = tempfile::tempdir().unwrap();
    let broken = "---\nsteps:\n  a:\n    after: nope\n    run: x\n---\n";
    let Some((h, mut c)) = start(tmp.path(), &[("broken", broken), ("logs", LOGS)]).await else {
        return;
    };
    let err = |res: rl_proto::rpc::Response| res.error.map(|e| e.message).unwrap_or_default();
    let said = err(c.call("flow.test", json!({ "name": "broken" })).await);
    assert!(said.contains("did not load") && said.contains("flow.md:3"), "{said}");
    let said = err(c.call("flow.test", json!({ "name": "ghost" })).await);
    assert!(said.contains("ghost"), "{said}");
    let said = err(c.call("flow.test", json!({ "name": "logs", "event": 999_999 })).await);
    assert_eq!(said, "invalid params: no event 999999: rl events lists them");
    h.stop().await;
}
