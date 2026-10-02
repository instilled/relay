//! `summary.make` / `summary.get`: a run's facts and output go to
//! `assistants.summarise` as an assistant run; the answer is kept by the run
//! and said in a `run.summarised` event, and given back while the run has not
//! moved on. A stretch with nothing in it needs no assistant; Claude's own
//! recap, from the transcript its hook names, needs none either. An
//! assistant's own run is refused.

mod common;

use common::{Conn, Harness, wait_journal};
use rl_core::Home;
use rl_proto::rpc::Response;
use rl_proto::types::{Cause, Event, Run, RunState, Summary};
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
    // The fixture answers with this file, and writes the prompt to that one;
    // it reads its input to the end first, as `claude -p` does, so an input
    // left open would hang it. The daemon is in this process, so its runs see
    // these variables.
    let answer = tmp.path().join("answer.txt");
    let prompt = tmp.path().join("prompt.txt");
    std::fs::write(&answer, "```\nIt printed hello and left.\nNothing changed.\n```\n").unwrap();
    // SAFETY: this test binary is alone in its process.
    unsafe {
        std::env::set_var("FAKE_AGENT_ANSWER", &answer);
        std::env::set_var("FAKE_AGENT_PROMPT", &prompt);
        std::env::set_var("FAKE_AGENT_READS", "1");
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

    // A run still going can be recapped too; a stretch with nothing in it
    // says so without asking anyone.
    let slow: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "sleep 5"], "cwd": cwd, "title": "slow" }),
        )
        .await;
    c.wait_state(&slow.id, RunState::Working).await;
    let quiet: Summary = c.ok("summary.make", json!({ "run": slow.id, "since": "1m" })).await;
    assert_eq!(quiet.text, "Nothing in the last 1m.");
    assert_eq!((quiet.source.as_str(), quiet.by.as_str()), ("nothing new", ""));
    assert_eq!(quiet.since.as_deref(), Some("1m"));
    let r: Response = c.call("summary.make", json!({ "run": slow.id, "since": "soon" })).await;
    assert!(r.error.unwrap().message.contains("not a stretch of time"));
    let _: serde_json::Value = c.ok("run.stop", json!({ "id": slow.id, "force": true })).await;

    // The CLI's own warning about its input is not the answer.
    std::fs::write(
        &answer,
        "Warning: no stdin data received in 3s, proceeding without it. If piping from a slow command, \
         redirect stdin explicitly: < /dev/null to skip, or wait longer.\nIt printed hello and left.\n",
    )
    .unwrap();
    let warned: Summary = c.ok("summary.make", json!({ "run": run.id, "again": true })).await;
    assert_eq!(warned.text, "It printed hello and left.");

    // Nothing said: an error that names the assistant's run.
    std::fs::write(&answer, "\n").unwrap();
    let r: Response = c.call("summary.make", json!({ "run": run.id, "again": true })).await;
    let msg = r.error.unwrap().message;
    assert!(msg.contains("said nothing"), "{msg}");

    unsafe {
        std::env::remove_var("FAKE_AGENT_ANSWER");
        std::env::remove_var("FAKE_AGENT_PROMPT");
        std::env::remove_var("FAKE_AGENT_READS");
    }
    h.stop().await;
}

#[tokio::test]
async fn claudes_own_recap_is_taken_from_its_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    // An assistant that is not on this machine: a fallback that needs one
    // fails plainly instead of asking a real agent.
    std::fs::create_dir_all(home.detect_dir()).unwrap();
    std::fs::write(
        home.detect_dir().join("nosuchai.yaml"),
        "kind: agent\nbins: [\"/nonexistent/nosuchai\"]\nheadless: [\"--headless\"]\n",
    )
    .unwrap();
    std::fs::write(home.config_file(), "assistants:\n  summarise: nosuchai\n").unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let mut c = Conn::open(&home).await;
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    // A stand-in `claude`: what makes the run Claude's is the binary's name.
    let bin = dir.join("claude");
    std::fs::write(&bin, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [bin], "cwd": dir, "title": "fix search", "kind": "agent" }),
        )
        .await;
    assert_eq!(run.cli.as_deref(), Some("claude"), "{run:?}");

    // Its hook names the transcript, which holds Claude's own recap.
    let transcript = dir.join("session.jsonl");
    let recent = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(30);
    let old = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(3);
    std::fs::write(
        &transcript,
        format!(
            "{{\"type\":\"system\",\"subtype\":\"away_summary\",\"timestamp\":\"{old}\",\"content\":\"Old news.\"}}\n\
             {{\"type\":\"system\",\"subtype\":\"away_summary\",\"timestamp\":\"{recent}\",\"content\":\"Search fixed; nothing committed.\"}}\n"
        ),
    )
    .unwrap();
    let _: Run = c
        .ok(
            "run.hook",
            json!({ "id": run.id, "event": "Stop", "payload": { "transcript_path": transcript } }),
        )
        .await;

    let s: Summary = c.ok("summary.make", json!({ "run": run.id })).await;
    assert_eq!(s.text, "Search fixed; nothing committed.");
    assert_eq!(
        (s.agent.as_str(), s.source.as_str(), s.by.as_str()),
        ("claude", "claude recap", "")
    );
    assert!(!s.stale);

    // Kept while the transcript has not grown; stale once it has.
    let kept: Option<Summary> = c.ok("summary.get", json!({ "id": run.id })).await;
    assert_eq!(kept.as_ref().map(|k| k.stale), Some(false));
    let mut f = std::fs::OpenOptions::new().append(true).open(&transcript).unwrap();
    std::io::Write::write_all(
        &mut f,
        format!("{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"Committed.\"}}]}}}}\n", jiff::Timestamp::now()).as_bytes(),
    )
    .unwrap();
    let kept: Option<Summary> = c.ok("summary.get", json!({ "id": run.id })).await;
    assert_eq!(kept.map(|k| k.stale), Some(true));

    // A recap from before the stretch does not count: what was said goes to
    // the assistant instead — here one that is not on this machine.
    let r: Response = c.call("summary.make", json!({ "run": run.id, "since": "10s" })).await;
    let msg = r.error.map(|e| e.message).unwrap_or_default();
    assert!(msg.contains("not on this machine"), "{msg}");

    let _: serde_json::Value = c.ok("run.stop", json!({ "id": run.id, "force": true })).await;
    h.stop().await;
}
