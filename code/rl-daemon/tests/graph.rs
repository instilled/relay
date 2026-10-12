//! `graph.get` over the socket: every flow a node, a flow's failures reaching
//! another flow's `on`, and an ask step's question drawn on the step's own
//! node, waiting on you.

mod common;

use common::{Conn, Harness};
use jiff::{SignedDuration, Timestamp};
use rl_core::Home;
use rl_proto::flow::{Firing, FiringStatus, Flow, FlowStatus};
use rl_proto::graph::{Graph, NodeKind};
use serde_json::json;
use std::time::{Duration, Instant};

/// Polls `flow.firings` every 50 ms for 10 s until the newest firing of `flow` is `there`.
async fn wait_firing(c: &mut Conn, flow: &str, there: impl Fn(&Firing) -> bool) -> Firing {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": flow })).await;
        if let Some(f) = firings.into_iter().next().filter(|f| there(f)) {
            return f;
        }
        assert!(Instant::now() < deadline, "`{flow}` never got there");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn the_graph_joins_flows_to_what_happened() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    for (name, md) in [
        ("nightly", "---\nsteps:\n  a:\n    run: exit 1\n---\n"),
        (
            "notify",
            "---\non: [run.failed, never.*]\nwhere: { cause: \"flow:nightly\" }\nsteps:\n  tell:\n    run: \"true\"\n---\n",
        ),
        (
            "asker",
            "---\nsteps:\n  review:\n    ask: \"go?\"\n    options: { yes: { run: \"true\" } }\n---\n",
        ),
    ] {
        let dir = home.flow_dir(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("flow.md"), md).unwrap();
    }
    let Some(h) = Harness::on(home) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&h.home).await;
    for name in ["nightly", "notify", "asker"] {
        let _: Flow = common::approve(&mut c, name).await;
    }
    let _: Firing = c.ok("flow.run", json!({ "name": "nightly" })).await;
    let _: Firing = c.ok("flow.run", json!({ "name": "asker" })).await;
    wait_firing(&mut c, "notify", |f| f.ended_at.is_some()).await;
    wait_firing(&mut c, "nightly", |f| f.ended_at.is_some()).await;
    let asked = wait_firing(&mut c, "asker", |f| f.status == FiringStatus::Waiting).await;
    let question = asked.steps[0].decision.clone().expect("asked");

    let g: Graph = c.ok("graph.get", json!({})).await;
    let edge = |from: &str, to: &str| {
        g.edges
            .iter()
            .find(|e| e.from == from && e.to == to)
            .unwrap_or_else(|| panic!("no edge {from} → {to} in {:#?}", g.edges))
    };

    let nightly = g.node("flow:nightly").expect("every flow is a node");
    assert_eq!(
        (nightly.kind, nightly.status, nightly.detail.as_str()),
        (NodeKind::Flow, Some(FlowStatus::On), "by hand")
    );
    assert_eq!((nightly.runs.total, nightly.runs.failed), (1, 1));
    assert_eq!(g.node("step:nightly/a").unwrap().runs.failed, 1);
    let e = edge("step:nightly/a", "flow:notify");
    assert_eq!((e.label.as_str(), e.count, e.declared), ("run.failed", 1, false));
    assert_eq!(edge("flow:nightly", "step:nightly/a").count, 1);

    // The ask step's question, waiting, on the step.
    let ask = g.node("step:asker/review").unwrap();
    assert_eq!(
        (ask.kind, ask.name.as_str(), ask.detail.as_str()),
        (NodeKind::Ask, "review", "go?")
    );
    assert_eq!(ask.waiting, [question]);
    assert_eq!(edge("flow:asker", "step:asker/review").count, 1);

    // What nothing raised is a way in.
    assert!(edge("event:never.*", "flow:notify").declared);
    assert!(g.nodes.iter().all(|n| n.kind != NodeKind::Outside));

    // A window that starts after all of it keeps the flows, their steps, and what still waits.
    let later = Timestamp::now() + SignedDuration::from_mins(1);
    let g: Graph = c.ok("graph.get", json!({ "since": later })).await;
    assert!(g.node("flow:nightly").unwrap().runs.total == 0);
    let first = g
        .edges
        .iter()
        .find(|e| e.from == "flow:nightly" && e.to == "step:nightly/a")
        .expect("a flow's wiring is there in any window");
    assert_eq!(first.count, 0);
    assert!(
        !g.edges
            .iter()
            .any(|e| e.to == "flow:notify" && e.from.starts_with("step:nightly"))
    );
    assert_eq!(g.node("step:asker/review").unwrap().waiting.len(), 1, "still waiting");
    h.stop().await;
}

/// A flow's node stands in its `in` as the project is registered, however `in` is spelled.
#[cfg(unix)]
#[tokio::test]
async fn a_flows_node_stands_in_its_project_however_its_in_is_spelled() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    let real = tmp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let dir = home.flow_dir("there");
    std::fs::create_dir_all(&dir).unwrap();
    let md = format!("---\nin: {}/\nsteps:\n  a:\n    run: \"true\"\n---\n", link.display());
    std::fs::write(dir.join("flow.md"), md).unwrap();
    let Some(h) = Harness::on(home) else { return };
    let mut c = Conn::open(&h.home).await;
    let _: serde_json::Value = c.ok("workspace.up", json!({ "path": real })).await;
    let g: Graph = c.ok("graph.get", json!({})).await;
    let node = g.node("flow:there").expect("every flow is a node");
    assert_eq!(node.folder, Some(std::fs::canonicalize(&real).unwrap()));
    h.stop().await;
}
