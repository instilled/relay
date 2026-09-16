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

    h.stop().await;
}
