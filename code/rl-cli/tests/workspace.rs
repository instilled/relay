//! `rl workspace rm`: unregistering says so; `--archive` takes the folder's
//! ended runs off the list; `--move-rules` moves the rule files aside, but
//! only once someone said yes — with nobody to ask, it wants `--yes` and
//! does nothing without it.

use jiff::Timestamp;
use rl_ledger::events::NewEvent;
use rl_ledger::{Ledger, events, runs};
use rl_proto::types::{Run, RunKind, RunState, event_kind};
use std::path::{Path, PathBuf};
use std::process::Command;

fn rld_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name("rld");
    p.is_file().then_some(p)
}

#[test]
fn workspace_rm_archives_and_moves_the_rules_aside_when_told_to() {
    let Some(rld) = rld_bin() else {
        eprintln!("skipping: rld not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let run = |args: &[&str], cwd: &Path| {
        let out = Command::new(env!("CARGO_BIN_EXE_rl"))
            .args(args)
            .current_dir(cwd)
            .env("RELAY_HOME", &home)
            .env("RELAY_RLD_BIN", &rld)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let ws = std::fs::canonicalize(tmp.path()).unwrap().join("proj");
    std::fs::create_dir_all(ws.join(".relay")).unwrap();
    std::fs::write(
        ws.join(".relay/schedules.yaml"),
        "schedules:\n  nightly:\n    every: 1h\n    run: [true]\n",
    )
    .unwrap();
    let dir = ws.to_str().unwrap();
    let (ok, out, err) = run(&["up", dir, "--yes"], tmp.path());
    assert!(ok, "{out}{err}");

    // An ended run of the folder's, written as the daemon would have.
    let ledger = Ledger::open(&home.join("ledger.db")).unwrap();
    let now = Timestamp::now();
    let mut r = Run::new(
        "ended001",
        RunKind::Command,
        "build",
        vec!["true".into()],
        &ws,
        &ws,
        now,
    );
    r.state = RunState::Finished;
    r.ended_at = Some(now);
    ledger
        .call_blocking(move |c| {
            runs::insert(c, &r)?;
            events::append(c, &NewEvent::for_run(&r, now, event_kind::RUN_STARTED, "started"))?;
            Ok(())
        })
        .unwrap();

    // Nobody to ask: moving wants a yes said up front, and nothing is done.
    let (ok, _, err) = run(&["workspace", "rm", dir, "--move-rules"], tmp.path());
    assert!(!ok && err.contains("--yes"), "{err}");
    let (_, out, _) = run(&["workspace", "ls", "--json"], tmp.path());
    assert!(out.contains("proj"), "still registered: {out}");

    // Told, and named from where it is: relative to the caller, not the daemon.
    let (ok, out, err) = run(
        &[
            "workspace",
            "rm",
            "proj",
            "--archive",
            "--move-rules",
            "--yes",
            "--json",
        ],
        tmp.path(),
    );
    assert!(ok, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["removed"], true, "{v}");
    assert_eq!(v["archived"], 1, "{v}");
    assert_eq!(v["stopped"], 0, "{v}");
    let to = PathBuf::from(v["rules_moved_to"].as_str().unwrap());
    assert!(to.starts_with(home.join("removed")), "{}", to.display());
    assert!(to.join("schedules.yaml").is_file());
    assert!(!ws.join(".relay").exists(), "only rules were in it, so it went whole");
    let kept = ledger.call_blocking(|c| runs::get(c, "ended001")).unwrap().unwrap();
    assert!(kept.archived_at.is_some());

    // In words, a folder that is no longer registered says so.
    let (ok, out, _) = run(&["workspace", "rm", dir], tmp.path());
    assert!(ok);
    assert!(out.trim().ends_with("proj was not registered"), "{out}");

    // Registered again, it says what went.
    let (ok, _, _) = run(&["up", dir], tmp.path());
    assert!(ok);
    let (ok, out, _) = run(&["workspace", "rm", dir, "--stop", "--archive"], tmp.path());
    assert!(ok);
    assert!(out.trim().ends_with("proj unregistered; its rules stop"), "{out}");

    assert!(run(&["daemon", "stop"], tmp.path()).0);
}
