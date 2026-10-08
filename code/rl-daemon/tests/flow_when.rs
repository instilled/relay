//! `when` while the flow's last firing still holds it: a queued firing waits for it, however
//! long, and is not said missed.

mod common;
use common::{Conn, Harness, approve};
use rl_core::Home;
use rl_proto::flow::{Firing, Flow};
use rl_proto::types::Event;
use serde_json::json;
use std::time::{Duration, Instant};

#[tokio::test]
async fn a_queued_firing_waits_past_two_minutes_and_is_not_missed() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    let dir = home.flow_dir("long");
    std::fs::create_dir_all(&dir).unwrap();
    let md = "---\nwhen: every 1h\noverlap: queue\nsteps:\n  a:\n    run: sleep 2\n---\n";
    std::fs::write(dir.join("flow.md"), md).unwrap();
    let Some(h) = Harness::on(home.clone()) else { return };
    let mut c = Conn::open(&h.home).await;
    approve(&mut c, "long").await;
    let first: Firing = c.ok("flow.run", json!({ "name": "long" })).await;
    // As if it had started ten minutes ago, and the next was due nine minutes ago: it held
    // the one due for longer than two minutes.
    let ledger = rl_ledger::Ledger::open(&home.ledger()).unwrap();
    let ago = |mins: i64| jiff::Timestamp::now().as_millisecond() - mins * 60_000;
    let id = first.id.clone();
    ledger
        .call_blocking(move |db| {
            db.execute(
                "UPDATE firings SET started_at = ?2 WHERE id = ?1",
                rusqlite::params![id, ago(10)],
            )?;
            db.execute("UPDATE flows SET next_at = ?1 WHERE name = 'long'", [ago(9)])?;
            Ok(())
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let firings = loop {
        let firings: Vec<Firing> = c.ok("flow.firings", json!({ "flow": "long" })).await;
        if firings.len() == 2 {
            break firings;
        }
        assert!(Instant::now() < deadline, "the queued firing never fired: {firings:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(firings[0].trigger, "when");
    assert!(firings[0].started_at >= firings[1].ended_at.unwrap(), "{firings:?}");
    let skipped: Vec<Event> = c.ok("event.list", json!({ "kinds": ["flow.skipped"] })).await;
    assert!(skipped.is_empty(), "{skipped:?}");
    // Planned again, ahead, once it has fired.
    loop {
        let f: Vec<Flow> = c.ok("flow.list", json!({})).await;
        if f[0].next_at.is_some_and(|at| at > jiff::Timestamp::now()) {
            break;
        }
        assert!(Instant::now() < deadline, "never planned again: {f:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    h.stop().await;
}
