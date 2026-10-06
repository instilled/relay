//! `rl since` through the real binaries: the catch-up since the last look —
//! the top, then each project and its rows — written by the summariser when
//! there is one to ask, counted when not.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

#[test]
fn since_groups_what_happened() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    // No assistant is asked here: the top is counted.
    std::fs::write(tmp.path().join("config.yaml"), "catch_up:\n  summary: ask\n").unwrap();
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_rl"))
            .args(args)
            .current_dir(tmp.path())
            .env("RELAY_HOME", tmp.path())
            .env("RELAY_RLD_BIN", &rld)
            .env("RELAY_SESSION_BIN", &session)
            .env_remove("RELAY_REF")
            .env_remove("RELAY_TAGS")
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    assert!(run(&["daemon", "start"]).0);
    let (ok, out, err) = run(&[
        "run",
        "--ref",
        "gh:7",
        "--title",
        "quick one",
        "--",
        "sh",
        "-c",
        "exit 3",
    ]);
    assert!(ok, "{err}");
    let id = out.trim().to_string();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, out, _) = run(&["ls", "--all", "--json"]);
        if out.contains("\"failed\"") {
            break;
        }
        assert!(Instant::now() < deadline, "never failed: {out}");
        std::thread::sleep(Duration::from_millis(100));
    }

    let (ok, out, err) = run(&["since"]);
    assert!(ok, "{err}");
    assert!(out.starts_with("while you were away · since "), "{out}");
    assert!(out.contains("\n  • 1 needs you: gh:7\n"), "counted: {out}");
    assert!(
        out.contains("\n▾ gh:7 · 1 needs you · 1 ran · 1 failed\n"),
        "grouped by ref: {out}"
    );
    assert!(out.contains(&format!(" {id}  quick one · failed")), "{out}");
    assert!(out.contains("exit 3"), "{out}");
    assert!(!err.contains("writing the top"), "{err}");

    let (ok, out, _) = run(&["since", "2999-01-01"]);
    assert!(ok);
    assert!(
        out.contains(&id),
        "a failed run always shows, whatever the boundary: {out}"
    );
    let (_, out, _) = run(&["since", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["items"][0]["run"]["id"], id);
    assert_eq!(v["items"][0]["unseen"], true);
    assert!(run(&["daemon", "stop"]).0);
}

fn fake_agent() -> PathBuf {
    std::fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-agent")).unwrap()
}

#[test]
fn since_has_the_top_written_only_as_you_come_back() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join("detect")).unwrap();
    std::fs::write(
        home.join("detect/fakeai.yaml"),
        format!(
            "kind: agent\nbins: [\"{}\"]\nheadless: [\"--headless\"]\n",
            fake_agent().display()
        ),
    )
    .unwrap();
    std::fs::write(home.join("config.yaml"), "assistants:\n  summary: fakeai\n").unwrap();
    // The assistant's run gets the daemon's environment, and the daemon is
    // started by the first `rl` here.
    let answer = tmp.path().join("answer.txt");
    std::fs::write(&answer, "• proj: it echoed, and that went fine\n").unwrap();
    let rl = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_rl"))
            .args(args)
            .current_dir(tmp.path())
            .env("RELAY_HOME", &home)
            .env("RELAY_RLD_BIN", &rld)
            .env("RELAY_SESSION_BIN", &session)
            .env("FAKE_AGENT_ANSWER", &answer)
            .env_remove("RELAY_RUN")
            .env_remove("RELAY_REF")
            .env_remove("RELAY_TAGS")
            .env_remove("FAKE_AGENT_SLEEP")
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    struct Stop(PathBuf, PathBuf);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = Command::new(env!("CARGO_BIN_EXE_rl"))
                .args(["daemon", "stop"])
                .env("RELAY_HOME", &self.0)
                .env("RELAY_RLD_BIN", &self.1)
                .output();
        }
    }
    let _stop = Stop(home.clone(), rld.clone());
    let (ok, id, err) = rl(&["run", "--title", "say hi", "--", "sh", "-c", "echo hi"]);
    assert!(ok, "{err}");
    let id = id.trim().to_string();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, out, _) = rl(&["ls", "--all", "--json"]);
        if out.contains("\"finished\"") {
            break;
        }
        assert!(Instant::now() < deadline, "never finished: {out}");
        std::thread::sleep(Duration::from_millis(100));
    }

    let asked = || {
        let (_, out, _) = rl(&["ls", "--all", "--json"]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .filter(|r| r["cause"] == "assistant:summary")
            .count()
    };

    // A stretch asked for is counted: no one is asked.
    let (ok, text, err) = rl(&["since", "1h"]);
    assert!(ok, "{err}");
    assert!(text.contains("while you were away"), "{text}");
    assert!(text.contains(": 1 ran\n"), "counted: {text}");
    assert!(text.contains(&format!(" {id}  say hi")), "the tree under it: {text}");
    assert!(!err.contains("writing the top"), "{err}");
    // The first bare look has no presence to come back from: counted too.
    let (ok, _, err) = rl(&["since"]);
    assert!(ok && !err.contains("writing the top"), "{err}");
    assert_eq!(asked(), 0);

    // Away two days, then a bare look: the top is written for you.
    let ledger = rl_ledger::Ledger::open(&home.join("ledger.db")).unwrap();
    ledger
        .call_blocking(|c| {
            c.execute_batch("update presence set last_seen_at = (strftime('%s','now') - 172800) * 1000;")?;
            Ok(())
        })
        .unwrap();
    let (ok, text, err) = rl(&["since"]);
    assert!(ok, "{err}");
    assert!(text.contains("\n  • proj: it echoed, and that went fine\n"), "{text}");
    assert!(text.contains("\n  # by fakeai · from 1 run\n"), "{text}");
    assert!(text.contains(&format!(" {id}  say hi")), "the tree under it: {text}");
    assert!(err.contains("writing the top…"), "{err}");
    assert_eq!(asked(), 1);
}
