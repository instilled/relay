//! `rl up` (and its alias `rl add`) and `rl workspace rm`: registering says so; unregistering says so;
//! `--archive` takes the folder's ended runs off the list.

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
fn workspace_rm_unregisters_and_archives_when_told_to() {
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
    std::fs::create_dir_all(&ws).unwrap();
    let dir = ws.to_str().unwrap();
    let (ok, out, err) = run(&["up", dir], tmp.path());
    assert!(ok, "{out}{err}");
    assert_eq!(out.trim(), format!("workspace proj ({dir})"), "{out}");

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

    // Told, and named from where it is: relative to the caller, not the daemon.
    let (ok, out, err) = run(&["workspace", "rm", "proj", "--archive", "--json"], tmp.path());
    assert!(ok, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["removed"], true, "{v}");
    assert_eq!(v["archived"], 1, "{v}");
    assert_eq!(v["stopped"], 0, "{v}");
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
    assert!(out.trim().ends_with("proj unregistered"), "{out}");

    assert!(run(&["daemon", "stop"], tmp.path()).0);
}

#[test]
fn rl_add_registers_a_folder_as_rl_up_does() {
    let Some(rld) = rld_bin() else {
        eprintln!("skipping: rld not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let rl = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_rl"))
            .args(args)
            .current_dir(tmp.path())
            .env("RELAY_HOME", &home)
            .env("RELAY_RLD_BIN", &rld)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let ws = std::fs::canonicalize(tmp.path()).unwrap().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let dir = ws.to_str().unwrap();
    assert_eq!(rl(&["add", dir]), format!("workspace proj ({dir})"));
    assert!(rl(&["workspace", "ls"]).contains("proj"));
    rl(&["daemon", "stop"]);
}
