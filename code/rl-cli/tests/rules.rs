//! `rl rules`: one table over both files, a yes by name, pause and resume
//! without saying which kind.

use std::path::PathBuf;
use std::process::Command;

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

#[test]
fn rules_list_approve_pause() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let ws = dir.join("proj");
    std::fs::create_dir_all(ws.join(".relay")).unwrap();
    std::fs::write(
        home.join("reactions.yaml"),
        "reactions:\n  notify:\n    on: run.failed\n    run: 'true'\n    cwd: /tmp\n",
    )
    .unwrap();
    std::fs::write(
        ws.join(".relay/schedules.yaml"),
        "schedules:\n  nightly:\n    every: 4h\n    run: 'make test'\n",
    )
    .unwrap();
    let rl = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rl"));
        c.args(args)
            .current_dir(&ws)
            .env("RELAY_HOME", &home)
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
    ok(rl(&["up"]));

    // Both files in one table, the workspace's waiting.
    let listed = ok(rl(&["rules"]));
    let lines: Vec<&str> = listed.lines().collect();
    assert!(lines[0].starts_with("SCOPE") && lines[0].contains("NAME"), "{listed}");
    assert!(
        lines[1].contains("global  notify") && lines[1].contains("on run.failed") && lines[1].contains("  on  "),
        "{listed}"
    );
    assert!(
        lines[2].contains("proj    nightly") && lines[2].contains("every 4h") && lines[2].contains("needs approval"),
        "{listed}"
    );
    assert!(
        listed.ends_with("1 needs your yes: `rl rules approve <name>`"),
        "{listed}"
    );

    // A yes by name.
    let approved = ok(rl(&["rules", "approve", "nightly"]));
    assert!(
        approved.contains("nightly") && approved.contains("  on  ") && approved.contains("in 3h59m"),
        "{approved}"
    );

    // Pause and resume find the kind themselves.
    let paused = ok(rl(&["rules", "pause", "notify"]));
    assert!(paused.contains("notify") && paused.contains("paused"), "{paused}");
    let resumed = ok(rl(&["--json", "rules", "resume", "notify"]));
    let v: serde_json::Value = serde_json::from_str(&resumed).unwrap();
    assert_eq!(v["kind"], "reaction");
    assert_eq!(v["paused"], false);

    // Nothing by that name says so.
    let out = rl(&["rules", "pause", "nope"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no rule named `nope`"));

    let reloaded = ok(rl(&["rules", "reload"]));
    assert!(
        reloaded.contains("schedules: 1 loaded") && reloaded.contains("reactions: 1 loaded"),
        "{reloaded}"
    );

    ok(rl(&["daemon", "stop"]));
}

/// A file that does not parse is said with where and why — the YAML's own
/// words, not just the method's name — and the reload still goes through.
#[test]
fn reload_says_what_did_not_load_and_where() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let ws = dir.join("proj");
    std::fs::create_dir_all(ws.join(".relay")).unwrap();
    let rl = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rl"));
        c.args(args)
            .current_dir(&ws)
            .env("RELAY_HOME", &home)
            .env("RELAY_RLD_BIN", &rld)
            .env("RELAY_SESSION_BIN", &session)
            .env_remove("RELAY_RUN");
        c
    };
    let run = |mut c: Command| {
        let out = c.output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(out.status.success(), "{err}");
        (String::from_utf8_lossy(&out.stdout).trim().to_string(), err)
    };
    run(rl(&["daemon", "start"]));

    std::fs::write(
        home.join("schedules.yaml"),
        "schedules:\n  nightly:\n    every: 1h\n   run: 'true'\n",
    )
    .unwrap();
    let (out, err) = run(rl(&["schedule", "reload"]));
    assert!(out.starts_with("0 loaded"), "{out}");
    assert!(
        err.contains("not loaded: global · schedules.yaml:4:") && err.contains("expected"),
        "{err}"
    );

    // A folder's broken reactions, said when it is brought up.
    std::fs::write(ws.join(".relay/reactions.yaml"), "reactions:\n  x: [\n").unwrap();
    let (_, err) = run(rl(&["up"]));
    assert!(err.contains("not loaded: proj · reactions.yaml:"), "{err}");

    run(rl(&["daemon", "stop"]));
}
