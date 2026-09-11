//! The daemon is nobody's job: the first `rl` command starts it and says so,
//! the next one finds it there and says nothing.

use std::path::PathBuf;
use std::process::Command;

fn rld_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name("rld");
    p.is_file().then_some(p)
}

#[test]
fn the_first_command_starts_the_daemon() {
    let Some(rld) = rld_bin() else {
        eprintln!("skipping: rld not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_rl"))
            .args(args)
            .env("RELAY_HOME", tmp.path())
            .env("RELAY_RLD_BIN", &rld)
            .env_remove("RELAY_RUN")
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
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
    let _stop = Stop(tmp.path().to_path_buf(), rld.clone());

    assert!(!tmp.path().join("run/rld.pid").exists());
    let (ok, out, err) = run(&["ls"]);
    assert!(ok, "{err}");
    assert!(err.contains("rld started"), "stderr says so: {err}");
    assert!(!err.contains("note:"), "same release, nothing to note: {err}");
    assert!(tmp.path().join("run/rld.pid").exists());
    assert!(
        out.contains("no runs") || out.trim().is_empty() || out.contains("ID"),
        "{out}"
    );

    let (ok, _, err) = run(&["ls"]);
    assert!(ok && !err.contains("rld started"), "already there: {err}");

    // `rl daemon status` is the explicit way and agrees.
    let (ok, out, _) = run(&["--json", "daemon", "status"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["running"], true);
    assert_eq!(v["daemon"]["version"], env!("CARGO_PKG_VERSION"));
}
