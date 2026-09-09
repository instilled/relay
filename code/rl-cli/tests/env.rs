//! `rl run` hands the run the environment of the shell it was started from,
//! not the daemon's; `--env` overrides; Relay's own names always win.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn sibling(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl")).with_file_name(name);
    p.is_file().then_some(p)
}

#[test]
fn the_run_sees_the_shell_environment() {
    let (Some(rld), Some(session)) = (sibling("rld"), sibling("rl-session")) else {
        eprintln!("skipping: rld / rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let base = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_rl"));
        c.args(args)
            .current_dir(tmp.path())
            .env("RELAY_HOME", tmp.path())
            .env("RELAY_RLD_BIN", &rld)
            .env("RELAY_SESSION_BIN", &session)
            .env_remove("RELAY_RUN")
            .env_remove("RELAY_REF")
            .env_remove("RELAY_TAGS")
            .env_remove("ONLY_THE_SHELL")
            .env_remove("ONLY_THE_DAEMON");
        c
    };

    // The daemon's environment carries a name the shell will not have.
    let out = base(&["daemon", "start"])
        .env("ONLY_THE_DAEMON", "leak")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let report = tmp.path().join("env.txt");
    let script = format!(
        "printf 'shell=%s daemon=%s over=%s run=%s home=%s\\n' \"$ONLY_THE_SHELL\" \"$ONLY_THE_DAEMON\" \"$OVERRIDDEN\" \"$RELAY_RUN\" \"$RELAY_HOME\" > {}",
        report.display()
    );
    let out = base(&["run", "--env", "OVERRIDDEN=yes", "--", "sh", "-c", &script])
        .env("ONLY_THE_SHELL", "here")
        .env("OVERRIDDEN", "no")
        .env("RELAY_RUN", "stale")
        .env("RELAY_HOME", tmp.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let deadline = Instant::now() + Duration::from_secs(10);
    while !report.is_file() {
        assert!(Instant::now() < deadline, "the run never wrote its report");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(50));
    let seen = std::fs::read_to_string(&report).unwrap();
    assert_eq!(
        seen.trim(),
        format!("shell=here daemon= over=yes run={id} home={}", tmp.path().display())
    );
    // The environment file does not outlive the start.
    let leftovers: Vec<_> = std::fs::read_dir(tmp.path().join("run"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".env"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert!(base(&["daemon", "stop"]).status().unwrap().success());
}
