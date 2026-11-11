//! `rl setup` ends with what you can do now (messaging, the guide's link)
//! when it set something up: by hand, and the first start's yes. Never after
//! `nothing to change`, nor for `--check` or `--remove`. Codex only: its
//! entries are files, so no harness CLI runs.

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use rl::guide::MESSAGING_GUIDE;
use rl_session::backend::{self, Spawned, Spec};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};
use std::time::Duration;

fn rl(home: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rl"))
        .args(args)
        .env("HOME", home)
        .env("RELAY_HOME", home.join(".relay"))
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("RL_SESSION")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn setup_says_what_you_can_do_now_only_when_it_set_something_up() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let check = rl(home, &["setup", "--check", "--harness", "codex"]);
    assert!(!stdout(&check).contains(MESSAGING_GUIDE), "{check:?}");

    let run = rl(home, &["setup", "--harness", "codex"]);
    assert!(run.status.success(), "{run:?}");
    let said = stdout(&run);
    assert!(said.starts_with("codex: hooks installed"), "{said}");
    assert!(
        said.contains("\n\nWhat you can do now: your agents can message each other"),
        "{said}"
    );
    assert!(said.trim_end().ends_with(&format!("More: {MESSAGING_GUIDE}")), "{said}");

    let again = rl(home, &["setup", "--harness", "codex"]);
    assert_eq!(stdout(&again), "nothing to change\n");

    let remove = rl(home, &["setup", "--remove", "--harness", "codex"]);
    assert!(remove.status.success(), "{remove:?}");
    assert!(!stdout(&remove).contains(MESSAGING_GUIDE), "{remove:?}");
}

/// Reads until `what` has been printed, or 20s.
async fn until(sp: &mut Spawned, out: &mut Vec<u8>, what: &str) -> bool {
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(b) = sp.output.recv().await {
            out.extend_from_slice(&b);
            if String::from_utf8_lossy(out).contains(what) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

#[tokio::test]
async fn the_first_starts_yes_ends_with_what_you_can_do_now() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut sp = backend::spawn(&Spec {
        argv: vec![env!("CARGO_BIN_EXE_rl").into()],
        cwd: home.clone(),
        env: vec![
            ("HOME".into(), home.display().to_string()),
            ("RELAY_HOME".into(), home.join(".relay").display().to_string()),
            ("PATH".into(), bin.display().to_string()),
            ("TERM".into(), "xterm-256color".into()),
        ],
        inherit_env: false,
        cols: 100,
        rows: 30,
        settle: Duration::ZERO,
    })
    .unwrap();
    let mut out = Vec::new();
    assert!(
        until(&mut sp, &mut out, "Set up Relay for codex? [Y/n] ").await,
        "not asked: {}",
        String::from_utf8_lossy(&out)
    );
    sp.input.send(b"y\r".to_vec()).await.unwrap();
    assert!(
        until(&mut sp, &mut out, "enter opens the list").await,
        "{}",
        String::from_utf8_lossy(&out)
    );
    let said = String::from_utf8_lossy(&out).into_owned();
    assert!(said.contains("codex: hooks installed"), "{said}");
    assert!(said.contains("What you can do now"), "{said}");
    assert!(said.contains(MESSAGING_GUIDE), "{said}");
    kill(Pid::from_raw(sp.pid as i32), Signal::SIGTERM).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(20), &mut sp.exit).await;
}
