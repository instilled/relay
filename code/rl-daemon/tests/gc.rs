//! `gc.run`: the journal of runs ended longer ago than retention says goes —
//! bytes, log and sidecars — the run and its events stay, and an event says
//! so. A recent run is untouched. With nothing set anywhere the default
//! stands (30d); only `retention: never` holds everything.

mod common;

use common::{Conn, Harness, wait_journal};
use jiff::{SignedDuration, Timestamp};
use rl_core::Home;
use rl_ledger::{Ledger, firings, runs, session_io};
use rl_proto::flow::{Firing, FiringStatus};
use rl_proto::methods::GcReport;
use rl_proto::rpc::Response;
use rl_proto::types::{Event, Run, RunState, Source};
use serde_json::json;

#[tokio::test]
async fn old_journals_go_and_the_facts_stay() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let mut c = Conn::open(&home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let old: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "echo old words; exit 0"], "cwd": cwd }),
        )
        .await;
    let fresh: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "echo fresh words; exit 0"], "cwd": cwd }),
        )
        .await;
    c.wait_terminal(&old.id).await;
    c.wait_terminal(&fresh.id).await;
    wait_journal(&home, &old.id, "old words").await;
    wait_journal(&home, &fresh.id, "fresh words").await;

    // `old` ended forty days ago, as far as the ledger knows.
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let ago = Timestamp::now() - SignedDuration::from_hours(40 * 24);
    let id = old.id.clone();
    ledger
        .call_blocking(move |db| runs::finish(db, &id, RunState::Finished, Source::Pty, Some(0), ago))
        .unwrap();
    let log = home.session_log(&old.id);
    assert!(log.exists(), "the holder's log is there before");

    // Nothing in the file and nothing asked: the default, not a refusal.
    let r: GcReport = c.ok("gc.run", json!({ "dry_run": true })).await;
    assert_eq!(
        (r.runs, r.from.as_str(), r.older_than.as_str()),
        (1, "default", "30d"),
        "{r:?}"
    );

    // `never` is the way to keep everything, and it is refused loudly.
    std::fs::write(home.config_file(), "retention: never\n").unwrap();
    let _: serde_json::Value = c.ok("config.get", json!(null)).await;
    let r: Response = c.call("gc.run", json!({})).await;
    assert!(r.error.unwrap().message.contains("`never`"));
    let id = old.id.clone();
    let still = ledger.call_blocking(move |db| session_io::out_bytes(db, &id)).unwrap();
    assert!(still > 0, "`never` kept it");

    // Asked for 30d, dry: one run would go, with its bytes and files.
    let r: GcReport = c.ok("gc.run", json!({ "older_than": "30d", "dry_run": true })).await;
    assert_eq!((r.runs, r.dry_run, r.from.as_str()), (1, true, "asked"), "{r:?}");
    assert!(r.bytes > 0 && r.files >= 1, "{r:?}");
    let id = old.id.clone();
    let still = ledger.call_blocking(move |db| session_io::out_bytes(db, &id)).unwrap();
    assert!(still > 0, "dry run keeps the journal");

    // For real, through the config.
    std::fs::write(home.config_file(), "retention: 30d\n").unwrap();
    let _: serde_json::Value = c.ok("config.get", json!(null)).await;
    let r: GcReport = c.ok("gc.run", json!({})).await;
    assert_eq!(
        (r.runs, r.dry_run, r.from.as_str(), r.older_than.as_str()),
        (1, false, "config", "30d"),
        "{r:?}"
    );
    let id = old.id.clone();
    let gone = ledger.call_blocking(move |db| session_io::out_bytes(db, &id)).unwrap();
    assert_eq!(gone, 0, "the old journal went");
    assert!(!log.exists(), "and its log");
    let id = fresh.id.clone();
    let kept = ledger.call_blocking(move |db| session_io::out_bytes(db, &id)).unwrap();
    assert!(kept > 0, "the fresh one stays");
    assert!(home.session_log(&fresh.id).exists());

    // The run and its events stay, and one more event says what happened.
    let run = c.get(&old.id).await;
    assert_eq!(run.state, RunState::Finished);
    let evs: Vec<Event> = c.ok("event.list", json!({ "run": old.id })).await;
    let dropped = evs.iter().find(|e| e.kind == "run.journal_dropped").expect("the event");
    assert!(dropped.summary.starts_with("journal dropped · "), "{}", dropped.summary);
    assert!(dropped.data.as_ref().unwrap()["bytes"].as_u64().unwrap() > 0);
    assert!(evs.iter().any(|e| e.kind == "run.started"), "older events stay");

    // Nothing left to drop.
    let r: GcReport = c.ok("gc.run", json!({})).await;
    assert_eq!(r.runs, 0);

    h.stop().await;
}

/// prod-logs keeps `runs/seen` beside its firings, by hand: retention takes old firings' folders
/// and leaves that, a folder of the flow's own, and a recent firing's, alone.
#[tokio::test]
async fn a_firings_folder_goes_with_its_runs_and_nothing_else_under_runs_does() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path().join("home"));
    let dir = home.flow_dir("keeps");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("flow.md"),
        "---\nsteps:\n  a:\n    run: 'echo x > \"$RELAY_WORK/kept\"'\n---\n",
    )
    .unwrap();
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let mut c = Conn::open(&home).await;
    common::approve(&mut c, "keeps").await;
    let mut ids = Vec::new();
    for _ in 0..2 {
        let f: Firing = c.ok("flow.run", json!({ "name": "keeps" })).await;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let fs: Vec<Firing> = c.ok("flow.firings", json!({ "flow": "keeps" })).await;
            if fs.iter().any(|x| x.id == f.id && x.ended_at.is_some()) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "{fs:?}");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        ids.push(f.id);
    }
    let (old, recent) = (ids[0].clone(), ids[1].clone());
    // `old` ended forty days ago, as far as the ledger knows.
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let ago = Timestamp::now() - SignedDuration::from_hours(40 * 24);
    let id = old.clone();
    ledger
        .call_blocking(move |db| {
            let steps = firings::steps(db, &id)?;
            firings::save(db, &id, &steps, FiringStatus::Done, Some(ago))
        })
        .unwrap();
    let runs = dir.join("runs");
    std::fs::write(runs.join("seen"), "abc\n").unwrap();
    std::fs::create_dir_all(runs.join("notes")).unwrap();
    assert!(home.firing_dir("keeps", &old).join("kept").exists());

    let r: GcReport = c.ok("gc.run", json!({ "dry_run": true })).await;
    assert_eq!(r.firings, 1, "{r:?}");
    assert!(home.firing_dir("keeps", &old).exists(), "a dry run only counts");
    let r: GcReport = c.ok("gc.run", json!({})).await;
    assert_eq!(r.firings, 1, "{r:?}");
    assert!(!home.firing_dir("keeps", &old).exists());
    assert!(
        home.firing_dir("keeps", &recent).join("kept").exists(),
        "a recent firing's stays"
    );
    assert_eq!(
        std::fs::read_to_string(runs.join("seen")).unwrap(),
        "abc\n",
        "a file of the flow's own stays"
    );
    assert!(runs.join("notes").is_dir(), "a folder that is no firing stays");
    h.stop().await;
}
