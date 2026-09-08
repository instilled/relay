//! J1's done-when, minus `join` (step 7): `kill -9 rld` mid-run, `rl daemon
//! start`, and the run is still there with its output intact.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

#[test]
fn kill_dash_nine_the_daemon_and_the_run_survives() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let rl = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_rl"))
            .args(args)
            .current_dir(tmp.path())
            .env("RELAY_HOME", tmp.path())
            .env("RELAY_RLD_BIN", &rld)
            .env("RELAY_SESSION_BIN", &session)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    assert!(rl(&["daemon", "start"]).0);
    let (ok, out, err) = rl(&[
        "run",
        "--no-tty",
        "--",
        "sh",
        "-c",
        "echo early; sleep 2; echo late; exit 0",
    ]);
    assert!(ok, "{err}");
    let id = out.trim().to_string();

    let (_, out, _) = rl(&["daemon", "status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let pid = v["daemon"]["pid"].as_u64().unwrap();
    assert!(
        Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    std::thread::sleep(Duration::from_millis(300));
    let (ok, _, _) = rl(&["daemon", "status"]);
    assert!(!ok, "daemon should be dead");

    let (ok, out, err) = rl(&["daemon", "start"]);
    assert!(ok, "{out}{err}");

    let deadline = Instant::now() + Duration::from_secs(15);
    let done = loop {
        let (_, out, _) = rl(&["ls", "--all", "--json"]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let me = v
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .expect("run still listed")
            .clone();
        if me["state"] == "finished" {
            break me;
        }
        assert!(Instant::now() < deadline, "never finished: {me}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(done["exit_code"], 0);
    assert_eq!(
        done["last_line"], "late",
        "output after the crash was journaled by the new daemon"
    );
    let (_, out, _) = rl(&["events", "--run", &id]);
    assert!(out.contains("run.started") && out.contains("run.finished"), "{out}");
    assert!(rl(&["daemon", "stop"]).0);
}
