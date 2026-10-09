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

#[test]
fn words_become_a_flow_and_go_writes_it() {
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
    // Named up front, as the daemon the first `rl` starts passes its environment on.
    let answer = dir.path().join("answer.txt");
    let prompt = dir.path().join("prompt.txt");
    let flow_md = "---\nwhen: every 1h\nsteps:\n  say:\n    run: echo hello\n---\n\nSays hello every hour.\n";
    std::fs::write(&answer, format!("Here it is:\nkind: flow\nname: hello\n{flow_md}")).unwrap();
    let rl = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rl"));
        c.args(args)
            .current_dir(&ws)
            .env("RELAY_HOME", &home)
            .env("RELAY_RLD_BIN", &rld)
            .env("RELAY_SESSION_BIN", &session)
            .env("FAKE_AGENT_ANSWER", &answer)
            .env("FAKE_AGENT_PROMPT", &prompt)
            .env("EDITOR", "true")
            .env_remove("VISUAL")
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

    // The draft, printed; nothing written.
    let (ok, out, err) = run(rl(&["interpret", "say", "hello", "every", "hour"]));
    assert!(ok, "{err}");
    assert!(
        out.starts_with(&format!("kind: flow\nname: hello\n{}", flow_md.trim_end())),
        "{out}"
    );
    assert!(out.contains("# by fakeai · run "), "{out}");
    assert!(
        out.contains("--go asks again and writes what it answers") && out.contains("rl flow new hello \""),
        "the hint says what --go does: {out}"
    );
    assert!(!home.join("flows/hello").exists());

    // --go: written as drafted, and said yes to.
    let (ok, out, err) = run(rl(&["interpret", "--go", "say", "hello", "every", "hour"]));
    assert!(ok, "{err}");
    assert!(out.ends_with("hello approved"), "{out}");
    assert_eq!(
        std::fs::read_to_string(home.join("flows/hello/flow.md")).unwrap(),
        flow_md
    );

    // `rl flow new <name> <words>` asks for that flow by name; the person's name wins the draft's.
    let (ok, out, err) = run(rl(&["flow", "new", "hi", "say", "hi", "every", "hour"]));
    assert!(ok, "{out}{err}");
    assert_eq!(out, "approved hi");
    assert_eq!(std::fs::read_to_string(home.join("flows/hi/flow.md")).unwrap(), flow_md);
    let asked = std::fs::read_to_string(&prompt).unwrap();
    assert!(
        asked.contains("a flow named `hi`") && asked.contains("## Flows"),
        "{asked}"
    );

    // With --go no editor shows it: the draft is shown before its yes.
    let (ok, out, err) = run(rl(&["flow", "new", "hi-go", "--go", "say", "hi"]));
    assert!(ok, "{out}{err}");
    assert_eq!(out, "approved hi-go");
    assert!(err.contains(flow_md.trim_end()), "{err}");

    // --json --go: one JSON object, the outcome in it.
    let (ok, out, err) = run(rl(&["--json", "interpret", "--go", "say", "hello"]));
    assert!(!ok, "hello is a flow already: {out}");
    assert!(err.contains("is a flow already"), "{err}");
    assert!(
        out.is_empty() || serde_json::from_str::<serde_json::Value>(&out).is_ok(),
        "{out}"
    );
    std::fs::write(&answer, format!("kind: flow\nname: hello-json\n{flow_md}")).unwrap();
    let (ok, out, err) = run(rl(&["--json", "interpret", "--go", "say", "hello"]));
    assert!(ok, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(
        (v["name"].as_str(), v["approved"].as_bool()),
        (Some("hello-json"), Some(true)),
        "{v}"
    );
    assert_eq!(v["said"], "hello-json approved");
    assert_eq!(v["flow_md"], flow_md, "what was said yes to is in it");

    // `rl flow new … --go --json` likewise: the draft shown, and its bytes in the JSON.
    let (ok, out, err) = run(rl(&["--json", "flow", "new", "hi-json", "--go", "say", "hi"]));
    assert!(ok, "{err}");
    assert!(err.contains(flow_md.trim_end()), "shown before its yes: {err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(
        (v["name"].as_str(), v["flow_md"].as_str()),
        (Some("hi-json"), Some(flow_md))
    );

    // A name that is not a flow's is never printed as one, nor written.
    std::fs::write(&answer, format!("kind: flow\nname: ../escaped\n{flow_md}")).unwrap();
    let (ok, out, err) = run(rl(&["interpret", "--go", "say", "hello"]));
    assert!(!ok);
    assert!(!out.contains("name: ../escaped"), "{out}");
    assert!(err.contains("is not a flow name"), "{err}");
    assert!(!home.join("escaped").exists() && !dir.path().join("escaped").exists());

    // A draft that would not load is refused before anything is written, by either way in.
    let broken = "---\nsteps:\n  a:\n    after: nope\n    run: x\n---\n";
    std::fs::write(&answer, format!("kind: flow\nname: broken\n{broken}")).unwrap();
    let (ok, out, err) = run(rl(&["interpret", "--go", "say", "hello"]));
    assert!(!ok, "{out}");
    assert!(
        err.contains("broken would not load: flow.md:3 · `a` comes after `nope`"),
        "{err}"
    );
    assert!(!home.join("flows/broken").exists());
    let (ok, out, err) = run(rl(&["flow", "new", "broken-too", "--go", "say", "hello"]));
    assert!(!ok, "{out}");
    assert!(err.contains("broken-too would not load: flow.md:3"), "{err}");
    assert!(!home.join("flows/broken-too").exists());

    // Asked for a flow, answered a run: said, and nothing written.
    std::fs::write(&answer, "kind: run\nrun: echo hey\n").unwrap();
    let (ok, _, err) = run(rl(&["flow", "new", "hey", "say", "hey"]));
    assert!(!ok);
    assert!(err.contains("asked for a flow; fakeai answered a run"), "{err}");
    assert!(!home.join("flows/hey").exists());
}
