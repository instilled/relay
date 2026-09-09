//! `rl since` through the real binaries: grouped news since the last look.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

#[test]
fn since_groups_what_happened() {
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
    let (ok, out, err) = run(&[
        "run",
        "--ref",
        "gh:7",
        "--title",
        "quick one",
        "--",
        "sh",
        "-c",
        "exit 3",
    ]);
    assert!(ok, "{err}");
    let id = out.trim().to_string();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, out, _) = run(&["ls", "--all", "--json"]);
        if out.contains("\"failed\"") {
            break;
        }
        assert!(Instant::now() < deadline, "never failed: {out}");
        std::thread::sleep(Duration::from_millis(100));
    }

    let (ok, out, err) = run(&["since"]);
    assert!(ok, "{err}");
    assert!(out.starts_with("since "), "{out}");
    assert!(out.contains("1 needs you"), "{out}");
    assert!(out.contains("\ngh:7\n"), "grouped by ref: {out}");
    assert!(out.contains(&format!("  ▸ {id}  failed")), "unseen marker: {out}");
    assert!(out.contains("exit 3 · "), "{out}");

    let (ok, out, _) = run(&["since", "2999-01-01"]);
    assert!(ok);
    assert!(
        out.contains(&id),
        "a failed run always shows, whatever the boundary: {out}"
    );
    let (_, out, _) = run(&["since", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["items"][0]["run"]["id"], id);
    assert_eq!(v["items"][0]["unseen"], true);
    assert!(run(&["daemon", "stop"]).0);
}
