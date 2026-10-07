//! `graph.get` over the socket: rules from the user's own file, and what the
//! ledger saw — a folder's failing shells reach a global reaction, and a
//! question's own events are traced back to the reaction that asked it.

mod common;

use common::Conn;
use jiff::{SignedDuration, Timestamp};
use rl_core::Home;
use rl_ledger::events::NewEvent;
use rl_ledger::{Ledger, decisions, events, runs};
use rl_proto::decision::{Decision, DecisionOption, DecisionStatus};
use rl_proto::graph::{Graph, NodeKind};
use rl_proto::types::{Run, RunKind, RunState, event_kind};
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn the_graph_joins_rules_to_what_happened() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    std::fs::write(
        home.reactions_file(),
        "reactions:\n  \
           tell-me:\n    \
             on: [run.failed, decision.raised]\n    \
             cwd: /tmp\n    \
             run: 'true'\n  \
           asker:\n    \
             on: custom.thing\n    \
             cwd: /tmp\n    \
             ask:\n      \
               title: go?\n      \
               options:\n        \
                 - key: yes\n          \
                   run: 'true'\n",
    )
    .unwrap();
    let token = CancellationToken::new();
    let server = tokio::spawn(rl_daemon::serve(home.clone(), token.clone()));
    let mut c = Conn::open(&home).await;

    // An hour ago: inside the window, too old for the reactor to act on.
    let then = Timestamp::now() - SignedDuration::from_hours(1);
    let side = Ledger::open(&home.ledger()).unwrap();
    let mut shell = Run::new("shell001", RunKind::Shell, "zsh", vec![], "/w/api", "/w/api", then);
    shell.state = RunState::Failed;
    let failed = NewEvent::for_run(&shell, then, event_kind::RUN_FAILED, "exit 1");
    let question = Decision {
        id: "dq000001".into(),
        workspace: "/tmp".into(),
        run: None,
        chain: "dq000001".into(),
        group: Some("asker".into()),
        title: "go?".into(),
        summary: None,
        options: vec![DecisionOption::new("yes", "")],
        multi: false,
        status: DecisionStatus::Open,
        created_at: then,
        updated_at: then,
        expires_at: None,
        resolved_at: None,
        chosen: vec![],
        outcome_runs: vec![],
        note: None,
    };
    let raised = NewEvent {
        at: then,
        workspace: "/tmp".into(),
        run: None,
        group_key: "asker".into(),
        kind: "decision.raised".into(),
        summary: "go?".into(),
        data: Some(json!({"decision": "dq000001"})),
    };
    side.call_blocking(move |db| {
        runs::insert(db, &shell)?;
        events::append(db, &failed)?;
        decisions::insert(db, &question)?;
        decisions::set_by_reaction(db, "dq000001", "asker")?;
        events::append(db, &raised)?;
        Ok(())
    })
    .unwrap();

    let g: Graph = c.ok("graph.get", json!({})).await;
    let edge = |from: &str, to: &str| {
        g.edges
            .iter()
            .find(|e| e.from == from && e.to == to)
            .unwrap_or_else(|| panic!("no edge {from} → {to} in {:#?}", g.edges))
    };

    let shells = g
        .node("runs:shells:/w/api")
        .expect("the folder's shells set something off");
    assert_eq!((shells.kind, shells.runs.failed), (NodeKind::Shell, 1));
    let e = edge("runs:shells:/w/api", "reaction:global/tell-me");
    assert_eq!((e.label.as_str(), e.count), ("run.failed", 1));

    // The question's event comes from the question, not from nowhere.
    let ask = g.node("ask:reaction:global/asker").unwrap();
    assert_eq!(ask.waiting, ["dq000001"]);
    let e = edge("ask:reaction:global/asker", "reaction:global/tell-me");
    assert_eq!((e.label.as_str(), e.count), ("decision.raised", 1));
    assert!(g.nodes.iter().all(|n| n.kind != NodeKind::Outside));

    // Declared: the answer and what it runs, and the event nothing raised.
    assert!(edge("ask:reaction:global/asker", "run:ask:reaction:global/asker/yes").declared);
    assert!(edge("event:custom.thing", "reaction:global/asker").declared);

    // A window that starts after all of it keeps the files and drops the rest.
    let later = Timestamp::now() + SignedDuration::from_mins(1);
    let g: Graph = c.ok("graph.get", json!({ "since": later })).await;
    assert!(g.node("runs:shells:/w/api").is_none());
    assert_eq!(
        g.node("ask:reaction:global/asker").unwrap().waiting,
        ["dq000001"],
        "still waiting"
    );

    token.cancel();
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
