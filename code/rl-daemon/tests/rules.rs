//! Rules against a real daemon: schedules and reactions come back as one
//! list, and one rule can be approved by name — after which it fires.

mod common;

use common::{Conn, Harness};
use rl_core::Home;
use rl_proto::rule::{NotLoaded, Rule, RuleKind, RuleStatus};
use rl_proto::schedule::{ReloadReport, UpReport};
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

/// A file that does not parse — yours too — is reported like a folder's:
/// its rules stay as they last loaded, every other file still loads, and
/// what did not load says where (scope, file, line, column) and why. It stays
/// said, on `rule.errors`, until the file is fixed.
#[tokio::test]
async fn a_broken_file_is_said_and_the_rest_still_load() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let nightly = "schedules:\n  nightly:\n    every: 1h\n    run: 'true'\n    cwd: /tmp\n";
    std::fs::write(home.schedules_file(), nightly).unwrap();
    let ws = tmp.path().join("proj");
    std::fs::create_dir_all(ws.join(".relay")).unwrap();
    let ws = std::fs::canonicalize(&ws).unwrap();
    let schedules = ws.join(".relay/schedules.yaml");
    std::fs::write(&schedules, "schedules:\n  tick:\n    every: 1h\n    run: 'true'\n").unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    let _: UpReport = c.ok("workspace.up", json!({ "path": ws })).await;
    let errors: Vec<NotLoaded> = c.ok("rule.errors", json!(null)).await;
    assert!(errors.is_empty(), "{errors:?}");
    let before: Vec<Rule> = c.ok("rule.list", json!({})).await;
    let hash_of = |rules: &[Rule], name: &str| {
        let r = rules.iter().find(|r| r.name() == name).expect(name);
        match r {
            Rule::Schedule(s) => s.hash.clone(),
            Rule::Reaction(r) => r.hash.clone(),
        }
    };

    // Your file breaks on line 4; the folder's gains a schedule.
    std::fs::write(
        home.schedules_file(),
        "schedules:\n  nightly:\n    every: 2h\n   run: 'true'\n",
    )
    .unwrap();
    std::fs::write(
        &schedules,
        "schedules:\n  tick:\n    every: 1h\n    run: 'true'\n  tock:\n    every: 1h\n    run: 'true'\n",
    )
    .unwrap();
    let r: ReloadReport = c.ok("schedule.reload", json!(null)).await;
    assert_eq!(r.added, ["proj:tock"], "the folder's file still loads: {r:?}");
    assert_eq!(r.not_loaded.len(), 1, "{r:?}");
    let n = &r.not_loaded[0];
    assert_eq!(
        (n.scope.as_str(), n.name.as_str(), &n.file, n.rule.as_deref(), n.line),
        ("global", "global", &home.schedules_file(), None, Some(4))
    );
    assert!(n.column.is_some(), "{n:?}");
    assert!(!n.message.is_empty() && !n.message.contains("at line 4"), "{n:?}");
    assert!(
        r.errors[0].starts_with(&format!("global · schedules.yaml:4:{}: ", n.column.unwrap())),
        "{:?}",
        r.errors
    );
    let after: Vec<Rule> = c.ok("rule.list", json!({})).await;
    assert_eq!(
        hash_of(&after, "nightly"),
        hash_of(&before, "nightly"),
        "your rule keeps its last good version"
    );
    let errors: Vec<NotLoaded> = c.ok("rule.errors", json!(null)).await;
    assert_eq!(errors, r.not_loaded, "and it stays said");

    // Yours fixed but for one rule without a `cwd`; the folder's reactions
    // break. Each says whose it is.
    std::fs::write(
        home.schedules_file(),
        format!("{nightly}  bad:\n    every: 1h\n    run: 'true'\n"),
    )
    .unwrap();
    std::fs::write(ws.join(".relay/reactions.yaml"), "reactions: [\n").unwrap();
    let r: ReloadReport = c.ok("schedule.reload", json!(null)).await;
    assert_eq!(r.errors, ["global · schedules.yaml:6 · bad: needs `cwd`"]);
    let r: ReloadReport = c.ok("reaction.reload", json!(null)).await;
    assert_eq!(r.not_loaded.len(), 1, "{r:?}");
    let n = &r.not_loaded[0];
    assert_eq!((n.scope.as_str(), n.name.as_str()), (ws.to_str().unwrap(), "proj"));
    assert_eq!(
        r.errors,
        ["proj · reactions.yaml:1:12: reactions: invalid type: sequence, expected a map"]
    );
    let errors: Vec<NotLoaded> = c.ok("rule.errors", json!(null)).await;
    let mut said: Vec<(String, Option<String>)> = errors.iter().map(|e| (e.name.clone(), e.rule.clone())).collect();
    said.sort();
    assert_eq!(said, [("global".into(), Some("bad".into())), ("proj".into(), None)]);

    // All fixed: nothing more to say.
    std::fs::write(home.schedules_file(), nightly).unwrap();
    std::fs::write(ws.join(".relay/reactions.yaml"), "reactions: {}\n").unwrap();
    let _: ReloadReport = c.ok("schedule.reload", json!(null)).await;
    let _: ReloadReport = c.ok("reaction.reload", json!(null)).await;
    let errors: Vec<NotLoaded> = c.ok("rule.errors", json!(null)).await;
    assert!(errors.is_empty(), "{errors:?}");

    h.stop().await;
}

/// What the daemon's own load at start found is said the same way.
#[tokio::test]
async fn a_file_broken_at_start_is_said() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    std::fs::write(home.reactions_file(), "reactions:\n  x:\n   on: [\n").unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    let errors: Vec<NotLoaded> = c.ok("rule.errors", json!(null)).await;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].to_string().starts_with("global · reactions.yaml:"),
        "{errors:?}"
    );
    h.stop().await;
}
