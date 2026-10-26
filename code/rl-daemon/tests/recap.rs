//! The agent's own recap, kept as it is written: Claude's from the transcript
//! its hook names, when a turn ends or it waits, and on from there as it
//! grows; Codex's last turn from its rollout. No assistant is asked; the
//! digest carries each row's first line, and only Claude's and Codex's.

mod common;

use common::{Conn, Harness};
use jiff::{SignedDuration, Timestamp};
use rl_core::Home;
use rl_ledger::{Ledger, summaries};
use rl_proto::brief::Brief;
use rl_proto::methods::{Digest, DigestItem};
use rl_proto::types::{Run, Summary};
use serde_json::json;
use std::path::Path;
use std::time::{Duration, Instant};

fn by_id<'a>(d: &'a Digest, id: &str) -> &'a DigestItem {
    d.items.iter().find(|i| i.run.id == id).expect("item in digest")
}

/// A stand-in agent: what makes a run Claude's or Codex's is the binary's name.
fn stand_in(dir: &Path, name: &str) -> std::path::PathBuf {
    let bin = dir.join(name);
    std::fs::write(&bin, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin
}

fn away(at: Timestamp, content: &str) -> String {
    format!(
        "{{\"type\":\"system\",\"subtype\":\"away_summary\",\"timestamp\":\"{at}\",\"content\":{}}}\n",
        serde_json::to_string(content).unwrap()
    )
}

fn append(p: &Path, text: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
    std::io::Write::write_all(&mut f, text.as_bytes()).unwrap();
}

/// Polls `summary.get` until the kept recap says `text`.
async fn wait_recap(c: &mut Conn, run: &str, text: &str) -> Summary {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let res = c
            .call("summary.get", json!({ "target": { "kind": "run", "id": run } }))
            .await;
        assert!(res.error.is_none(), "{:?}", res.error);
        let s: Option<Summary> = serde_json::from_value(res.result.unwrap_or_default()).unwrap();
        if let Some(s) = s.filter(|s| s.text == text) {
            return s;
        }
        assert!(Instant::now() < deadline, "the recap never said {text:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn claudes_recap_is_kept_as_it_writes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    // The assistant a written recap needs: a stand-in answering in three parts.
    let fake = std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-agent")).unwrap();
    std::fs::create_dir_all(home.detect_dir()).unwrap();
    std::fs::write(
        home.detect_dir().join("fakeai.yaml"),
        format!(
            "kind: agent\nbins: [\"{}\"]\nheadless: [\"--headless\"]\n",
            fake.display()
        ),
    )
    .unwrap();
    std::fs::write(home.config_file(), "assistants:\n  summary: fakeai\n").unwrap();
    let answer = tmp.path().join("answer.txt");
    std::fs::write(
        &answer,
        "Worked on: Fixed the search.\nOpen questions: none\nNext: Commit it.\n",
    )
    .unwrap();
    // SAFETY: this is the only test in this binary that asks an assistant.
    unsafe {
        std::env::set_var("FAKE_AGENT_ANSWER", &answer);
        std::env::set_var("FAKE_AGENT_READS", "1");
    }
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let mut c = Conn::open(&home).await;
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let bin = stand_in(&dir, "claude");
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [bin], "cwd": dir, "title": "fix search", "kind": "agent" }),
        )
        .await;
    assert_eq!(run.cli.as_deref(), Some("claude"));
    let shell: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "sleep 30"], "cwd": dir, "title": "a shell" }),
        )
        .await;

    // One from a session before this run, and this run's own, with the hint
    // Claude shows on its screen.
    let transcript = dir.join("session.jsonl");
    let old = Timestamp::now() - SignedDuration::from_hours(3);
    std::fs::write(
        &transcript,
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"I fixed the search index.\"}}]}}}}\n",
            Timestamp::now()
        ) + &away(old, "Old news.")
            + &away(
                Timestamp::now(),
                "Search fixed; nothing committed. Next: say commit. (disable recaps in /config)",
            ),
    )
    .unwrap();
    let _: Run = c
        .ok(
            "run.hook",
            json!({ "id": run.id, "event": "Stop", "payload": { "transcript_path": transcript } }),
        )
        .await;
    let s = wait_recap(&mut c, &run.id, "Search fixed; nothing committed. Next: say commit.").await;
    assert_eq!(
        (s.agent.as_str(), s.source.as_str(), s.by.as_str()),
        ("claude", "claude recap", "")
    );
    assert!(!s.stale);
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let read = {
        let id = run.id.clone();
        ledger.call_blocking(move |c| summaries::read_to(c, &id)).unwrap()
    };
    assert_eq!(read, std::fs::metadata(&transcript).unwrap().len());

    // It is a seed: the pane's `summary.get` shows it, but nothing is written.
    assert!(
        ledger
            .call_blocking({
                let id = run.id.clone();
                move |c| summaries::get(c, &id)
            })
            .unwrap()
            .is_none()
    );

    // The digest carries it, as a row's line; a shell's summary is its own.
    {
        let id = shell.id.clone();
        let asked = Summary {
            run: id,
            agent: "haiku".into(),
            by: "a1".into(),
            at: Timestamp::now(),
            text: "It slept.".into(),
            since: None,
            source: "summarised".into(),
            reach: "o:0".into(),
            stale: false,
            target: String::new(),
            deep: false,
            from: None,
            calls: 0,
            runs: 1,
        };
        ledger.call_blocking(move |c| summaries::put(c, &asked)).unwrap();
    }
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert_eq!(
        by_id(&d, &run.id).recap.as_deref(),
        Some("Search fixed; nothing committed. Next: say commit.")
    );
    assert_eq!(by_id(&d, &shell.id).recap, None, "only Claude's and Codex's rows");

    // A newer recap, read on from where the last read stopped: a hook, or
    // the sweep with no hook at all.
    append(&transcript, &away(Timestamp::now(), "Committed; the PR is open."));
    let _: Run = c
        .ok(
            "run.hook",
            json!({ "id": run.id, "event": "Notification",
                    "payload": { "notification_type": "idle_prompt", "message": "Claude is waiting for your input", "transcript_path": transcript } }),
        )
        .await;
    wait_recap(&mut c, &run.id, "Committed; the PR is open.").await;
    // Newer by the clock the ledger keeps, in milliseconds: one written in
    // the same one is not newer, and is not kept.
    tokio::time::sleep(Duration::from_millis(2)).await;
    append(&transcript, &away(Timestamp::now(), "Merged.\nNothing left."));
    wait_recap(&mut c, &run.id, "Merged.\nNothing left.").await;
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert_eq!(by_id(&d, &run.id).recap.as_deref(), Some("Merged."), "its first line");

    // `/summary` writes a recap — an assistant is asked, the agent's own
    // being its seed. What is written is what `get` gives from then on; the
    // seed stays where it was, and is never taken for a written one.
    let made: Summary = c
        .ok("summary.make", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    assert!(!made.by.is_empty(), "written by an assistant: {made:?}");
    assert_eq!(made.source, "claude");
    assert_eq!(
        made.text,
        "Worked on: Fixed the search.\nOpen questions: none\nNext: Commit it."
    );
    assert!(Brief::parse(&made.text).worked_on.is_some());
    let got: Summary = c
        .ok("summary.get", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    assert_eq!(
        (got.by.as_str(), got.text.as_str()),
        (made.by.as_str(), made.text.as_str())
    );
    let id = run.id.clone();
    let (seed, unwritten, written) = ledger
        .call_blocking(move |c| {
            let n = |sql: &str| c.query_row(sql, [&id], |r| r.get::<_, i64>(0));
            Ok((
                rl_ledger::seeds::get(c, &id)?,
                n("SELECT count(*) FROM summaries WHERE run = ?1 AND by_run = ''")?,
                n("SELECT count(*) FROM summaries WHERE run = ?1 AND by_run != ''")?,
            ))
        })
        .unwrap();
    assert_eq!(seed.map(|s| s.text).as_deref(), Some("Merged.\nNothing left."));
    assert_eq!((unwritten, written), (0, 1), "no seed posing as a written recap");

    for id in [&run.id, &shell.id] {
        let _: serde_json::Value = c.ok("run.stop", json!({ "id": id, "force": true })).await;
    }
    h.stop().await;
}

#[tokio::test]
async fn a_picked_up_recap_is_a_seed_not_a_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let mut c = Conn::open(&home).await;
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let bin = stand_in(&dir, "claude");
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [bin], "cwd": dir, "title": "fix search", "kind": "agent" }),
        )
        .await;
    let transcript = dir.join("session.jsonl");
    std::fs::write(&transcript, away(Timestamp::now(), "Search fixed.")).unwrap();
    let _: Run = c
        .ok(
            "run.hook",
            json!({ "id": run.id, "event": "Stop", "payload": { "transcript_path": transcript } }),
        )
        .await;
    wait_recap(&mut c, &run.id, "Search fixed.").await;

    let ledger = Ledger::open(&home.ledger()).unwrap();
    let id = run.id.clone();
    let (seed, written, facts) = ledger
        .call_blocking(move |c| {
            Ok((
                rl_ledger::seeds::get(c, &id)?,
                summaries::get(c, &id)?,
                rl_ledger::facts::get(c, &id)?,
            ))
        })
        .unwrap();
    let seed = seed.expect("the recap is a seed");
    assert_eq!((seed.text.as_str(), seed.by.as_str()), ("Search fixed.", ""));
    assert!(written.is_none(), "nothing written");
    assert!(facts.is_none(), "the run's facts are untouched");

    let _: serde_json::Value = c.ok("run.stop", json!({ "id": run.id, "force": true })).await;
    h.stop().await;
}

#[tokio::test]
async fn codex_last_turn_is_its_recap() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let mut c = Conn::open(&home).await;
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let bin = stand_in(&dir, "codex");
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [bin], "cwd": dir, "title": "parser", "kind": "agent" }),
        )
        .await;
    assert_eq!(run.cli.as_deref(), Some("codex"));

    // Its rollout, as `rld` would have found it in `~/.codex/sessions`.
    let rollout = dir.join("rollout.jsonl");
    let turn = |message: &str| {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\"last_agent_message\":{}}}}}\n",
            Timestamp::now(),
            serde_json::to_string(message).unwrap()
        )
    };
    std::fs::write(
        &rollout,
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"exec_command_end\"}}}}\n{}",
            Timestamp::now(),
            turn("Tests pass.")
        ),
    )
    .unwrap();
    {
        let ledger = Ledger::open(&home.ledger()).unwrap();
        let (id, path) = (run.id.clone(), rollout.to_string_lossy().into_owned());
        ledger
            .call_blocking(move |c| summaries::set_transcript(c, &id, &path))
            .unwrap();
    }
    // Waiting on you, which is when the sweep looks.
    let _: Run = c.ok("run.mark", json!({ "id": run.id, "mark": "done" })).await;
    let s = wait_recap(&mut c, &run.id, "Tests pass.").await;
    assert_eq!(
        (s.agent.as_str(), s.source.as_str(), s.by.as_str()),
        ("codex", "codex turn", "")
    );

    // The next turn, and then the run ends: its last turn is kept.
    append(&rollout, &turn("Fixed the parser.\nDetails follow."));
    let _: serde_json::Value = c.ok("run.stop", json!({ "id": run.id, "force": true })).await;
    c.wait_terminal(&run.id).await;
    wait_recap(&mut c, &run.id, "Fixed the parser.\nDetails follow.").await;
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert_eq!(by_id(&d, &run.id).recap.as_deref(), Some("Fixed the parser."));
    h.stop().await;
}
