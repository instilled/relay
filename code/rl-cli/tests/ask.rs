//! `rl ask` from inside a run, `rl decisions`, `rl decide`: the shape a
//! script or an agent actually uses, including the YAML form on stdin.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

#[test]
fn ask_then_decide() {
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

    // The flag form: a question with plain options.
    let id = ok(rl(&[
        "ask",
        "Ship it?",
        "-o",
        "ship:Ship now",
        "-o",
        "wait:Wait for review",
        "--summary",
        "18 commits since the last release",
    ]));
    assert_eq!(id.len(), 8, "expected a decision id, got {id:?}");

    let listed = ok(rl(&["decisions"]));
    assert!(listed.contains(&id) && listed.contains("Ship it?"), "{listed}");
    assert!(listed.contains("ship, wait"), "{listed}");

    // With no option named, `rl decide` shows the question and its options.
    let shown = ok(rl(&["decide", &id]));
    assert!(shown.contains("Ship now") && shown.contains("18 commits"), "{shown}");

    // The YAML form: options that carry actions, and a default.
    let mut child = rl(&["ask"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let yaml = concat!(
        "title: 3 errors in prod\n",
        "group: nightly/errors\n",
        "options:\n",
        "  - key: fix\n",
        "    label: Open a PR\n",
        "    run: [sh, -c, \"echo fixing > fixed.txt\"]\n",
        "  - key: ignore\n",
        "    label: Ignore\n",
        "    default: true\n",
    );
    child.stdin.take().unwrap().write_all(yaml.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let errors_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let listed = ok(rl(&["decisions"]));
    assert!(listed.contains("ignore*"), "the default is marked: {listed}");

    // Choosing runs what the option carries.
    let taken = ok(rl(&["decide", &errors_id, "fix"]));
    assert!(taken.contains("→ fix") && taken.contains("started "), "{taken}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !dir.join("fixed.txt").is_file() {
        assert!(std::time::Instant::now() < deadline, "the option never ran");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Dismissing answers without running anything.
    assert!(ok(rl(&["decide", &id, "--dismiss"])).contains("dismissed"));
    assert_eq!(ok(rl(&["decisions"])), "nothing waiting on you");
    assert!(ok(rl(&["decisions", "--all"])).contains("dismissed"));

    assert!(rl(&["daemon", "stop"]).status().unwrap().success());
}
