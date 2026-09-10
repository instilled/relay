//! `rl on` writes a rule into your own reactions.yaml and it takes effect;
//! `rl react ls` and `rl react test` say what it is and what it would do.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

#[test]
fn on_writes_a_rule_that_fires() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let rl = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rl"));
        c.args(args)
            .current_dir(&dir)
            .env("RELAY_HOME", &dir)
            .env("RELAY_RLD_BIN", &rld)
            .env("RELAY_SESSION_BIN", &session)
            .env_remove("RELAY_RUN");
        c
    };
    let ok = |mut c: Command| {
        let out = c.output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    ok(rl(&["daemon", "start"]));

    // The file is yours: a comment written first survives.
    let file = dir.join("reactions.yaml");
    std::fs::write(&file, "# my rules\nreactions:\n").unwrap();
    let written = ok(rl(&[
        "on",
        "run.failed",
        "--name",
        "tell-me",
        "--",
        "sh",
        "-c",
        "echo {kind} >> told.txt",
    ]));
    assert!(
        written.contains("tell-me:") && written.contains("on: run.failed"),
        "{written}"
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("# my rules\n"), "the file was rewritten: {text}");
    assert!(text.contains("  tell-me:\n"), "{text}");

    let listed = ok(rl(&["react", "ls"]));
    assert!(listed.contains("tell-me") && listed.contains("run.failed"), "{listed}");
    assert!(listed.contains("global"), "{listed}");

    // The same name twice is refused rather than duplicated.
    let out = rl(&["on", "run.failed", "--name", "tell-me", "--", "true"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already in"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A run fails; the rule answers it.
    ok(rl(&["run", "--title", "build", "--", "sh", "-c", "exit 2"]));
    let told = dir.join("told.txt");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !told.is_file() {
        assert!(Instant::now() < deadline, "the rule never fired");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(std::fs::read_to_string(&told).unwrap().trim(), "run.failed");

    // `react test` explains a real event without doing anything.
    let events = ok(rl(&["--json", "events", "--kind", "run.failed"]));
    let id = events
        .split("\"id\":")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .unwrap()
        .trim()
        .to_string();
    let trial = ok(rl(&["react", "test", &id]));
    assert!(trial.contains("tell-me") && trial.contains("told.txt"), "{trial}");
    // It just fired, so the honest answer is that the throttle holds it.
    assert!(trial.contains("fired too recently"), "{trial}");

    assert!(rl(&["daemon", "stop"]).status().unwrap().success());
}
