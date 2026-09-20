//! End to end through the real binaries: `rl daemon start` spawns `rld`,
//! detached, and `stop` takes it down. Everything under a temp RELAY_HOME.

use std::path::PathBuf;
use std::process::Command;

fn rl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rl"))
}

/// `rld` lives next to `rl` in target/; cargo builds every workspace bin before
/// running tests, so it is there under `cargo test --workspace`.
fn rld_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name("rld");
    p.is_file().then_some(p)
}

#[test]
fn start_status_stop_via_binaries() {
    let Some(rld) = rld_bin() else {
        eprintln!("skipping: rld binary not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = rl()
            .args(args)
            .env("RELAY_HOME", tmp.path())
            .env("RELAY_RLD_BIN", &rld)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };

    let (ok, out, _) = run(&["daemon", "status", "--json"]);
    assert!(!ok, "status should be non-zero when not running: {out}");
    assert!(out.contains("\"running\":false"), "{out}");

    let (ok, out, err) = run(&["daemon", "start", "--json"]);
    assert!(ok, "start failed: {out} {err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["started"], true);
    let pid = v["daemon"]["pid"].as_u64().unwrap();
    assert!(pid > 0);

    let (ok, out, _) = run(&["daemon", "start", "--json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["started"], false, "second start must be a no-op");
    assert_eq!(v["daemon"]["pid"].as_u64().unwrap(), pid);

    let (ok, out, _) = run(&["daemon", "status"]);
    assert!(ok);
    assert!(out.starts_with("rld running"), "{out}");

    let (ok, out, _) = run(&["daemon", "stop"]);
    assert!(ok);
    assert_eq!(out.trim(), "rld stopped");
    assert!(!tmp.path().join("run/rld.sock").exists());

    let (ok, out, _) = run(&["daemon", "stop"]);
    assert!(ok);
    assert_eq!(out.trim(), "rld was not running");
    assert!(tmp.path().join("log/rld.log").is_file());
}

/// `--with-runs`: the runs go first, so a throwaway home leaves nothing
/// holding a terminal.
#[test]
fn stop_with_runs_takes_the_runs_too() {
    let Some(rld) = rld_bin() else {
        eprintln!("skipping: rld binary not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = rl()
            .args(args)
            .env("RELAY_HOME", tmp.path())
            .env("RELAY_RLD_BIN", &rld)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };

    let (ok, out, err) = run(&["run", "--json", "--", "sleep", "600"]);
    assert!(ok, "run failed: {out} {err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let id = v["id"].as_str().unwrap().to_string();

    let (ok, out, _) = run(&["daemon", "stop", "--with-runs", "--json"]);
    assert!(ok, "{out}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(
        (v["stopped"].as_bool(), v["runs_stopped"].as_u64()),
        (Some(true), Some(1))
    );
    // The holder's socket goes with the run it held.
    let sock = tmp.path().join(format!("run/session-{id}.sock"));
    for _ in 0..100 {
        if !sock.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("{} is still there", sock.display());
}
