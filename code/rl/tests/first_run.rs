//! The list's first start on a terminal asks once to set Relay up: `rl` on
//! a pty, with a HOME of its own and only a stand-in codex on its PATH
//! (codex's entries are files: answering no, nothing is run or written but
//! `config.json`).

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use rl_session::backend::{self, Spawned, Spec};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

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

fn spawn(home: &std::path::Path, bin: &std::path::Path) -> Spawned {
    backend::spawn(&Spec {
        argv: vec![env!("CARGO_BIN_EXE_rl").into()],
        cwd: home.to_path_buf(),
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
    .unwrap()
}

#[tokio::test]
async fn the_first_start_asks_once_and_a_no_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut sp = spawn(&home, &bin);
    let mut out = Vec::new();
    assert!(
        until(&mut sp, &mut out, "Set up Relay for codex? [Y/n] ").await,
        "not asked: {}",
        String::from_utf8_lossy(&out)
    );
    let said = String::from_utf8_lossy(&out).into_owned();
    assert!(
        said.contains("codex: ~/.codex/hooks.json (hooks) · ~/.codex/config.toml (mcp relay)"),
        "{said}"
    );
    sp.input.send(b"n\r".to_vec()).await.unwrap();
    assert!(until(&mut sp, &mut out, "\x1b[?1049h").await, "the list never opened");
    kill(Pid::from_raw(sp.pid as i32), Signal::SIGTERM).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(20), &mut sp.exit).await;
    assert!(home.join(".relay/config.json").exists(), "kept, so not asked again");
    assert!(!home.join(".codex").exists(), "a no sets nothing up");

    // The second start opens the list at once.
    let mut sp = spawn(&home, &bin);
    let mut out = Vec::new();
    assert!(until(&mut sp, &mut out, "\x1b[?1049h").await);
    assert!(!String::from_utf8_lossy(&out).contains("[Y/n]"));
    kill(Pid::from_raw(sp.pid as i32), Signal::SIGTERM).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(20), &mut sp.exit).await;
}
