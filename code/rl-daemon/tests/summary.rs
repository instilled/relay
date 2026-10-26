//! `summary.make` / `summary.get`: a run's facts and output go to
//! `assistants.summary` as an assistant run; the answer is kept by the run
//! and said in a `run.summarised` event, and given back while the run has not
//! moved on. A stretch with nothing in it needs no assistant; any other
//! brief is written, with the run's facts and the agent's own recap as a
//! seed. An assistant's own run is refused.

mod common;

use common::{Conn, Harness, wait_journal};
use rl_core::Home;
use rl_ledger::Ledger;
use rl_proto::brief::Brief;
use rl_proto::facts::RunFacts;
use rl_proto::rpc::Response;
use rl_proto::types::{Cause, Event, Run, RunState, Summary};
use serde_json::json;
use std::path::PathBuf;

/// The assistant stand-in is steered by process-wide variables: one test at a time.
static ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn fake_agent() -> PathBuf {
    std::fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-agent")).unwrap()
}

#[tokio::test]
async fn an_ended_run_gets_a_summary_kept_by_it() {
    let _env = ENV.lock().await;
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
    std::fs::write(home.config_file(), "assistants:\n  summary: fakeai\n").unwrap();
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
    let nothing: Response = c
        .call("summary.get", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    assert!(nothing.error.is_none(), "{nothing:?}");
    assert!(nothing.result.unwrap_or(serde_json::Value::Null).is_null());
    c.wait_terminal(&run.id).await;
    wait_journal(&home, &run.id, "hello from the run").await;

    let s: Summary = c
        .ok("summary.make", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    assert_eq!(s.run, run.id);
    assert_eq!(s.agent, "fakeai");
    assert_eq!(
        s.text, "It printed hello and left.\nNothing changed.",
        "fences dropped, kept as prose"
    );
    let by: Run = c.get(&s.by).await;
    assert_eq!(by.cause, Cause::Assistant("summary".into()));
    assert_eq!(by.title, "summary: say hello");
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
    let kept: Option<Summary> = c
        .ok("summary.get", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    assert_eq!(kept.as_ref(), Some(&s));
    let same: Summary = c
        .ok("summary.make", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    assert_eq!(same.by, s.by);
    let again: Summary = c
        .ok(
            "summary.make",
            json!({ "target": { "kind": "run", "id": run.id }, "again": true }),
        )
        .await;
    assert_ne!(again.by, s.by);
    let kept: Option<Summary> = c
        .ok("summary.get", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    assert_eq!(kept.map(|k| k.by), Some(again.by.clone()));

    // Said as an event on the run, first line only.
    let evs: Vec<Event> = c
        .ok("event.list", json!({ "run": run.id, "kinds": ["run.summarised"] }))
        .await;
    assert_eq!(evs.len(), 2, "{evs:?}");
    assert_eq!(evs[0].summary, "It printed hello and left.");

    // An assistant's own run is not summarised.
    let r: Response = c
        .call("summary.make", json!({ "target": { "kind": "run", "id": again.by } }))
        .await;
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
    let from = jiff::Timestamp::from_millisecond(jiff::Timestamp::now().as_millisecond() - 60_000).unwrap();
    let quiet: Summary = c
        .ok(
            "summary.make",
            json!({ "target": { "kind": "run", "id": slow.id }, "from": from }),
        )
        .await;
    assert!(quiet.text.starts_with("nothing happened since "), "{}", quiet.text);
    assert_eq!((quiet.source.as_str(), quiet.by.as_str()), ("nothing new", ""));
    assert_eq!(quiet.from, Some(from));
    let _: serde_json::Value = c.ok("run.stop", json!({ "id": slow.id, "force": true })).await;

    // The CLI's own warning about its input is not the answer.
    std::fs::write(
        &answer,
        "Warning: no stdin data received in 3s, proceeding without it. If piping from a slow command, \
         redirect stdin explicitly: < /dev/null to skip, or wait longer.\nIt printed hello and left.\n",
    )
    .unwrap();
    let warned: Summary = c
        .ok(
            "summary.make",
            json!({ "target": { "kind": "run", "id": run.id }, "again": true }),
        )
        .await;
    assert_eq!(warned.text, "It printed hello and left.");

    // Nothing said: an error that names the assistant's run.
    std::fs::write(&answer, "\n").unwrap();
    let r: Response = c
        .call(
            "summary.make",
            json!({ "target": { "kind": "run", "id": run.id }, "again": true }),
        )
        .await;
    let msg = r.error.unwrap().message;
    assert!(msg.contains("said nothing"), "{msg}");

    unsafe {
        std::env::remove_var("FAKE_AGENT_ANSWER");
        std::env::remove_var("FAKE_AGENT_PROMPT");
        std::env::remove_var("FAKE_AGENT_READS");
    }
    h.stop().await;
}

/// A home with `fakeai` as the summary assistant, answering with a brief in
/// three parts and writing the prompt it was given to `prompt.txt`.
struct Fake {
    tmp: tempfile::TempDir,
    home: Home,
    prompt: PathBuf,
}

fn fake_home() -> Fake {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    home.ensure_dirs().unwrap();
    std::fs::create_dir_all(home.detect_dir()).unwrap();
    std::fs::write(
        home.detect_dir().join("fakeai.yaml"),
        format!(
            "kind: agent\nbins: [\"{}\"]\nheadless: [\"--headless\"]\n",
            fake_agent().display()
        ),
    )
    .unwrap();
    std::fs::write(home.config_file(), "assistants:\n  summary: fakeai\n").unwrap();
    let answer = tmp.path().join("answer.txt");
    let prompt = tmp.path().join("prompt.txt");
    std::fs::write(
        &answer,
        "Worked on: Fixed the search.\nOpen questions: none\nNext: Commit it.\n",
    )
    .unwrap();
    // SAFETY: every test that sets these holds `ENV` for its whole run.
    unsafe {
        std::env::set_var("FAKE_AGENT_ANSWER", &answer);
        std::env::set_var("FAKE_AGENT_PROMPT", &prompt);
        std::env::set_var("FAKE_AGENT_READS", "1");
    }
    Fake { tmp, home, prompt }
}

fn unset_fake() {
    // SAFETY: as in `fake_home`.
    unsafe {
        std::env::remove_var("FAKE_AGENT_ANSWER");
        std::env::remove_var("FAKE_AGENT_PROMPT");
        std::env::remove_var("FAKE_AGENT_READS");
    }
}

/// Sets a run's facts as given (or none), once the daemon's own collection on
/// its ending has written, so it cannot overwrite them.
async fn freeze_facts(ledger: &Ledger, run: &str, facts: Option<RunFacts>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let id = run.to_string();
        let found = ledger.call_blocking(move |c| rl_ledger::facts::get(c, &id)).unwrap();
        if found.is_some() || std::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let id = run.to_string();
    ledger
        .call_blocking(move |c| match &facts {
            Some(f) => rl_ledger::facts::set(c, f),
            None => c
                .execute("DELETE FROM run_facts WHERE run = ?1", [&id])
                .map(|_| ())
                .map_err(Into::into),
        })
        .unwrap();
}

/// A stand-in `claude` that sleeps, and its session file with the agent's
/// own recap in it, named by a `Stop` hook; waits for the seed to be kept.
async fn claude_with_recap(c: &mut Conn, dir: &std::path::Path, recap: &str) -> (Run, jiff::Timestamp) {
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
    let transcript = dir.join("session.jsonl");
    let old = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(3);
    // Within the slack before the run began, so it is this run's; what was
    // said comes a few seconds after it.
    let recap_at = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(5);
    let recent = recap_at + jiff::SignedDuration::from_secs(5);
    std::fs::write(
        &transcript,
        format!(
            "{{\"type\":\"system\",\"subtype\":\"away_summary\",\"timestamp\":\"{old}\",\"content\":\"Old news.\"}}\n\
             {{\"type\":\"system\",\"subtype\":\"away_summary\",\"timestamp\":\"{recap_at}\",\"content\":\"{recap}\"}}\n\
             {{\"type\":\"assistant\",\"timestamp\":\"{recent}\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"I fixed the search index.\"}}]}}}}\n"
        ),
    )
    .unwrap();
    let _: Run = c
        .ok(
            "run.hook",
            json!({ "id": run.id, "event": "Stop", "payload": { "transcript_path": transcript } }),
        )
        .await;
    // The pick-up is asynchronous: the seed shows through `summary.get`.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let s: Option<Summary> = c
            .ok("summary.get", json!({ "target": { "kind": "run", "id": run.id } }))
            .await;
        if s.is_some_and(|s| s.text == recap) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the recap was never picked up");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    (run, recap_at)
}

#[tokio::test]
async fn a_claude_brief_is_written_with_its_recap_as_a_seed() {
    let _env = ENV.lock().await;
    let f = fake_home();
    let Some(h) = Harness::on(f.home.clone()) else {
        eprintln!("skipping: rl-session not built");
        unset_fake();
        return;
    };
    let mut c = Conn::open(&f.home).await;
    let dir = std::fs::canonicalize(f.tmp.path()).unwrap();
    let (run, recap_at) = claude_with_recap(&mut c, &dir, "Search fixed; nothing committed.").await;

    // Picked up, the recap is shown as a seed: the agent's, nobody's brief.
    let seen: Summary = c
        .ok("summary.get", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    assert_eq!(
        (seen.by.as_str(), seen.source.as_str(), seen.text.as_str()),
        ("", "claude recap", "Search fixed; nothing committed.")
    );

    // The run ends and its facts are frozen.
    let _: serde_json::Value = c.ok("run.stop", json!({ "id": run.id, "force": true })).await;
    c.wait_terminal(&run.id).await;
    let ledger = Ledger::open(&f.home.ledger()).unwrap();
    let facts = RunFacts {
        run: run.id.clone(),
        commits: 3,
        added: 180,
        removed: 40,
        files: 4,
        top_paths: vec!["auth/".into()],
        ..Default::default()
    };
    freeze_facts(&ledger, &run.id, Some(facts)).await;

    assert!(!f.prompt.exists(), "no assistant yet");
    let s: Summary = c
        .ok("summary.make", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    let p = std::fs::read_to_string(&f.prompt).expect("the assistant was asked");
    for want in ["## The facts", "+180/\u{2212}40", "## Its own recap", "Search fixed"] {
        assert!(p.contains(want), "{want}: {p}");
    }
    assert!(!s.by.is_empty(), "an assistant wrote it: {s:?}");
    assert_eq!(s.source, "claude");
    assert!(Brief::parse(&s.text).worked_on.is_some(), "{}", s.text);

    // What is written is what `get` gives back; the seed lives apart.
    let got: Summary = c
        .ok("summary.get", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    assert_eq!((got.by.as_str(), got.text.as_str()), (s.by.as_str(), s.text.as_str()));
    let id = run.id.clone();
    let (seeds, seeded, unwritten) = ledger
        .call_blocking(move |c| {
            let n = |sql: &str| c.query_row(sql, [&id], |r| r.get::<_, i64>(0));
            Ok((
                n("SELECT count(*) FROM summaries WHERE run = ?1 AND by_run != ''")?,
                n("SELECT count(*) FROM recap_seeds WHERE run = ?1 AND text = 'Search fixed; nothing committed.'")?,
                n("SELECT count(*) FROM summaries WHERE run = ?1 AND by_run = ''")?,
            ))
        })
        .unwrap();
    assert_eq!((seeds, seeded, unwritten), (1, 1, 0));

    // A seed from before the stretch is not handed on.
    std::fs::remove_file(&f.prompt).unwrap();
    let _: Summary = c
        .ok(
            "summary.make",
            json!({ "target": { "kind": "run", "id": run.id }, "again": true,
                    "from": recap_at + jiff::SignedDuration::from_secs(1) }),
        )
        .await;
    let p = std::fs::read_to_string(&f.prompt).unwrap();
    assert!(!p.contains("## Its own recap"), "{p}");

    drop(ledger);
    h.stop().await;
    unset_fake();
}

#[tokio::test]
async fn a_seed_alone_never_satisfies_the_kept_check() {
    let _env = ENV.lock().await;
    let f = fake_home();
    let Some(h) = Harness::on(f.home.clone()) else {
        eprintln!("skipping: rl-session not built");
        unset_fake();
        return;
    };
    let mut c = Conn::open(&f.home).await;
    let dir = std::fs::canonicalize(f.tmp.path()).unwrap();
    let (run, _) = claude_with_recap(&mut c, &dir, "Search fixed.").await;

    // The seed is there and its reach matches the transcript's; still,
    // `make` asks an assistant, and what it makes is kept, not the seed.
    let seed: Summary = c
        .ok("summary.get", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    assert_eq!(seed.by, "");
    assert!(!seed.stale);
    assert!(!f.prompt.exists());
    let made: Summary = c
        .ok("summary.make", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    assert!(f.prompt.exists(), "the assistant was asked");
    assert!(!made.by.is_empty(), "{made:?}");
    let same: Summary = c
        .ok("summary.make", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    assert_eq!(same.by, made.by, "the written one is kept");

    let _: serde_json::Value = c.ok("run.stop", json!({ "id": run.id, "force": true })).await;
    h.stop().await;
    unset_fake();
}

#[tokio::test]
async fn a_shell_run_is_briefed_from_its_output() {
    let _env = ENV.lock().await;
    let f = fake_home();
    let Some(h) = Harness::on(f.home.clone()) else {
        eprintln!("skipping: rl-session not built");
        unset_fake();
        return;
    };
    let mut c = Conn::open(&f.home).await;
    let cwd = f.tmp.path().to_string_lossy().into_owned();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "echo built the thing"], "cwd": cwd, "title": "build" }),
        )
        .await;
    c.wait_terminal(&run.id).await;
    wait_journal(&f.home, &run.id, "built the thing").await;
    // No facts row: as for a run whose collection found nothing.
    let ledger = Ledger::open(&f.home.ledger()).unwrap();
    freeze_facts(&ledger, &run.id, None).await;
    let s: Summary = c
        .ok("summary.make", json!({ "target": { "kind": "run", "id": run.id } }))
        .await;
    let p = std::fs::read_to_string(&f.prompt).unwrap();
    assert!(p.contains("built the thing"), "{p}");
    assert!(!p.contains("## The facts") && !p.contains("## Its own recap"), "{p}");
    assert_eq!(s.source, "output");
    assert!(!s.by.is_empty());
    h.stop().await;
    unset_fake();
}

/// How many times the assistant has run: its own runs in the ledger.
fn assistant_runs(ledger: &Ledger) -> usize {
    ledger
        .call_blocking(|c| {
            rl_ledger::runs::list(
                c,
                &rl_proto::methods::RunFilter {
                    archived: true,
                    limit: Some(1000),
                    ..Default::default()
                },
            )
        })
        .unwrap()
        .iter()
        .filter(|r| matches!(r.cause, Cause::Assistant(_)))
        .count()
}

/// Three ended shell runs that said something each, and a time before them.
async fn three_shell_runs(c: &mut Conn, f: &Fake, h: &Harness) -> (Vec<Run>, jiff::Timestamp) {
    let from = jiff::Timestamp::now() - jiff::SignedDuration::from_mins(5);
    let cwd = f.tmp.path().to_string_lossy().into_owned();
    let mut runs = Vec::new();
    for t in ["a", "b", "c"] {
        let run: Run = c
            .ok(
                "run.start",
                json!({ "argv": ["sh", "-c", format!("echo said-{t}")], "cwd": cwd, "title": t }),
            )
            .await;
        c.wait_terminal(&run.id).await;
        wait_journal(&h.home, &run.id, &format!("said-{t}")).await;
        runs.push(run);
    }
    (runs, from)
}

#[tokio::test]
async fn over_brief_writes_each_runs_brief_then_joins() {
    let _env = ENV.lock().await;
    let f = fake_home();
    let Some(h) = Harness::on(f.home.clone()) else {
        eprintln!("skipping: rl-session not built");
        unset_fake();
        return;
    };
    let mut c = Conn::open(&f.home).await;
    let ledger = Ledger::open(&f.home.ledger()).unwrap();
    let (runs, from) = three_shell_runs(&mut c, &f, &h).await;
    let make = json!({ "target": { "kind": "all" }, "from": from });
    let s: Summary = c.ok("summary.make", make.clone()).await;
    assert_eq!((s.calls, s.runs), (4, 3), "{s:?}");
    assert_eq!(assistant_runs(&ledger), 4, "three briefs and the join");
    // Each run's own brief is kept for it.
    for r in &runs {
        let k: Summary = c
            .ok("summary.get", json!({ "target": { "kind": "run", "id": r.id } }))
            .await;
        assert!(k.text.starts_with("Worked on:"), "{k:?}");
        // Read for the over, not news on the run: no `run.summarised`.
        let evs: Vec<Event> = c
            .ok("event.list", json!({ "run": r.id, "kinds": ["run.summarised"] }))
            .await;
        assert!(evs.is_empty(), "{evs:?}");
    }
    // The same again: the join is kept, and no run is asked again.
    let again: Summary = c.ok("summary.make", make.clone()).await;
    assert_eq!(assistant_runs(&ledger), 4, "nothing new: {again:?}");
    // Re-joining (`again`) asks once, and the kept briefs are reused.
    let mut redo = make.clone();
    redo["again"] = json!(true);
    let s: Summary = c.ok("summary.make", redo).await;
    assert_eq!(s.calls, 1, "{s:?}");
    assert_eq!(assistant_runs(&ledger), 5, "only the join was asked");
    h.stop().await;
    unset_fake();
}

#[tokio::test]
async fn over_dry_counts_one_per_unkept_run_plus_the_join() {
    let _env = ENV.lock().await;
    let f = fake_home();
    let Some(h) = Harness::on(f.home.clone()) else {
        eprintln!("skipping: rl-session not built");
        unset_fake();
        return;
    };
    let mut c = Conn::open(&f.home).await;
    let (runs, from) = three_shell_runs(&mut c, &f, &h).await;
    let dry = json!({ "target": { "kind": "all" }, "from": from, "dry": true });
    let s: Summary = c.ok("summary.make", dry.clone()).await;
    assert_eq!((s.calls, s.runs), (4, 3), "{s:?}");
    assert!(s.text.is_empty());
    // One run's brief kept: one call fewer.
    let _: Summary = c
        .ok("summary.make", json!({ "target": { "kind": "run", "id": runs[0].id } }))
        .await;
    let s: Summary = c.ok("summary.make", dry).await;
    assert_eq!((s.calls, s.runs), (3, 3), "{s:?}");
    h.stop().await;
    unset_fake();
}
