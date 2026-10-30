//! A run's agent says, by its hook, that its turn is over or that it waits on
//! you: the run is queued for review, asking or finished by what it last
//! said. Hooks that say neither queue nothing, nor does a Codex turn's end
//! that arrives after the next prompt.

mod common;

use common::{Conn, Harness};
use jiff::Timestamp;
use rl_core::Home;
use rl_ledger::{Ledger, review_queue};
use rl_proto::queue::{Hint, How, QueueItem, RestKind};
use rl_proto::types::{Run, RunState};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A stand-in agent: what makes a run Claude's, Codex's or Gemini's is the
/// binary's name.
fn stand_in(dir: &Path, name: &str) -> PathBuf {
    let bin = dir.join(name);
    std::fs::write(&bin, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin
}

struct Setup {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    home: Home,
    h: Harness,
    c: Conn,
    run: Run,
}

/// A daemon and one agent run of `agent`'s; `None` when `rl-session` is
/// not built.
async fn setup(agent: &str) -> Option<Setup> {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built");
        return None;
    };
    let mut c = Conn::open(&home).await;
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let bin = stand_in(&dir, agent);
    let run: Run = c
        .ok("run.start", json!({ "argv": [bin], "cwd": dir, "kind": "agent" }))
        .await;
    assert_eq!(run.cli.as_deref(), Some(agent));
    Some(Setup {
        _tmp: tmp,
        dir,
        home,
        h,
        c,
        run,
    })
}

async fn hook(c: &mut Conn, run: &Run, dialect: &str, event: &str, payload: Value) -> Run {
    c.ok(
        "run.hook",
        json!({ "id": run.id, "dialect": dialect, "event": event, "payload": payload }),
    )
    .await
}

fn item(home: &Home, run: &str) -> Option<QueueItem> {
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let id = run.to_string();
    ledger.call_blocking(move |c| review_queue::get(c, "run", &id)).unwrap()
}

/// Polls the ledger until the run is queued: the hook queues it off its own
/// clock.
async fn queued(home: &Home, run: &str) -> QueueItem {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(i) = item(home, run) {
            return i;
        }
        assert!(Instant::now() < deadline, "run {run} was never queued");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Gives a hook that should queue nothing the time to have, then says it
/// did not.
async fn not_queued(home: &Home, run: &str) {
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(item(home, run), None);
}

fn said(i: &QueueItem) -> (RestKind, How, Vec<Hint>) {
    (i.kind, i.how, i.hints.clone())
}

#[tokio::test]
async fn a_question_in_the_reply_queues_asks() {
    let Some(Setup {
        _tmp,
        home,
        h,
        mut c,
        run,
        ..
    }) = setup("claude").await
    else {
        return;
    };
    hook(
        &mut c,
        &run,
        "claude",
        "Stop",
        json!({ "last_assistant_message": "Done.\n\nShould I push?" }),
    )
    .await;
    let i = queued(&home, &run.id).await;
    assert_eq!(said(&i), (RestKind::Asks, How::Reply, vec![Hint::Question]));
    assert_eq!(i.rests, 1);
    assert_eq!(
        i.from.as_millisecond(),
        run.started_at.as_millisecond(),
        "from the run's start"
    );
    h.stop().await;
}

#[tokio::test]
async fn a_stop_with_no_message_reads_the_log() {
    let Some(Setup {
        _tmp,
        dir,
        home,
        h,
        mut c,
        run,
        ..
    }) = setup("claude").await
    else {
        return;
    };
    let line = |text: &str| {
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":{}}}]}}}}\n",
            Timestamp::now(),
            serde_json::to_string(text).unwrap()
        )
    };
    let transcript = dir.join("session.jsonl");
    std::fs::write(&transcript, line("Should I look at the index?") + &line("Fixed it.")).unwrap();
    hook(&mut c, &run, "claude", "Stop", json!({ "transcript_path": transcript })).await;
    let i = queued(&home, &run.id).await;
    assert_eq!(said(&i), (RestKind::Finished, How::Reply, vec![]));
    h.stop().await;
}

#[tokio::test]
async fn a_permission_prompt_queues_asks() {
    let Some(Setup {
        _tmp,
        home,
        h,
        mut c,
        run,
        ..
    }) = setup("claude").await
    else {
        return;
    };
    hook(
        &mut c,
        &run,
        "claude",
        "Notification",
        json!({ "notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash" }),
    )
    .await;
    let i = queued(&home, &run.id).await;
    assert_eq!(said(&i), (RestKind::Asks, How::Permission, vec![Hint::Permission]));
    h.stop().await;
}

#[tokio::test]
async fn idle_prompt_queues_nothing() {
    let Some(Setup {
        _tmp,
        home,
        h,
        mut c,
        run,
        ..
    }) = setup("claude").await
    else {
        return;
    };
    hook(
        &mut c,
        &run,
        "claude",
        "Notification",
        json!({ "notification_type": "idle_prompt", "message": "Claude is waiting for your input" }),
    )
    .await;
    not_queued(&home, &run.id).await;
    h.stop().await;
}

#[tokio::test]
async fn codex_late_stop_queues_nothing() {
    let Some(Setup {
        _tmp,
        home,
        h,
        mut c,
        run,
        ..
    }) = setup("codex").await
    else {
        return;
    };
    let working = hook(&mut c, &run, "codex", "UserPromptSubmit", json!({ "turn_id": "t2" })).await;
    assert_eq!(working.state, RunState::Working);
    // The first turn's end, arriving after the second prompt.
    let late = hook(
        &mut c,
        &run,
        "codex",
        "Stop",
        json!({ "turn_id": "t1", "last_assistant_message": "Done." }),
    )
    .await;
    assert_eq!(late.state, RunState::Working, "a late end leaves the run be");
    not_queued(&home, &run.id).await;

    let idle = hook(
        &mut c,
        &run,
        "codex",
        "Stop",
        json!({ "turn_id": "t2", "last_assistant_message": "Done." }),
    )
    .await;
    assert_eq!(idle.state, RunState::Idle);
    let i = queued(&home, &run.id).await;
    assert_eq!(said(&i), (RestKind::Finished, How::Reply, vec![]));
    assert_eq!(i.rests, 1);
    h.stop().await;
}

#[tokio::test]
async fn gemini_after_agent_reads_prompt_response() {
    let Some(Setup {
        _tmp,
        home,
        h,
        mut c,
        run,
        ..
    }) = setup("gemini").await
    else {
        return;
    };
    hook(
        &mut c,
        &run,
        "gemini",
        "AfterAgent",
        json!({ "prompt": "fix it", "prompt_response": "Fixed the failing test." }),
    )
    .await;
    let i = queued(&home, &run.id).await;
    assert_eq!(said(&i), (RestKind::Finished, How::Reply, vec![]));
    h.stop().await;
}
