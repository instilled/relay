//! Decisions against a real daemon: a question outlives the run that asked
//! it, choosing an option starts the run that option carries in the same
//! chain, answering twice is refused, asking again in a group replaces the
//! open question, and a deadline takes the default.

mod common;

use common::{Conn, Harness};
use rl_core::Home;
use rl_proto::decision::{Decision, DecisionStatus};
use rl_proto::types::{Cause, Run};
use serde_json::json;
use std::time::{Duration, Instant};

async fn decision(c: &mut Conn, id: &str) -> Decision {
    c.ok("decision.get", json!({ "id": id })).await
}

/// Polls until the question is no longer open, and until the runs its answer
/// started are recorded: the sweep resolves first and starts them after, so a
/// reader can catch it in between.
async fn wait_resolved(c: &mut Conn, id: &str, runs: usize) -> Decision {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let d = decision(c, id).await;
        if !d.status.is_open() && d.outcome_runs.len() == runs {
            return d;
        }
        assert!(Instant::now() < deadline, "decision {id} never settled: {d:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_question_outlives_its_run_and_its_answer_starts_the_next_one() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    // A run asks, then ends. The question stays.
    let asker: Run = c
        .ok("run.start", json!({ "argv": ["true"], "cwd": dir, "title": "logs" }))
        .await;
    c.wait_terminal(&asker.id).await;
    let d: Decision = c
        .ok(
            "decision.raise",
            json!({
                "run": asker.id,
                "title": "3 errors in prod",
                "summary": "NullPointerException in checkout, 3 times since 02:00",
                "options": [
                    { "key": "fix", "label": "Open a PR", "run": ["sh", "-c", "echo fixing $RELAY_CHOICE: $RELAY_NOTE > fixed.txt"] },
                    { "key": "ignore", "label": "Ignore" },
                ],
            }),
        )
        .await;
    assert!(d.status.is_open());
    assert_eq!(d.run.as_deref(), Some(asker.id.as_str()));
    assert_eq!(d.chain, asker.chain, "the question joins the run's chain");
    assert_eq!(d.workspace, asker.workspace);

    let open: Vec<Decision> = c.ok("decision.list", json!({})).await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, d.id);

    // Choosing starts the option's run, in the same chain, caused by this decision.
    let taken: Decision = c
        .ok(
            "decision.take",
            json!({ "id": d.id, "options": ["fix"], "note": "go on" }),
        )
        .await;
    assert_eq!(taken.status, DecisionStatus::Taken);
    assert_eq!(taken.chosen, ["fix"]);
    assert_eq!(taken.outcome_runs.len(), 1, "the option did not start");
    let started = c.get(&taken.outcome_runs[0]).await;
    assert_eq!(started.cause, Cause::Decision(d.id.clone()));
    assert_eq!(started.chain, asker.chain);
    c.wait_terminal(&started.id).await;
    // The answer's option and note reach the run it starts.
    assert_eq!(
        std::fs::read_to_string(dir.join("fixed.txt")).unwrap().trim(),
        "fixing fix: go on"
    );

    // Answered once. It is off the open list and refuses a second answer.
    assert!(c.ok::<Vec<Decision>>("decision.list", json!({})).await.is_empty());
    let res = c
        .call("decision.take", json!({ "id": d.id, "options": ["ignore"] }))
        .await;
    assert!(
        res.error
            .map(|e| e.message)
            .unwrap_or_default()
            .contains("already taken"),
        "a second answer was allowed"
    );
    let all: Vec<Decision> = c.ok("decision.list", json!({ "all": true })).await;
    assert_eq!(all.len(), 1);

    // What it asks for is checked: unknown options, and too many of them.
    let d2: Decision = c
        .ok(
            "decision.raise",
            json!({ "title": "deploy?", "cwd": dir, "options": [{ "key": "yes" }, { "key": "no" }] }),
        )
        .await;
    assert!(d2.run.is_none(), "no run asked this one");
    let res = c
        .call("decision.take", json!({ "id": d2.id, "options": ["maybe"] }))
        .await;
    assert!(res.error.is_some());
    let res = c
        .call("decision.take", json!({ "id": d2.id, "options": ["yes", "no"] }))
        .await;
    assert!(res.error.is_some(), "one option only, unless multi");
    let dismissed: Decision = c.ok("decision.dismiss", json!({ "id": d2.id })).await;
    assert_eq!(dismissed.status, DecisionStatus::Dismissed);
    assert!(dismissed.outcome_runs.is_empty());

    h.stop().await;
}

#[tokio::test]
async fn a_group_asks_again_instead_of_piling_up() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    let ask = |n: u32| {
        json!({
            "title": format!("{n} errors in prod"),
            "cwd": dir,
            "group": "nightly/errors",
            "options": [{ "key": "look" }],
        })
    };
    let first: Decision = c.ok("decision.raise", ask(3)).await;
    let again: Decision = c.ok("decision.raise", ask(5)).await;
    assert_eq!(again.id, first.id, "the same question, restated");
    assert_eq!(again.title, "5 errors in prod");
    let open: Vec<Decision> = c.ok("decision.list", json!({})).await;
    assert_eq!(open.len(), 1, "a group leaves one line, not many");

    // Answered, the group is free for the next one.
    c.ok::<Decision>("decision.dismiss", json!({ "id": first.id })).await;
    let fresh: Decision = c.ok("decision.raise", ask(1)).await;
    assert_ne!(fresh.id, first.id);
    h.stop().await;
}

#[tokio::test]
async fn a_deadline_takes_the_default_or_expires() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;

    // With a default, running out of time is an answer.
    let d: Decision = c
        .ok(
            "decision.raise",
            json!({
                "title": "send the daily mail?",
                "cwd": dir,
                "expires": "1s",
                "options": [
                    { "key": "send", "label": "Send it", "default": true, "run": ["sh", "-c", "echo sent > sent.txt"] },
                    { "key": "hold", "label": "Hold" },
                ],
            }),
        )
        .await;
    assert!(d.expires_at.is_some());
    let resolved = wait_resolved(&mut c, &d.id, 1).await;
    assert_eq!(resolved.status, DecisionStatus::Taken);
    assert_eq!(resolved.chosen, ["send"]);
    assert_eq!(resolved.outcome_runs.len(), 1);
    c.wait_terminal(&resolved.outcome_runs[0]).await;
    assert!(dir.join("sent.txt").is_file());

    // Without one, it just runs out.
    let d: Decision = c
        .ok(
            "decision.raise",
            json!({ "title": "still there?", "cwd": dir, "expires": "1s", "options": [{ "key": "yes" }] }),
        )
        .await;
    let resolved = wait_resolved(&mut c, &d.id, 0).await;
    assert_eq!(resolved.status, DecisionStatus::Expired);
    assert!(resolved.chosen.is_empty());
    assert!(resolved.outcome_runs.is_empty());
    h.stop().await;
}
