//! A run's agent says, by its hook, that its turn is over or that it waits on
//! you: the run is queued for review, asking or finished by what it last
//! said. Hooks that say neither queue nothing, nor does a Codex turn's end
//! that arrives after the next prompt. A run with no hook is queued by its
//! screen, unless a hook follows within the grace; its end adds `stopped`
//! or `failed`.

mod common;

use common::{Conn, Harness, fake_cli};
use jiff::Timestamp;
use rl_core::Home;
use rl_ledger::{Ledger, review_queue};
use rl_proto::lead::{Band, Lead};
use rl_proto::methods::{AssistantOutput, QueueReviewed, Report};
use rl_proto::queue::{Hint, How, ItemView, QueueItem, RestKind};
use rl_proto::types::{Cause, Run, RunState, Source};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A stand-in agent: what makes a run Claude's, Codex's or Gemini's is the
/// binary's name.
fn stand_in(dir: &Path, name: &str) -> PathBuf {
    stand_in_doing(dir, name, "sleep 30")
}

fn stand_in_doing(dir: &Path, name: &str, script: &str) -> PathBuf {
    let bin = dir.join(name);
    std::fs::write(&bin, format!("#!/bin/sh\n{script}\n")).unwrap();
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

#[tokio::test]
async fn an_assistant_run_queues_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    // Relay's own assistant: a stand-in agent profile, named for a job.
    let fake = std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-agent")).unwrap();
    std::fs::write(
        home.detect_dir().join("fakeai.yaml"),
        format!(
            "kind: agent\nbins: [\"{}\"]\nheadless: [\"--headless\"]\n",
            fake.display()
        ),
    )
    .unwrap();
    std::fs::write(home.config_file(), "assistants:\n  interpret: fakeai\n").unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let mut c = Conn::open(&home).await;
    // SAFETY: the only test in this binary that runs the fake agent; the
    // others never read it.
    unsafe { std::env::set_var("FAKE_AGENT_READS", "1") };
    let out: AssistantOutput = c
        .ok(
            "assistant.run",
            json!({ "job": "interpret", "prompt": "run the tests", "cwd": tmp.path(), "title": "interpret", "timeout": "10s" }),
        )
        .await;
    let run = c.get(&out.run).await;
    assert_eq!(run.cause, Cause::Assistant("interpret".into()));
    hook(
        &mut c,
        &run,
        "claude",
        "Stop",
        json!({ "last_assistant_message": "Should I run them?" }),
    )
    .await;
    not_queued(&home, &run.id).await;
    h.stop().await;
}

/// A daemon whose screen reads a prompt as idle within 300ms; `None` when
/// `rl-session` is not built.
fn quick() -> Option<(tempfile::TempDir, PathBuf, Home, Harness)> {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let registry = Harness::quick_registry(&home);
    let Some(h) = Harness::with_registry(home.clone(), Some(registry)) else {
        eprintln!("skipping: rl-session not built");
        return None;
    };
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    Some((tmp, dir, home, h))
}

#[tokio::test]
async fn a_shell_that_goes_idle_queues_finished_by_screen() {
    let Some((_tmp, dir, home, h)) = quick() else {
        return;
    };
    let mut c = Conn::open(&home).await;
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["/bin/sh", "-i"], "cwd": dir, "kind": "shell" }),
        )
        .await;
    let idle = c.wait_state(&run.id, RunState::Idle).await;
    let i = queued(&home, &run.id).await;
    assert_eq!(said(&i), (RestKind::Finished, How::Screen, vec![]));
    assert!(i.at <= idle.state_at, "at the rest, not after the grace");
    let _: Run = c.ok("run.stop", json!({ "id": run.id })).await;
    h.stop().await;
}

#[tokio::test]
async fn an_end_inside_the_grace_wins() {
    let Some((_tmp, dir, home, h)) = quick() else {
        return;
    };
    let mut c = Conn::open(&home).await;
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["/bin/sh", "-i"], "cwd": dir, "kind": "shell" }),
        )
        .await;
    c.wait_state(&run.id, RunState::Idle).await;
    let _: Run = c.ok("run.stop", json!({ "id": run.id })).await;
    let ended = c.wait_terminal(&run.id).await;
    let i = queued(&home, &run.id).await;
    // Past the grace, the screen's rest is not counted over the end.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let i2 = item(&home, &run.id).unwrap();
    assert_eq!(i2, i, "the screen's rest changed nothing");
    assert_eq!(said(&i), (RestKind::Finished, How::Screen, vec![Hint::Stopped]));
    assert_eq!(i.rests, 1);
    assert_eq!(
        i.at.as_millisecond(),
        ended.ended_at.unwrap().as_millisecond(),
        "at the end"
    );
    h.stop().await;
}

#[tokio::test]
async fn a_hook_inside_the_grace_wins() {
    let Some((_tmp, dir, home, h)) = quick() else {
        return;
    };
    let mut c = Conn::open(&home).await;
    let bin = stand_in_doing(&dir, "claude", "printf '> '; sleep 30");
    let run: Run = c
        .ok("run.start", json!({ "argv": [bin], "cwd": dir, "kind": "agent" }))
        .await;
    let idle = c.wait_state(&run.id, RunState::Idle).await;
    assert_eq!(idle.source, Source::Pty, "the screen saw it first");
    hook(
        &mut c,
        &run,
        "claude",
        "Stop",
        json!({ "last_assistant_message": "Done." }),
    )
    .await;
    let i = queued(&home, &run.id).await;
    assert_eq!(said(&i), (RestKind::Finished, How::Reply, vec![]));
    // Past the grace, the screen's rest is still not counted.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let i = item(&home, &run.id).unwrap();
    assert_eq!((i.how, i.rests), (How::Reply, 1));
    let _: Run = c.ok("run.stop", json!({ "id": run.id })).await;
    h.stop().await;
}

#[tokio::test]
async fn a_failed_run_is_queued_failed() {
    let Some((_tmp, dir, home, h)) = quick() else {
        return;
    };
    let mut c = Conn::open(&home).await;
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "sleep 0.2; exit 3"], "cwd": dir, "kind": "command" }),
        )
        .await;
    let ended = c.wait_terminal(&run.id).await;
    assert_eq!(ended.state, RunState::Failed);
    let i = queued(&home, &run.id).await;
    assert_eq!(said(&i), (RestKind::Finished, How::Screen, vec![Hint::Failed]));
    h.stop().await;
}

#[tokio::test]
async fn a_killed_run_is_stopped() {
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
        json!({ "last_assistant_message": "Should I push?" }),
    )
    .await;
    queued(&home, &run.id).await;
    let _: Run = c.ok("run.stop", json!({ "id": run.id })).await;
    assert_eq!(c.wait_terminal(&run.id).await.state, RunState::Stopped);
    let deadline = Instant::now() + Duration::from_secs(15);
    let i = loop {
        let i = item(&home, &run.id).unwrap();
        if i.hints.contains(&Hint::Stopped) {
            break i;
        }
        assert!(Instant::now() < deadline, "never stopped: {i:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        said(&i),
        (RestKind::Asks, How::Reply, vec![Hint::Question, Hint::Stopped])
    );
    h.stop().await;
}

#[tokio::test]
async fn start_head_is_kept() {
    let Some((_tmp, dir, home, h)) = quick() else {
        return;
    };
    let git = |repo: &Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let repo = dir.join("repo");
    let plain = dir.join("plain");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&plain).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let mut c = Conn::open(&home).await;
    let ledger = Ledger::open(&home.ledger()).unwrap();
    for (cwd, want) in [(&repo, Some(head)), (&plain, None)] {
        let run: Run = c
            .ok(
                "run.start",
                json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent" }),
            )
            .await;
        assert_eq!(run.start_head, want);
        let id = run.id.clone();
        let kept = ledger
            .call_blocking(move |c| rl_ledger::runs::get(c, &id))
            .unwrap()
            .unwrap();
        assert_eq!(kept.start_head, want, "{}", cwd.display());
        let _: Run = c.ok("run.stop", json!({ "id": run.id })).await;
    }
    h.stop().await;
}

/// One assistant message of a Claude session file, said at `at`.
fn claude_line(at: Timestamp, text: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":{}}}]}}}}\n",
        serde_json::to_string(text).unwrap()
    )
}

/// Every run lead in the report, by key.
async fn run_leads(c: &mut Conn) -> Vec<Lead> {
    let report: Report = c.ok("report.get", json!({})).await;
    report
        .needs_you
        .into_iter()
        .chain(report.live)
        .chain(report.happened)
        .filter(|l| l.key.starts_with("run:"))
        .collect()
}

#[tokio::test]
async fn review_clears_and_report_drops_it() {
    let Some(Setup {
        _tmp,
        dir,
        home,
        h,
        mut c,
        run,
    }) = setup("claude").await
    else {
        return;
    };
    let transcript = dir.join("session.jsonl");
    std::fs::write(&transcript, claude_line(Timestamp::now(), "Fixed it.\n\nThe details.")).unwrap();
    hook(&mut c, &run, "claude", "Stop", json!({ "transcript_path": transcript })).await;
    let i = queued(&home, &run.id).await;

    let leads = run_leads(&mut c).await;
    let lead = leads.iter().find(|l| l.key == format!("run:{}", run.id)).unwrap();
    assert_eq!(lead.band, Band::Happened);
    let rest = lead.rest.as_ref().unwrap();
    assert_eq!(rest.item, i);
    assert_eq!(rest.last.as_deref(), Some("Fixed it."), "its first paragraph");
    assert_eq!(rest.changed, None, "no repo, nothing said of git");

    let done: QueueReviewed = c.ok("queue.review", json!({ "runs": [run.id] })).await;
    assert_eq!(done.cleared, 1);
    assert_eq!(item(&home, &run.id), None);
    assert!(run_leads(&mut c).await.is_empty(), "a reviewed run at rest is not news");
    let again: QueueReviewed = c.ok("queue.review", json!({ "runs": [run.id] })).await;
    assert_eq!(again.cleared, 0, "nothing left to clear");
    h.stop().await;
}

#[tokio::test]
async fn typing_into_a_run_reviews_it() {
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
        json!({ "last_assistant_message": "Should I push?" }),
    )
    .await;
    queued(&home, &run.id).await;
    let _: () = c
        .ok("run.input", json!({ "id": run.id, "bytes": b"yes\n".to_vec() }))
        .await;
    assert_eq!(item(&home, &run.id), None, "answered: reviewed");
    h.stop().await;
}

#[tokio::test]
async fn item_view_reads_the_log_since_from() {
    let Some(Setup {
        _tmp,
        dir,
        home,
        h,
        mut c,
        run,
    }) = setup("claude").await
    else {
        return;
    };
    let transcript = dir.join("session.jsonl");
    let before = run.started_at - Duration::from_secs(3600);
    std::fs::write(
        &transcript,
        claude_line(before, "Old news.") + &claude_line(Timestamp::now(), "Fixed it."),
    )
    .unwrap();
    hook(&mut c, &run, "claude", "Stop", json!({ "transcript_path": transcript })).await;
    queued(&home, &run.id).await;
    let texts = |v: &ItemView| v.log.iter().map(|l| l.text.clone()).collect::<Vec<_>>();
    let view: ItemView = c.ok("queue.item", json!({ "run": run.id })).await;
    assert_eq!(texts(&view), ["Fixed it."], "since the run's start");
    assert_eq!((view.changed, view.branch), (None, None), "no repo");

    let _: QueueReviewed = c.ok("queue.review", json!({ "runs": [run.id] })).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let mut more = std::fs::read_to_string(&transcript).unwrap();
    more += &claude_line(Timestamp::now(), "Next.");
    std::fs::write(&transcript, more).unwrap();
    let view: ItemView = c.ok("queue.item", json!({ "run": run.id })).await;
    assert_eq!(texts(&view), ["Next."], "since the review");
    h.stop().await;
}

#[tokio::test]
async fn a_permission_ask_is_told_by_what_it_asks() {
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
    queued(&home, &run.id).await;
    let leads = run_leads(&mut c).await;
    let lead = leads.iter().find(|l| l.key == format!("run:{}", run.id)).unwrap();
    assert_eq!(lead.band, Band::NeedsYou);
    assert_eq!(
        lead.rest.as_ref().unwrap().last.as_deref(),
        Some("Claude needs your permission to use Bash")
    );
    h.stop().await;
}
