//! Rules against a real daemon: schedules and reactions come back as one
//! list, and one rule can be approved by name — after which it fires.

mod common;

use common::{Conn, Harness};
use rl_core::Home;
use rl_proto::rule::{Rule, RuleKind, RuleStatus};
use rl_proto::schedule::UpReport;
use rl_proto::types::Run;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn one_list_and_a_yes_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    std::fs::write(
        home.reactions_file(),
        "reactions:\n  tell-me:\n    on: run.failed\n    run: 'true'\n    cwd: /tmp\n",
    )
    .unwrap();
    let ws = tmp.path().join("proj");
    std::fs::create_dir_all(ws.join(".relay")).unwrap();
    let ws = std::fs::canonicalize(&ws).unwrap();
    std::fs::write(
        ws.join(".relay/schedules.yaml"),
        "schedules:\n  tick:\n    every: 1s\n    run: [sh, -c, 'echo tick > seen.txt']\n",
    )
    .unwrap();
    std::fs::write(
        ws.join(".relay/reactions.yaml"),
        "reactions:\n  tell-me:\n    on: run.finished\n    run: 'true'\n",
    )
    .unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    let _: UpReport = c.ok("workspace.up", json!({ "path": ws })).await;

    // Both files, one list; the workspace's wait for a yes, yours does not.
    let rules: Vec<Rule> = c.ok("rule.list", json!({})).await;
    let mut seen: Vec<(RuleKind, String, String, RuleStatus)> = rules
        .iter()
        .map(|r| (r.kind(), r.scope_label(), r.name().to_string(), r.status()))
        .collect();
    seen.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)).then_with(|| a.0.cmp(&b.0)));
    assert_eq!(
        seen,
        [
            (RuleKind::Reaction, "global".into(), "tell-me".into(), RuleStatus::On),
            (
                RuleKind::Reaction,
                "proj".into(),
                "tell-me".into(),
                RuleStatus::NeedsApproval
            ),
            (
                RuleKind::Schedule,
                "proj".into(),
                "tick".into(),
                RuleStatus::NeedsApproval
            ),
        ]
    );

    // A name in two files needs the scope; a name that is both kinds needs the kind.
    let res = c.call("rule.approve", json!({ "name": "tell-me" })).await;
    let msg = res.error.expect("ambiguous").message;
    assert!(msg.contains("exists in"), "{msg}");
    let res = c.call("rule.approve", json!({ "name": "nope" })).await;
    assert!(res.error.is_some());

    // One yes, by name; the schedule then fires.
    let r: Rule = c.ok("rule.approve", json!({ "name": "tick", "scope": ws })).await;
    assert_eq!(r.status(), RuleStatus::On);
    assert!(r.next_at().is_some(), "planned on approval");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let runs: Vec<Run> = c.ok("run.list", json!({ "cause": "schedule:tick", "limit": 5 })).await;
    assert!(!runs.is_empty(), "did not fire after approval");

    // The other one is still waiting: a yes is one rule at a time.
    let rules: Vec<Rule> = c.ok("rule.list", json!({})).await;
    let ws_reaction = rules
        .iter()
        .find(|r| r.kind() == RuleKind::Reaction && !r.is_global())
        .unwrap();
    assert_eq!(ws_reaction.status(), RuleStatus::NeedsApproval);
    let r: Rule = c
        .ok(
            "rule.approve",
            json!({ "name": "tell-me", "scope": ws, "kind": "reaction" }),
        )
        .await;
    assert!(r.approved());

    h.stop().await;
}
