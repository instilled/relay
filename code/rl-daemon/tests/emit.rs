//! `event.emit`: a command or plugin puts a custom event into the ledger, a
//! reaction picks it up, and `where.data` narrows by the payload. Reserved
//! kinds are refused so an emit cannot forge one of the daemon's own.

mod common;

use common::{Conn, Harness};
use rl_core::Home;
use rl_proto::types::Event;
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

async fn wait_for(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.trim().is_empty()
        {
            return text;
        }
        assert!(Instant::now() < deadline, "{} never appeared", path.display());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn an_emitted_event_feeds_a_reaction_and_where_data_narrows() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::write(
        home.reactions_file(),
        format!(
            "reactions:\n  \
               triage:\n    \
                 on: log.issue\n    \
                 where: {{ data: {{ severity: [ERROR, WARNING] }} }}\n    \
                 run: 'echo \"{{data.msg}}\" >> caught.txt'\n    \
                 throttle: '0'\n    \
                 cwd: {}\n",
            dir.display()
        ),
    )
    .unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    // An INFO issue: the rule's `where` should pass it by.
    let quiet = c
        .call(
            "event.emit",
            json!({ "kind": "log.issue", "workspace": dir, "data": { "severity": "INFO", "msg": "nothing" } }),
        )
        .await;
    assert!(quiet.error.is_none(), "emit failed: {:?}", quiet.error);

    // An ERROR issue: this one fires the rule.
    let loud = c
        .call(
            "event.emit",
            json!({ "kind": "log.issue", "workspace": dir, "data": { "severity": "ERROR", "msg": "disk full" } }),
        )
        .await;
    assert!(loud.error.is_none(), "emit failed: {:?}", loud.error);

    // Only the ERROR one reached the rule, wearing its payload.
    let caught = wait_for(&dir.join("caught.txt")).await;
    assert_eq!(
        caught.trim(),
        "disk full",
        "where.data let ERROR through and filled {{data.msg}}"
    );

    // Both events are in the ledger, with their data.
    let events: Vec<Event> = c.ok("event.list", json!({ "limit": 50 })).await;
    let issues: Vec<&Event> = events.iter().filter(|e| e.kind == "log.issue").collect();
    assert_eq!(issues.len(), 2, "both emits landed");
    assert!(
        issues.iter().any(|e| e.data.as_ref().unwrap()["severity"] == "ERROR"),
        "the payload is kept"
    );

    // A reserved kind is the daemon's alone; an emit cannot forge one.
    let forged = c
        .call("event.emit", json!({ "kind": "run.failed", "workspace": dir }))
        .await;
    assert!(forged.error.is_some(), "run.failed must be refused");

    // Words may be hyphenated, as rules are named; not every shape goes.
    for ok in ["flint.product.new-feedback", "log.issue_2"] {
        let r = c.call("event.emit", json!({ "kind": ok, "workspace": dir })).await;
        assert!(r.error.is_none(), "{ok}: {:?}", r.error);
    }
    for bad in [
        "nodot",
        "log.-issue",
        "log..issue",
        "Log.issue",
        "log.1issue",
        "log.is sue",
    ] {
        let r = c.call("event.emit", json!({ "kind": bad, "workspace": dir })).await;
        assert!(r.error.is_some(), "{bad} must be refused");
    }

    h.stop().await;
}

/// `event.list` newest first, a page at a time: `before` goes on past the
/// last one read, `after` picks up what came since — the events view's
/// paging and its tick.
#[tokio::test]
async fn event_list_pages_newest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    for n in 0..7 {
        let r = c
            .call(
                "event.emit",
                json!({ "kind": "page.test", "workspace": dir, "summary": format!("n{n}") }),
            )
            .await;
        assert!(r.error.is_none(), "emit failed: {:?}", r.error);
    }
    let said = |evs: &[Event]| evs.iter().map(|e| e.summary.clone()).collect::<Vec<_>>();
    let page = |extra: serde_json::Value| {
        let mut p = json!({ "kinds": ["page.test"], "newest_first": true, "limit": 3 });
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        p
    };

    let first: Vec<Event> = c.ok("event.list", page(json!({}))).await;
    assert_eq!(said(&first), ["n6", "n5", "n4"], "the newest, newest first");
    let second: Vec<Event> = c.ok("event.list", page(json!({ "before": first[2].id }))).await;
    assert_eq!(said(&second), ["n3", "n2", "n1"], "on from where the first page ended");
    let last: Vec<Event> = c.ok("event.list", page(json!({ "before": second[2].id }))).await;
    assert_eq!(said(&last), ["n0"], "a short page: nothing older");

    // What came since the newest one read, and nothing else.
    let r = c
        .call(
            "event.emit",
            json!({ "kind": "page.test", "workspace": dir, "summary": "n7" }),
        )
        .await;
    assert!(r.error.is_none());
    let since: Vec<Event> = c.ok("event.list", page(json!({ "after": first[0].id }))).await;
    assert_eq!(said(&since), ["n7"]);

    // Without the flag, as ever: oldest first.
    let old: Vec<Event> = c.ok("event.list", json!({ "kinds": ["page.test"], "limit": 2 })).await;
    assert_eq!(said(&old), ["n0", "n1"]);
    h.stop().await;
}
