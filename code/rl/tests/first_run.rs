//! The list's first start on a terminal walks through setup once: `rl` on
//! a pty, with a HOME of its own and only a stand-in codex on its PATH
//! (codex's entries are files: nothing is run). `q` there changes nothing
//! but `config.json`; `enter` all the way through sets codex up; either way
//! the list opens after, and the next start opens it at once.

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use rl_session::backend::{self, Spawned, Spec};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

/// Where the cursor is, as a terminal answers `ESC [ 6n`: the wizard draws
/// in an inline viewport, and asks.
const CURSOR_AT: &[u8] = b"\x1b[1;1R";

/// Reads until `what` has been printed, or 20s, answering the cursor
/// position asks on the way.
async fn until(sp: &mut Spawned, out: &mut Vec<u8>, what: &str) -> bool {
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(b) = sp.output.recv().await {
            let asks = b.windows(4).filter(|w| *w == b"\x1b[6n").count();
            for _ in 0..asks {
                sp.input.send(CURSOR_AT.to_vec()).await.unwrap();
            }
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

/// A HOME with only a stand-in codex on its PATH.
fn machine() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    (tmp, home, bin)
}

async fn stop(sp: &mut Spawned) {
    kill(Pid::from_raw(sp.pid as i32), Signal::SIGTERM).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(20), &mut sp.exit).await;
}

/// Every file under `dir`, but the stand-in's.
fn files(dir: &std::path::Path) -> Vec<String> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                out.push(p.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.retain(|f| f != "bin/codex");
    out.sort();
    out
}

#[tokio::test]
async fn q_on_the_first_start_changes_nothing_but_the_config_and_opens_the_list() {
    let (_tmp, home, bin) = machine();
    let mut sp = spawn(&home, &bin);
    let mut out = Vec::new();
    assert!(
        until(&mut sp, &mut out, "Walk me through it").await,
        "no wizard: {}",
        String::from_utf8_lossy(&out)
    );
    let said = String::from_utf8_lossy(&out).into_owned();
    assert!(said.contains("relay setup") && said.contains("1/6"), "{said}");
    assert!(said.contains("codex"), "{said}");
    sp.input.send(b"q".to_vec()).await.unwrap();
    assert!(until(&mut sp, &mut out, "\x1b[?1049h").await, "the list never opened");
    let said = String::from_utf8_lossy(&out).into_owned();
    assert!(said.contains("nothing changed"), "the summary: {said}");
    stop(&mut sp).await;
    let written: Vec<String> = files(&home)
        .into_iter()
        .filter(|f| !f.starts_with(".relay/") || f == ".relay/config.json")
        .collect();
    assert_eq!(written, [".relay/config.json"], "nothing but config.json");

    // The second start opens the list at once.
    let mut sp = spawn(&home, &bin);
    let mut out = Vec::new();
    assert!(until(&mut sp, &mut out, "\x1b[?1049h").await);
    assert!(!String::from_utf8_lossy(&out).contains("Walk me through it"));
    stop(&mut sp).await;
}

#[tokio::test]
async fn enter_through_the_first_start_sets_up_and_opens_the_list() {
    let (_tmp, home, bin) = machine();
    let mut sp = spawn(&home, &bin);
    let mut out = Vec::new();
    assert!(until(&mut sp, &mut out, "Walk me through it").await);
    for (key, next) in [
        // Words of one colour each that only that step shows: the screen
        // is written by what changed, colour by colour, so a step's number
        // never comes whole.
        (&b"\r"[..], "~/.codex/hooks.json"),
        (b"\r", "all set up"),
        (b"\r", "Relay can't do this part"),
        (b"\r", "press ctrl+j to go on"),
        // ctrl+j goes on, as it moves on the list.
        (b"\n", "──send──▶"),
        (b"\r", "\x1b[?1049h"),
    ] {
        sp.input.send(key.to_vec()).await.unwrap();
        assert!(
            until(&mut sp, &mut out, next).await,
            "not at {next:?}: {}",
            String::from_utf8_lossy(&out)
        );
    }
    stop(&mut sp).await;
    let said = String::from_utf8_lossy(&out).into_owned();
    assert!(said.contains(" · done"), "the summary: {said}");
    assert!(home.join(".codex/hooks.json").exists());
    assert!(home.join(".relay/config.json").exists());
}
