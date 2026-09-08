//! J2's done-when through the real binaries: `rl run --tag e2e -- <cmd>`
//! returns a run id immediately; `rl ls` later shows it finished with its
//! exit code.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

#[test]
fn run_then_ls_shows_the_outcome() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
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

    let (ok, out, err) = run(&["run", "--tag", "e2e", "--", "sh", "-c", "echo working; exit 0"]);
    assert!(ok, "{err}");
    let id = out.trim().to_string();
    assert_eq!(id.len(), 8, "run prints just the id: {out:?}");

    let (ok, _, err) = run(&["run"]);
    assert!(!ok && err.contains("nothing to run"), "{err}");

    let deadline = Instant::now() + Duration::from_secs(15);
    let finished = loop {
        let (_, out, _) = run(&["ls", "--all", "--tag", "e2e", "--json"]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let me = v
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .expect("run listed");
        if me["state"] == "finished" {
            break me.clone();
        }
        assert!(Instant::now() < deadline, "never finished: {me}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(finished["exit_code"], 0);
    assert_eq!(finished["tags"], serde_json::json!(["e2e"]));
    assert_eq!(finished["last_line"], "working");

    let (_, out, _) = run(&["ls"]);
    assert_eq!(out.trim(), "no runs", "finished runs are hidden by default");
    let (_, out, _) = run(&["ls", "--all"]);
    assert!(out.contains(&id) && out.contains("finished"), "{out}");
    let (_, out, _) = run(&["events", "--run", &id]);
    assert!(out.contains("run.started") && out.contains("run.finished"), "{out}");

    // Agent form: argv is the cli plus the prompt; stop it.
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-cli");
    let (ok, out, err) = run(&["run", "--agent", fake.to_str().unwrap(), "--json", "do the thing"]);
    assert!(ok, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["kind"], "agent");
    assert_eq!(v["cli"], "fake-cli");
    assert_eq!(v["argv"][1], "do the thing");
    let agent_id = v["id"].as_str().unwrap().to_string();
    let (ok, out, _) = run(&["stop", &agent_id]);
    assert!(ok && out.contains("stop requested"), "{out}");

    assert!(run(&["daemon", "stop"]).0);
}
