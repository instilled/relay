//! `assistant.run`: the configured agent runs headless with the prompt as
//! its last argument, as an ordinary run with `cause: assistant:<job>`, and
//! its output comes back; a slow one is stopped at the timeout; no agent
//! configured or installed is refused.

mod common;

use common::{Conn, Harness, journal};
use relay_core::Home;
use relay_proto::methods::AssistantOutput;
use relay_proto::rpc::Response;
use relay_proto::types::{Cause, Run, RunKind, RunState};
use serde_json::json;
use std::path::PathBuf;

fn fake_agent() -> PathBuf {
    std::fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-agent")).unwrap()
}

#[tokio::test]
async fn the_assistant_is_a_run_and_answers() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    // An agent profile of the test's own, pointing at the fixture, and the
    // config naming it as the interpreter.
    std::fs::create_dir_all(home.detect_dir()).unwrap();
    std::fs::write(
        home.detect_dir().join("fakeai.yaml"),
        format!(
            "kind: agent\nbins: [\"{}\"]\nheadless: [\"--headless\"]\n",
            fake_agent().display()
        ),
    )
    .unwrap();
    std::fs::write(home.config_file(), "assistants:\n  interpret: fakeai\n").unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let mut c = Conn::open(&home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();

    let out: AssistantOutput = c
        .ok(
            "assistant.run",
            json!({ "job": "interpret", "prompt": "PACK… run the tests", "cwd": cwd, "title": "interpret: run the tests" }),
        )
        .await;
    assert_eq!(out.agent, "fakeai");
    assert!(!out.timed_out);
    assert_eq!(out.exit, Some(0));
    assert!(
        out.output.contains("kind: run") && out.output.contains("run: echo drafted"),
        "{}",
        out.output
    );

    // An ordinary run, with the job as its cause and the tag.
    let run: Run = c.get(&out.run).await;
    assert_eq!(run.kind, RunKind::Agent);
    assert_eq!(run.cause, Cause::Assistant("interpret".into()));
    assert_eq!(run.state, RunState::Finished);
    assert!(run.tags.iter().any(|t| t == "relay:assistant"), "{:?}", run.tags);
    assert_eq!(run.title, "interpret: run the tests");
    assert_eq!(
        run.argv.last().map(String::as_str),
        Some("PACK… run the tests"),
        "the prompt is the last argument"
    );
    assert!(
        journal(&home, &out.run).contains("kind: run"),
        "the answer is in the journal too"
    );

    // Too slow: stopped at the timeout, said so.
    // SAFETY: this test binary is alone in its process.
    unsafe { std::env::set_var("FAKE_AGENT_SLEEP", "30") };
    let out: AssistantOutput = c
        .ok(
            "assistant.run",
            json!({ "job": "interpret", "prompt": "p", "cwd": cwd, "title": "slow", "timeout": "1s" }),
        )
        .await;
    assert!(out.timed_out, "{out:?}");
    let run = c.wait_terminal(&out.run).await;
    assert_ne!(run.state, RunState::Finished);
    unsafe { std::env::remove_var("FAKE_AGENT_SLEEP") };

    // Another job does not exist; an agent that is not installed is refused.
    let r: Response = c
        .call(
            "assistant.run",
            json!({ "job": "summarise", "prompt": "p", "cwd": cwd, "title": "t" }),
        )
        .await;
    assert!(r.error.unwrap().message.contains("summarise"));
    std::fs::write(home.config_file(), "assistants:\n  interpret: cursor\n").unwrap();
    let _: serde_json::Value = c.ok("config.get", json!(null)).await;
    let r: Response = c
        .call(
            "assistant.run",
            json!({ "job": "interpret", "prompt": "p", "cwd": cwd, "title": "t" }),
        )
        .await;
    let msg = r.error.unwrap().message;
    assert!(msg.contains("cursor") && msg.contains("not on this machine"), "{msg}");

    h.stop().await;
}
