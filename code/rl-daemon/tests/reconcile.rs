//! J1: nothing dies when the daemon does. A second daemon on the same home
//! picks up the sessions the first one left, resuming the journal at the exact
//! byte it stopped at, or closes runs that ended while nobody was watching.
//!
//! What went wrong on the way is kept as a `run.trouble` event, so it is still
//! there tomorrow rather than having flashed past in somebody's footer.

mod common;

use common::{Conn, Harness, fake_cli, journal, wait_journal};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use rl_core::Home;
use rl_ledger::session_io::{self, Direction};
use rl_ledger::{Ledger, sessions};
use rl_proto::methods::{Digest, Resumed};
use rl_proto::types::{Event, Run, RunKind, RunState};
use serde_json::json;
use std::time::Duration;

fn session_row(home: &Home, id: &str) -> sessions::SessionRow {
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let id = id.to_string();
    ledger.call_blocking(move |db| sessions::get(db, &id)).unwrap().unwrap()
}

fn notes(home: &Home, id: &str) -> Vec<String> {
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let id = id.to_string();
    ledger
        .call_blocking(move |db| session_io::read(db, &id, 0, 10_000))
        .unwrap()
        .into_iter()
        .filter(|c| c.direction == Direction::Note)
        .map(|c| String::from_utf8_lossy(&c.data).into_owned())
        .collect()
}

async fn wait_gone(pid: u32) {
    for _ in 0..100 {
        if kill(Pid::from_raw(pid as i32), None).is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("pid {pid} still alive");
}

#[tokio::test]
async fn restart_resumes_the_journal_without_gaps_or_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let Some(h) = Harness::on(home.clone()) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let script = "echo one; sleep 1; echo two; sleep 1; echo three";
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", script], "cwd": cwd, "tty": false }),
        )
        .await;
    wait_journal(&home, &run.id, "one").await;

    // Daemon goes away; the run does not notice.
    drop(c);
    h.stop().await;
    let pid = session_row(&home, &run.id).pid;
    assert!(
        kill(Pid::from_raw(pid as i32), None).is_ok(),
        "child must survive the daemon"
    );
    tokio::time::sleep(Duration::from_millis(1300)).await; // "two" happens while nobody watches
    assert_eq!(
        journal(&home, &run.id),
        "one\n",
        "nothing journals while the daemon is down"
    );

    // A new daemon re-attaches from the journal's offset.
    let h = Harness::on(home.clone()).unwrap();
    let mut c = Conn::open(&home).await;
    let done = c.wait_terminal(&run.id).await;
    assert_eq!((done.state, done.exit_code), (RunState::Finished, Some(0)));
    assert_eq!(journal(&home, &run.id), "one\ntwo\nthree\n");
    assert_eq!(done.last_line.as_deref(), Some("three"));
    assert!(notes(&home, &run.id).is_empty(), "no gap expected");

    let evs: Vec<Event> = c.ok("event.list", json!({ "run": run.id })).await;
    let kinds: Vec<&str> = evs.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["run.started", "run.finished"]);
    assert!(session_row(&home, &run.id).closed_at.is_some());
    h.stop().await;
}

#[tokio::test]
async fn output_beyond_the_ring_is_noted_as_a_gap() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let Some(h) = Harness::on(home.clone()) else {
        return;
    };
    let mut c = Conn::open(&home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    // 2 MiB of x's while the daemon is down: more than the 1 MiB ring keeps.
    let script = "echo one; sleep 0.5; head -c 2097152 /dev/zero | tr '\\0' x; echo; echo end";
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", script], "cwd": cwd, "tty": false }),
        )
        .await;
    wait_journal(&home, &run.id, "one").await;
    drop(c);
    h.stop().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let h = Harness::on(home.clone()).unwrap();
    let mut c = Conn::open(&home).await;
    let done = c.wait_terminal(&run.id).await;
    assert_eq!(done.state, RunState::Finished);
    let text = journal(&home, &run.id);
    assert!(
        text.starts_with("one\n") && text.ends_with("\nend\n"),
        "{}…{}",
        &text[..8],
        &text[text.len() - 8..]
    );
    let notes = notes(&home, &run.id);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("bytes of output were lost"), "{}", notes[0]);
    // And it keeps: a warning nobody saw is a warning nobody can act on.
    let evs: Vec<Event> = c
        .ok("event.list", json!({ "run": run.id, "kinds": ["run.trouble"] }))
        .await;
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert!(evs[0].summary.contains("output were lost"), "{}", evs[0].summary);
    assert!(evs[0].data.as_ref().unwrap()["bytes"].as_u64().unwrap() > 0);
    h.stop().await;
}

#[tokio::test]
async fn exit_while_away_is_closed_from_the_sidecar() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let Some(h) = Harness::on(home.clone()) else {
        return;
    };
    let mut c = Conn::open(&home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "sleep 0.5; echo bye; exit 4"], "cwd": cwd, "tty": false }),
        )
        .await;
    drop(c);
    h.stop().await;

    // The run ends with nobody watching; then the holder itself goes away
    // (a reboot, or its linger expired) — only the sidecar is left.
    let row = session_row(&home, &run.id);
    for _ in 0..100 {
        if row.exit_file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(row.exit_file.exists(), "holder should have written the exit sidecar");
    kill(Pid::from_raw(row.holder_pid as i32), Signal::SIGTERM).unwrap();
    wait_gone(row.holder_pid).await;

    let h = Harness::on(home.clone()).unwrap();
    let mut c = Conn::open(&home).await;
    let done = c.get(&run.id).await;
    assert_eq!((done.state, done.exit_code), (RunState::Failed, Some(4)), "{done:?}");
    let evs: Vec<Event> = c
        .ok("event.list", json!({ "run": run.id, "kinds": ["run.failed"] }))
        .await;
    assert_eq!(evs.len(), 1);
    assert!(evs[0].summary.contains("exit 4"), "{}", evs[0].summary);
    assert!(session_row(&home, &run.id).closed_at.is_some());
    h.stop().await;
}

#[tokio::test]
async fn holder_lost_without_exit_is_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let Some(h) = Harness::on(home.clone()) else {
        return;
    };
    let mut c = Conn::open(&home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let run: Run = c.ok("run.start", json!({ "argv": [fake_cli()], "cwd": cwd })).await;
    drop(c);
    h.stop().await;

    let row = session_row(&home, &run.id);
    kill(Pid::from_raw(row.holder_pid as i32), Signal::SIGKILL).unwrap();
    wait_gone(row.holder_pid).await;
    let _ = kill(Pid::from_raw(row.pid as i32), Signal::SIGKILL); // orphaned child; tidy up

    let h = Harness::on(home.clone()).unwrap();
    let mut c = Conn::open(&home).await;
    let done = c.get(&run.id).await;
    assert_eq!(done.state, RunState::Failed);
    assert_eq!(done.exit_code, None);
    let evs: Vec<Event> = c
        .ok("event.list", json!({ "run": run.id, "kinds": ["run.failed"] }))
        .await;
    assert!(evs[0].summary.contains("went away"), "{}", evs[0].summary);
    // The diagnosis keeps too, beside the outcome: what the holder did, and
    // why rld could not reach it.
    let evs: Vec<Event> = c
        .ok("event.list", json!({ "run": run.id, "kinds": ["run.trouble"] }))
        .await;
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert!(
        evs[0].summary.contains("without recording an exit"),
        "{}",
        evs[0].summary
    );
    let active: Vec<Run> = c.ok("run.list", json!({ "active_only": true })).await;
    assert!(active.is_empty());
    h.stop().await;
}

/// A restart takes a shell and a command with it. Coming back, both are
/// marked lost and offered; resuming what was lost picks the shell back up
/// and leaves the command until it is asked for, since running it again could
/// do what it did twice. Each run picked up leaves the list for the one that
/// carries it on, and names it.
#[tokio::test]
async fn runs_lost_in_a_restart_are_offered_and_picked_back_up() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let Some(h) = Harness::on(home.clone()) else {
        return;
    };
    let mut c = Conn::open(&home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let shell: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["/bin/sh", "-i"], "kind": "shell", "cwd": cwd, "tags": ["shell"] }),
        )
        .await;
    assert_eq!(shell.kind, RunKind::Shell);
    let cmd: Run = c.ok("run.start", json!({ "argv": [fake_cli()], "cwd": cwd })).await;
    assert_eq!(cmd.kind, RunKind::Command);
    drop(c);
    h.stop().await;

    // The machine goes: holders and children with it, and no exit on record.
    for id in [&shell.id, &cmd.id] {
        let row = session_row(&home, id);
        kill(Pid::from_raw(row.holder_pid as i32), Signal::SIGKILL).unwrap();
        wait_gone(row.holder_pid).await;
        let _ = kill(Pid::from_raw(row.pid as i32), Signal::SIGKILL);
    }

    let h = Harness::on(home.clone()).unwrap();
    let mut c = Conn::open(&home).await;
    let d: Digest = c.ok("digest.get", json!({})).await;
    let mut lost = d.lost.clone();
    lost.sort();
    let mut want = vec![shell.id.clone(), cmd.id.clone()];
    want.sort();
    assert_eq!(lost, want, "both lost, and offered");
    let evs: Vec<Event> = c
        .ok("event.list", json!({ "run": shell.id, "kinds": ["run.lost"] }))
        .await;
    assert_eq!(evs.len(), 1, "{evs:?}");

    // What a restart took, less the command.
    let out: Vec<Resumed> = c.ok("run.resume", json!({ "lost": true })).await;
    assert_eq!(out.len(), 2, "{out:?}");
    let sh = out.iter().find(|r| r.from == shell.id).unwrap();
    let again = sh.run.as_ref().expect("the shell is picked back up");
    assert_eq!(again.kind, RunKind::Shell);
    assert_eq!(again.title, shell.title);
    assert!(
        again.tags.contains(&"shell".to_string()),
        "still a warm shell: {:?}",
        again.tags
    );
    assert!(
        again.tags.contains(&format!("resumes:{}", shell.id)),
        "{:?}",
        again.tags
    );
    let left = out.iter().find(|r| r.from == cmd.id).unwrap();
    assert!(left.run.is_none());
    assert_eq!(left.why_not.as_deref(), Some(rl_proto::methods::RESUME_LEFT_OUT));

    // The shell's old run has left the list; the command waits, still offered.
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert_eq!(d.lost, vec![cmd.id.clone()]);
    assert!(
        d.items.iter().all(|i| i.run.id != shell.id),
        "the old shell is archived"
    );

    // Asked for, the command runs again, and nothing lost is left.
    let out: Vec<Resumed> = c.ok("run.resume", json!({ "lost": true, "commands": true })).await;
    let rerun = out[0].run.as_ref().expect("the command runs again");
    assert_eq!(rerun.argv, cmd.argv);
    let d: Digest = c.ok("digest.get", json!({})).await;
    assert!(d.lost.is_empty(), "{:?}", d.lost);

    // A run still going is not picked up twice.
    let out: Vec<Resumed> = c.ok("run.resume", json!({ "ids": [again.id] })).await;
    assert!(out[0].run.is_none());
    assert!(out[0].why_not.as_deref().unwrap().contains("still going"));

    for id in [&again.id, &rerun.id] {
        let _: Run = c.ok("run.stop", json!({ "id": id, "force": true })).await;
        c.wait_terminal(id).await;
    }
    h.stop().await;
}
