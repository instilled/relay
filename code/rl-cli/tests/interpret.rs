//! `rl interpret "…"`: the words go to the configured agent as a run, the
//! draft comes back as YAML, and `--go` does what it says.

use std::path::PathBuf;
use std::process::Command;

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

fn fake_agent() -> PathBuf {
    std::fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-agent")).unwrap()
}

#[test]
fn words_become_a_draft_and_go_does_it() {
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
    std::fs::write(home.join("config.yaml"), "assistants:\n  interpret: fakeai\n").unwrap();
    let ws = dir.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    // The assistant's run gets the daemon's environment, and the daemon is
    // started by the first `rl` here: the answer file is named up front and
    // written only for the last case (the fixture ignores a missing one).
    let answer = dir.path().join("answer.txt");
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

    // The draft, printed; nothing done.
    let (ok, out, err) = run(rl(&["interpret", "run", "the", "tests"]));
    assert!(ok, "{err}");
    let ws_str = std::fs::canonicalize(&ws).unwrap().to_string_lossy().into_owned();
    assert!(out.starts_with("kind: run\n"), "{out}");
    assert!(out.contains(&format!("in: {ws_str}")) || out.contains("in: ~"), "{out}");
    assert!(
        out.contains("run: echo drafted") && out.contains("title: drafted"),
        "{out}"
    );
    assert!(
        out.contains("# by fakeai · run ") && out.contains("--go does it"),
        "{out}"
    );
    let (_, listed, _) = run(rl(&["ls", "--all", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&listed).unwrap();
    let titles: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["title"].as_str().unwrap())
        .collect();
    assert!(
        titles.iter().any(|t| t.starts_with("interpret: run the tests")),
        "{titles:?}"
    );
    assert!(
        !titles.contains(&"drafted"),
        "without --go nothing was started: {titles:?}"
    );

    // --go: the drafted run starts.
    let (ok, out, err) = run(rl(&["interpret", "--go", "run", "the", "tests"]));
    assert!(ok, "{err}");
    assert!(out.contains("started drafted · "), "{out}");
    let (_, listed, _) = run(rl(&["ls", "--all", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&listed).unwrap();
    let drafted = v
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["title"] == "drafted")
        .expect("the drafted run");
    assert_eq!(drafted["argv"], serde_json::json!(["sh", "-c", "echo drafted"]));
    assert_eq!(drafted["cause"], "manual", "the person's go, not the assistant's");
    let asked = v
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["cause"] == "assistant:interpret")
        .count();
    assert_eq!(asked, 2, "each interpret was a run of its own");

    // An answer that is not a draft is said, with what the agent said.
    std::fs::write(&answer, "I would rather not.\n").unwrap();
    let (ok, _, err) = run(rl(&["interpret", "anything"]));
    assert!(!ok);
    assert!(
        err.contains("no `kind:`") && err.contains("I would rather not"),
        "{err}"
    );
}
