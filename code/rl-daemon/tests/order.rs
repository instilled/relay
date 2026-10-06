//! `digest.order`: a project's rows in the order you put them, kept across a
//! restart, carried in the digest, and let go by `/sort`.

mod common;

use common::{Conn, Harness};
use rl_core::Home;
use rl_proto::methods::{Digest, Order, Resumed};
use rl_proto::rpc::RpcError;
use rl_proto::types::Run;
use serde_json::json;

#[tokio::test]
async fn a_projects_order_is_kept_across_a_restart_until_sort_lets_it_go() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let mut c = Conn::open(&h.home).await;
    let mut ids = Vec::new();
    let mut group = String::new();
    for _ in 0..3 {
        let r: Run = c
            .ok(
                "run.start",
                json!({ "argv": ["/bin/sh", "-c", "echo hi"], "cwd": proj.to_string_lossy() }),
            )
            .await;
        group = format!("g:{}", r.workspace.display());
        ids.push(r.id);
    }
    let rows: Vec<String> = ids.iter().rev().map(|id| format!("r:{id}")).collect();
    let now: Vec<Order> = c.ok("digest.order", json!({ "group": group, "rows": rows })).await;
    assert_eq!(
        now,
        [Order {
            group: group.clone(),
            rows: rows.clone()
        }]
    );
    for id in &ids {
        c.wait_terminal(id).await;
    }

    // A restart keeps it, and the digest carries it.
    h.stop().await;
    let h = Harness::on(home.clone()).unwrap();
    let mut c = Conn::open(&h.home).await;
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert_eq!(
        d.order,
        [Order {
            group: group.clone(),
            rows
        }]
    );

    // `/sort`: no rows, and the project sorts itself again — kept so too.
    let now: Vec<Order> = c.ok("digest.order", json!({ "group": group, "rows": [] })).await;
    assert!(now.is_empty());
    h.stop().await;
    let h = Harness::on(home).unwrap();
    let mut c = Conn::open(&h.home).await;
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert!(d.order.is_empty());

    // What cannot be put in order says so.
    for (group, row) in [
        ("", "r:a"),
        ("g:", "r:a"),
        ("/w/a", "r:a"),
        ("g:/w", "q:x"),
        ("g:/w", "r:"),
    ] {
        let res = c.call("digest.order", json!({ "group": group, "rows": [row] })).await;
        assert_eq!(res.error.unwrap().code, RpcError::INVALID_PARAMS, "{group:?} {row:?}");
    }
    h.stop().await;
}

#[tokio::test]
async fn a_resumed_run_takes_the_place_of_the_one_it_picks_up() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let mut c = Conn::open(&h.home).await;
    let mut ids = Vec::new();
    let mut group = String::new();
    for _ in 0..3 {
        let r: Run = c
            .ok(
                "run.start",
                json!({ "argv": ["/bin/sh", "-c", "echo hi"], "cwd": proj.to_string_lossy() }),
            )
            .await;
        group = format!("g:{}", r.workspace.display());
        ids.push(r.id);
    }
    for id in &ids {
        c.wait_terminal(id).await;
    }
    let rows: Vec<String> = ids.iter().map(|id| format!("r:{id}")).collect();
    let _: Vec<Order> = c.ok("digest.order", json!({ "group": group, "rows": rows })).await;

    // The middle one, picked back up, stays in the middle.
    let out: Vec<Resumed> = c.ok("run.resume", json!({ "ids": [ids[1]] })).await;
    let again = out[0].run.as_ref().expect("the command runs again");
    c.wait_terminal(&again.id).await;
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert_eq!(
        d.order,
        [Order {
            group,
            rows: vec![rows[0].clone(), format!("r:{}", again.id), rows[2].clone()]
        }]
    );
    h.stop().await;
}
