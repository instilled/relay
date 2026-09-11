//! `summary.make` / `summary.get`: an ended run's facts and output go to
//! `assistants.summarise` as an assistant run; the answer is kept by the run
//! and said in a `run.summarised` event. A run still going, and an
//! assistant's own run, are refused.

mod common;

use common::{Conn, Harness, wait_journal};
use relay_core::Home;
use relay_proto::rpc::Response;
use relay_proto::types::{Cause, Event, Run, RunState, Summary};
use serde_json::json;
use std::path::PathBuf;

fn fake_agent() -> PathBuf {
    std::fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-agent")).unwrap()
}

#[tokio::test]
async fn an_ended_run_gets_a_summary_kept_by_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    std::fs::create_dir_all(home.detect_dir()).unwrap();
    std::fs::write(
        home.detect_dir().join("fakeai.yaml"),
        format!(
            "kind: agent\nbins: [\"{}\"]\nheadless: [\"--headless\"]\n",
            fake_agent().display()
        ),
    )
    .unwrap();
    std::fs::write(home.config_file(), "assistants:\n  summarise: fakeai\n").unwrap();
    // The fixture answers with this file, and writes the prompt to that one.
    // The daemon is in this process, so its runs see these variables.
    let answer = tmp.path().join("answer.txt");
    let prompt = tmp.path().join("prompt.txt");
    std::fs::write(&answer, "```\nIt printed hello and left.\nNothing changed.\n```\n").unwrap();
    // SAFETY: this test binary is alone in its process.
    unsafe {
        std::env::set_var("FAKE_AGENT_ANSWER", &answer);
        std::env::set_var("FAKE_AGENT_PROMPT", &prompt);
    }
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let mut c = Conn::open(&home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();

    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "echo hello from the run"], "cwd": cwd, "title": "say hello" }),
        )
        .await;
    // Nothing kept yet: a null result.
    let nothing: Response = c.call("summary.get", json!({ "id": run.id })).await;
    assert!(nothing.error.is_none(), "{nothing:?}");
    assert!(nothing.result.unwrap_or(serde_json::Value::Null).is_null());
    c.wait_terminal(&run.id).await;
    wait_journal(&home, &run.id, "hello from the run").await;

    let s: Summary = c.ok("summary.make", json!({ "run": run.id })).await;
    assert_eq!(s.run, run.id);
    assert_eq!(s.agent, "fakeai");
    assert_eq!(
        s.text, "It printed hello and left.\nNothing changed.",
        "fences dropped, kept as prose"
    );
    let by: Run = c.get(&s.by).await;
    assert_eq!(by.cause, Cause::Assistant("summarise".into()));
    assert_eq!(by.title, "summarise: say hello");
    assert_eq!(by.state, RunState::Finished);
    let p = std::fs::read_to_string(&prompt).unwrap();
    assert!(p.contains("title: say hello"), "{p}");
    assert!(p.contains("command: sh -c echo hello from the run"), "{p}");
    assert!(
        p.contains("run.finished") || p.contains("run.started"),
        "the events: {p}"
    );
    assert!(p.contains("hello from the run"), "the output: {p}");

    // Kept: the same one comes back; asked again, a new assistant run.
    let kept: Option<Summary> = c.ok("summary.get", json!({ "id": run.id })).await;
    assert_eq!(kept.as_ref(), Some(&s));
    let same: Summary = c.ok("summary.make", json!({ "run": run.id })).await;
    assert_eq!(same.by, s.by);
    let again: Summary = c.ok("summary.make", json!({ "run": run.id, "again": true })).await;
    assert_ne!(again.by, s.by);
    let kept: Option<Summary> = c.ok("summary.get", json!({ "id": run.id })).await;
    assert_eq!(kept.map(|k| k.by), Some(again.by.clone()));

    // Said as an event on the run, first line only.
    let evs: Vec<Event> = c
        .ok("event.list", json!({ "run": run.id, "kinds": ["run.summarised"] }))
        .await;
    assert_eq!(evs.len(), 2, "{evs:?}");
    assert_eq!(evs[0].summary, "It printed hello and left.");

    // An assistant's own run is not summarised.
    let r: Response = c.call("summary.make", json!({ "run": again.by })).await;
    assert!(r.error.unwrap().message.contains("assistant's own run"));

    // A run still going is not either.
    let slow: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "sleep 5"], "cwd": cwd, "title": "slow" }),
        )
        .await;
    c.wait_state(&slow.id, RunState::Working).await;
    let r: Response = c.call("summary.make", json!({ "run": slow.id })).await;
    assert!(r.error.unwrap().message.contains("still going"));
    let _: serde_json::Value = c.ok("run.stop", json!({ "id": slow.id, "force": true })).await;

    // Nothing said: an error that names the assistant's run.
    std::fs::write(&answer, "\n").unwrap();
    let r: Response = c.call("summary.make", json!({ "run": run.id, "again": true })).await;
    let msg = r.error.unwrap().message;
    assert!(msg.contains("said nothing"), "{msg}");

    unsafe {
        std::env::remove_var("FAKE_AGENT_ANSWER");
        std::env::remove_var("FAKE_AGENT_PROMPT");
    }
    h.stop().await;
}
