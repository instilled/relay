//! `rl every`: a schedule written into this folder's schedules.yaml (or
//! yours with --global), loaded and on, the file's comments untouched.

use std::path::PathBuf;
use std::process::Command;

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

#[test]
fn every_writes_loads_and_approves() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld/rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let ws = dir.path().join("proj");
    std::fs::create_dir_all(ws.join(".relay")).unwrap();
    std::fs::write(
        ws.join(".relay/schedules.yaml"),
        "# ours\nschedules:\n  # the old one\n  nightly:\n    every: 4h\n    run: 'make test'\n",
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
    let run = |mut c: Command| {
        let out = c.output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    };
    assert!(run(rl(&["daemon", "start"])).0);
    // A failing assertion must not leave the daemon behind.
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

    // A folder not yet known: registered on the way, the new rule on.
    let (ok, out, err) = run(rl(&["every", "30m", "--tag", "ci", "--", "make", "lint"]));
    assert!(ok, "{err}");
    assert!(
        out.starts_with("make:\n    every: 30m\n    run:\n    - make\n    - lint\n    tags:\n    - ci"),
        "{out}"
    );
    assert!(out.ends_with("schedules.yaml · on"), "{out}");
    let text = std::fs::read_to_string(ws.join(".relay/schedules.yaml")).unwrap();
    assert!(
        text.starts_with("# ours\nschedules:\n  # the old one\n  nightly:"),
        "comments and the old rule stay: {text}"
    );
    assert!(text.contains("\n  make:\n    every: 30m\n"), "{text}");
    let (_, listed, _) = run(rl(&["rules"]));
    let make = listed
        .lines()
        .find(|l| l.contains("  make "))
        .unwrap_or_default()
        .to_string();
    assert!(make.contains("  on  ") && make.contains("every 30m"), "{listed}");
    let nightly = listed
        .lines()
        .find(|l| l.contains("nightly"))
        .unwrap_or_default()
        .to_string();
    assert!(
        nightly.contains("  on  "),
        "up --yes on the way approved the folder's rules too: {listed}"
    );

    // The same name again is refused; --replace rewrites in place; --name is another.
    let (ok, _, err) = run(rl(&["every", "1h", "--", "make", "lint"]));
    assert!(!ok && err.contains("already in") && err.contains("--replace"), "{err}");
    let (ok, _, err) = run(rl(&["every", "1h", "--replace", "--", "make", "lint"]));
    assert!(ok, "{err}");
    let text = std::fs::read_to_string(ws.join(".relay/schedules.yaml")).unwrap();
    assert!(
        text.contains("\n  make:\n    every: 1h\n") && !text.contains("30m"),
        "{text}"
    );
    assert_eq!(text.matches("  make:").count(), 1);

    // A cron line, an agent, yours: into the home file with a cwd; --json is the rule.
    let (ok, out, err) = run(rl(&[
        "--json",
        "every",
        "0 2 * * *",
        "--global",
        "--agent",
        "claude",
        "--name",
        "night-fix",
        "--",
        "fix",
        "the",
        "build",
    ]));
    assert!(ok, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["scope"], "global");
    assert_eq!(v["name"], "night-fix");
    assert!(v["approved_hash"].is_string(), "{v}");
    let text = std::fs::read_to_string(home.join("schedules.yaml")).unwrap();
    assert!(
        text.contains("schedules:\n  night-fix:\n    cron: 0 2 * * *\n    run:\n      agent: claude\n      prompt: fix the build\n    cwd: "),
        "{text}"
    );

    // Not a cadence.
    let (ok, _, err) = run(rl(&["every", "soon", "--", "true"]));
    assert!(!ok && err.contains("neither a duration"), "{err}");
}
