//! `rl summary <what>`: the kept summary, or a new one from
//! `assistants.summary`; `--again` asks anew. `<what>` is a run, a folder, a
//! flow or `all`. `rl config` shows the summariser beside the interpreter.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

fn fake_agent() -> PathBuf {
    std::fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-agent")).unwrap()
}

#[test]
fn an_ended_run_is_summarised_on_the_line() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld/rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
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
    let ws = dir.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    // The assistant's run gets the daemon's environment, and the daemon is
    // started by the first `rl` here.
    let answer = dir.path().join("answer.txt");
    std::fs::write(&answer, "It said hi. That is all.\n").unwrap();
    let rl = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rl"));
        c.args(args)
            .current_dir(&ws)
            .env("RELAY_HOME", &home)
            .env("RELAY_RLD_BIN", &rld)
            .env("RELAY_SESSION_BIN", &session)
            .env("FAKE_AGENT_ANSWER", &answer)
            .env_remove("RELAY_RUN")
            .env_remove("FAKE_AGENT_SLEEP");
        c
    };
    let run = |mut c: Command| {
        let out = c.output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
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

    let (ok, id, err) = run(rl(&["run", "--title", "hi", "--", "sh", "-c", "echo hi"]));
    assert!(ok, "{err}");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, listed, _) = run(rl(&["ls", "--all", "--json"]));
        let v: serde_json::Value = serde_json::from_str(&listed).unwrap();
        let ended = v
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == id && r["state"] == "finished");
        if ended {
            break;
        }
        assert!(Instant::now() < deadline, "the run never ended: {listed}");
        std::thread::sleep(Duration::from_millis(100));
    }

    let (ok, out, err) = run(rl(&["summary", &id]));
    assert!(ok, "{err}");
    assert!(out.starts_with("It said hi. That is all.\n# by fakeai · "), "{out}");
    assert!(err.contains("summarising…"), "{err}");

    // Kept: no asking the second time; `--again` asks.
    let (ok, out2, err) = run(rl(&["summary", &id]));
    assert!(ok && out2 == out, "{out2}\n{err}");
    assert!(!err.contains("summarising…"), "{err}");
    let (_, listed, _) = run(rl(&["ls", "--all", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&listed).unwrap();
    let asked = |v: &serde_json::Value| {
        v.as_array()
            .unwrap()
            .iter()
            .filter(|r| r["cause"] == "assistant:summary")
            .count()
    };
    assert_eq!(asked(&v), 1);
    let (ok, out, err) = run(rl(&["--json", "summary", "--again", &id]));
    assert!(ok, "{err}");
    let s: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(s["run"], id);
    assert_eq!(s["agent"], "fakeai");
    let (_, listed, _) = run(rl(&["ls", "--all", "--json"]));
    assert_eq!(asked(&serde_json::from_str(&listed).unwrap()), 2);

    // `rl config` shows both assistants.
    let (ok, out, err) = run(rl(&["config"]));
    assert!(ok, "{err}");
    assert!(
        out.contains("assistants.interpret  ") && out.contains("assistants.summary  "),
        "{out}"
    );
    assert!(out.contains("--headless  (config)"), "{out}");
    assert!(
        out.contains("`auto` writes the top when you come back after `after`, by assistants.summary"),
        "{out}"
    );

    // Everything, over the last hour: said by what it is of, and from when.
    let (ok, out, err) = run(rl(&["summary", "all", "1h"]));
    assert!(ok, "{err}");
    assert!(out.contains("# summary of everything · since "), "{out}");
    assert!(out.contains("by fakeai"), "{out}");
    // A stretch with nothing in it asks no one.
    let (ok, out, err) = run(rl(&["summary", "all", "2999-01-01"]));
    assert!(ok, "{err}");
    assert!(out.contains("nothing happened since"), "{out}");
    assert!(
        out.contains("# summary of everything · since ") && out.contains("nothing new"),
        "{out}"
    );

    // A folder is a project; with no stretch, the last day.
    let (ok, out, err) = run(rl(&["summary", ".", "--deep"]));
    assert!(ok, "{err}");
    assert!(
        out.contains("# summary of the project ") && out.contains("/proj · since "),
        "{out}"
    );
    assert!(out.contains(" · deep · by fakeai · "), "{out}");
    // From a fixed instant it is kept, like a run's.
    let (ok, out, err) = run(rl(&["summary", ".", "2020-01-01"]));
    assert!(ok && out.contains("since Jan 01 00:00 · by fakeai"), "{out}\n{err}");
    assert!(err.contains("summarising…"), "{err}");
    let (ok, again, err) = run(rl(&["summary", ".", "2020-01-01"]));
    assert!(ok && again == out && !err.contains("summarising…"), "{again}\n{err}");

    // A flow.
    let (ok, out, err) = run(rl(&["summary", "flow:nightly", "1h"]));
    assert!(ok, "{err}");
    assert!(out.contains("# summary of the flow nightly · since "), "{out}");

    // A run, read from its logs.
    let (ok, out, err) = run(rl(&["summary", &id, "--deep"]));
    assert!(ok, "{err}");
    assert!(out.lines().last().unwrap().starts_with("# deep · "), "{out}");

    // No such run.
    let (ok, _, err) = run(rl(&["summary", "nope"]));
    assert!(!ok && err.contains("no run nope"), "{err}");
}
