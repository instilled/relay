//! The Claude path end to end without a model: `rl run --agent claude` where
//! `claude` is the fixture player (`code/fixtures/claude`), replaying a real
//! recorded session and the hook payloads it sent, through the settings file
//! rld injects. What the ledger says afterwards is what a real run says.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

#[test]
fn a_recorded_claude_session_reports_through_its_hooks() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../fixtures")
        .canonicalize()
        .unwrap();
    let path = format!("{}:{}", fixtures.display(), std::env::var("PATH").unwrap_or_default());
    let tmp = tempfile::tempdir().unwrap();
    // `rl since` would have the fixture write its top: it is counted instead.
    std::fs::write(tmp.path().join("config.yaml"), "catch_up:\n  summary: ask\n").unwrap();
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_rl"))
            .args(args)
            .current_dir(tmp.path())
            .env("RELAY_HOME", tmp.path())
            .env("RELAY_RLD_BIN", &rld)
            .env("RELAY_SESSION_BIN", &session)
            .env("PATH", &path)
            .env("CLAUDE_PLAYER_SPEED", "0")
            .env_remove("RELAY_RUN")
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
    let me = |out: &str, id: &str| -> serde_json::Value {
        let v: serde_json::Value = serde_json::from_str(out).unwrap();
        v.as_array().unwrap().iter().find(|r| r["id"] == id).unwrap().clone()
    };
    let wait = |id: &str, done: &dyn Fn(&serde_json::Value) -> bool| -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let (_, out, _) = run(&["ls", "--all", "--json"]);
            let r = me(&out, id);
            if done(&r) {
                break r;
            }
            assert!(Instant::now() < deadline, "waited too long: {r}");
            std::thread::sleep(Duration::from_millis(100));
        }
    };

    // Interactive: a prompt without -p answers it and waits at the `>` for the next.
    let (ok, out, err) = run(&["run", "--agent", "claude", "Run echo and say done"]);
    assert!(ok, "{err}");
    let chat = out.trim().to_string();
    // The Stop hook lands before the `>` is printed and flushed; wait for both.
    let row = wait(&chat, &|r| r["state"] == "idle" && r["last_line"] == ">");
    assert_eq!(row["source"], "hook");
    assert_eq!(row["confidence"], "exact");
    assert_eq!(row["detail"], "finished responding", "the last thing the hooks said");
    assert!(run(&["stop", &chat]).0);
    wait(&chat, &|r| r["state"] == "stopped");

    // Headless: the same turn, then exit 0.
    let (ok, out, err) = run(&[
        "run",
        "--agent",
        "claude",
        "--ref",
        "gh:10",
        "-p",
        "Run echo and say done",
    ]);
    assert!(ok, "{err}");
    let id = out.trim().to_string();
    let row = wait(&id, &|r| r["state"] == "finished");

    // Detected as Claude, given the hooks file, exact all the way.
    assert_eq!(row["cli"], "claude");
    assert_eq!(row["kind"], "agent");
    assert_eq!(row["exit_code"], 0);
    let argv: Vec<String> = serde_json::from_value(row["argv"].clone()).unwrap();
    let settings = tmp.path().join("hooks/claude.json");
    assert_eq!(
        &argv[..3],
        &[
            "claude".to_string(),
            "--settings".into(),
            settings.to_string_lossy().into_owned()
        ],
        "{argv:?}"
    );
    assert_eq!(row["detail"], "finished responding", "the last thing the hooks said");

    // The ledger's account of the turn, in order. `working` exact after
    // `working` guess is a confidence upgrade, not news; the Stop hook's
    // `idle` is.
    let (_, out, _) = run(&["events", "--run", &id, "--json"]);
    let evs: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    let kinds: Vec<&str> = evs.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["run.started", "run.state", "run.finished"], "{out}");
    assert_eq!(evs[1]["data"]["state"], "idle");
    assert_eq!(evs[1]["data"]["detail"], "finished responding");
    assert_eq!(evs[1]["data"]["confidence"], "exact");
    assert_eq!(evs[1]["data"]["source"], "hook");
    assert!(evs[2]["summary"].as_str().unwrap().contains("exit 0"), "{}", evs[2]);

    // What Claude printed reached the terminal.
    let (ok, out, _) = run(&["join", &id]);
    assert!(ok, "an ended run's page replays it");
    assert!(out.contains("⏺ Bash(echo relay-fixture)"), "{out}");
    assert!(out.contains("relay-fixture"), "{out}");

    let (_, out, _) = run(&["since", "24h"]);
    assert!(out.contains("gh:10"), "{out}");
    assert!(out.contains(&format!(" {id}  ")), "{out}");
    assert!(out.contains("done · exit 0"), "{out}");
    assert!(run(&["daemon", "stop"]).0);
}
