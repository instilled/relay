//! `rl ls` / `rl events` through the real binaries, rows seeded straight into
//! the ledger file the daemon is serving.

use jiff::Timestamp;
use relay_ledger::events::NewEvent;
use relay_ledger::{Ledger, events, runs};
use relay_proto::types::{Run, RunKind, event_kind};
use std::path::PathBuf;
use std::process::Command;

fn rld_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name("rld");
    p.is_file().then_some(p)
}

#[test]
fn ls_and_events() {
    let Some(rld) = rld_bin() else {
        eprintln!("skipping: rld binary not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_rl"))
            .args(args)
            .env("RELAY_HOME", tmp.path())
            .env("RELAY_RLD_BIN", &rld)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };

    let (ok, _, err) = run(&["ls"]);
    assert!(
        !ok && err.contains("rl daemon start"),
        "ls without daemon should say how to start it: {err}"
    );

    assert!(run(&["daemon", "start"]).0);
    let (ok, out, _) = run(&["ls"]);
    assert!(ok);
    assert_eq!(out.trim(), "no runs");
    let (ok, out, _) = run(&["ls", "--json"]);
    assert!(ok);
    assert_eq!(out.trim(), "[]");

    let ledger = Ledger::open(&tmp.path().join("ledger.db")).unwrap();
    let now = Timestamp::now();
    let mut r = Run::new(
        "k7x2m9qa",
        RunKind::Command,
        "make e2e",
        vec!["make".into(), "e2e".into()],
        "/w/repo",
        "/w/repo",
        now,
    );
    r.tags = vec!["e2e".into()];
    let ev = NewEvent::for_run(&r, now, event_kind::RUN_STARTED, "started make e2e");
    ledger
        .call_blocking(move |c| {
            runs::insert(c, &r)?;
            events::append(c, &ev)?;
            Ok(())
        })
        .unwrap();

    let (ok, out, _) = run(&["ls"]);
    assert!(ok);
    assert!(out.starts_with("ID  "), "{out}");
    assert!(
        out.contains("k7x2m9qa") && out.contains("starting") && out.contains("make e2e"),
        "{out}"
    );
    let (_, out, _) = run(&["ls", "--tag", "nope"]);
    assert_eq!(out.trim(), "no runs");
    let (_, out, _) = run(&["ls", "--tag", "e2e", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["id"], "k7x2m9qa");

    let (ok, out, _) = run(&["events"]);
    assert!(ok);
    assert!(out.contains("run.started") && out.contains("started make e2e"), "{out}");
    let (_, out, _) = run(&["events", "--since", "2999-01-01T00:00:00Z", "--json"]);
    assert_eq!(out.trim(), "[]");
    let (_, out, _) = run(&["events", "--since", "1h", "--kind", "run.started", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["kind"], "run.started");
    let (ok, _, err) = run(&["events", "--since", "yesterday"]);
    assert!(!ok && err.contains("not a duration"), "{err}");

    assert!(run(&["daemon", "stop"]).0);
}
