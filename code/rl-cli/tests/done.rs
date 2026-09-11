//! `rl done`: an ended run leaves `rl ls` and `rl since` but stays in the
//! ledger and in `rl ls --all`; a run still going cannot be done with;
//! `--all` takes everything ended and looked at.

use jiff::Timestamp;
use relay_ledger::events::NewEvent;
use relay_ledger::{Ledger, events, runs};
use relay_proto::types::{Run, RunKind, RunState, event_kind};
use std::path::PathBuf;
use std::process::Command;

fn rld_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name("rld");
    p.is_file().then_some(p)
}

#[test]
fn done_hides_but_keeps() {
    let Some(rld) = rld_bin() else {
        eprintln!("skipping: rld not built (run `cargo test --workspace`)");
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
    assert!(run(&["daemon", "start"]).0);

    // Three runs, written as the daemon would have: two ended, one going.
    let ledger = Ledger::open(&tmp.path().join("ledger.db")).unwrap();
    let now = Timestamp::now();
    let mk = |id: &str, title: &str, state: RunState, seen: bool| {
        let mut r = Run::new(
            id,
            RunKind::Command,
            title,
            vec!["true".into()],
            "/w/repo",
            "/w/repo",
            now,
        );
        r.state = state;
        if state.is_terminal() {
            r.ended_at = Some(now);
            r.exit_code = Some(0);
        }
        if seen {
            r.seen_at = Some(now);
        }
        r
    };
    let seen = mk("seen0001", "seen and ended", RunState::Finished, true);
    let fresh = mk("fresh001", "ended, not looked at", RunState::Failed, false);
    let going = mk("going001", "still going", RunState::Working, false);
    ledger
        .call_blocking(move |c| {
            for r in [&seen, &fresh, &going] {
                runs::insert(c, r)?;
                events::append(c, &NewEvent::for_run(r, now, event_kind::RUN_STARTED, "started"))?;
            }
            Ok(())
        })
        .unwrap();

    // Done with one: gone from ls and since, there with --all, and the
    // ledger still has it.
    let (ok, out, _) = run(&["done", "seen0001"]);
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "1 done");
    let (_, out, _) = run(&["ls", "--all", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ids: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"seen0001"), "--all still lists it: {ids:?}");
    let (_, out, _) = run(&["since", "1h"]);
    assert!(!out.contains("seen0001") && out.contains("fresh001"), "{out}");
    let kept = ledger.call_blocking(|c| runs::get(c, "seen0001")).unwrap().unwrap();
    assert!(kept.archived_at.is_some());

    // A run still going is not something to be done with; a bad id neither.
    let (ok, _, err) = run(&["done", "going001"]);
    assert!(!ok && err.contains("still running"), "{err}");
    let (ok, _, err) = run(&["done", "nope0000"]);
    assert!(!ok, "{err}");
    let (ok, _, err) = run(&["done"]);
    assert!(!ok && err.contains("say which"), "{err}");

    // --all: only what has ended and been looked at. `fresh` has not been.
    let (ok, out, _) = run(&["done", "--all"]);
    assert!(ok);
    assert_eq!(out.trim(), "0 done");
    ledger
        .call_blocking(move |c| runs::set_seen(c, &["fresh001".into()], now))
        .unwrap();
    let (_, out, _) = run(&["done", "--all", "--json"]);
    assert_eq!(out.trim(), r#"{"archived":1}"#);
    let (_, out, _) = run(&["ls", "--all"]);
    assert!(out.contains("fresh001"), "still listed with --all: {out}");
    let (_, out, _) = run(&["ls"]);
    assert!(out.contains("going001") && !out.contains("fresh001"), "{out}");

    assert!(run(&["daemon", "stop"]).0);
}
