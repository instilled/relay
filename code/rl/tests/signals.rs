//! A killed list gives the terminal back: `rl` on a pty, sent a signal.

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use rl_session::backend::{self, Spec};
use std::time::Duration;

async fn killed_by(signal: Signal) -> (i32, String) {
    let tmp = tempfile::tempdir().unwrap();
    // Started before: the first start's question is not asked.
    std::fs::create_dir_all(tmp.path().join("relay")).unwrap();
    std::fs::write(tmp.path().join("relay/config.json"), "{}").unwrap();
    let mut sp = backend::spawn(&Spec {
        argv: vec![env!("CARGO_BIN_EXE_rl").into()],
        cwd: tmp.path().to_path_buf(),
        // Its own HOME too: the list's setup check reads the harnesses'
        // files there, never the real ones.
        env: vec![
            ("RELAY_HOME".into(), tmp.path().join("relay").display().to_string()),
            ("HOME".into(), tmp.path().display().to_string()),
        ],
        inherit_env: true,
        cols: 100,
        rows: 30,
        settle: Duration::ZERO,
    })
    .unwrap();
    let mut out = Vec::new();
    // Until the list has taken the alternate screen.
    let entered = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(b) = sp.output.recv().await {
            out.extend_from_slice(&b);
            if String::from_utf8_lossy(&out).contains("\x1b[?1049h") {
                return true;
            }
        }
        false
    })
    .await;
    assert!(matches!(entered, Ok(true)), "the list never took the screen");
    kill(Pid::from_raw(sp.pid as i32), signal).unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            tokio::select! {
                b = sp.output.recv() => match b {
                    Some(b) => out.extend_from_slice(&b),
                    None => break,
                },
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
        (&mut sp.exit).await
    })
    .await
    .expect("the list did not stop")
    .unwrap();
    while let Ok(b) = sp.output.try_recv() {
        out.extend_from_slice(&b);
    }
    (exit.code, String::from_utf8_lossy(&out).into_owned())
}

async fn leaves_cleanly(signal: Signal) {
    let (code, out) = killed_by(signal).await;
    assert_eq!(code, 0, "{signal:?}");
    let after = &out[out.rfind("\x1b[?1049h").unwrap()..];
    assert!(after.contains("\x1b[?1049l"), "{signal:?}: alternate screen left");
    assert!(after.contains("\x1b[?1000l"), "{signal:?}: mouse capture off");
    assert!(after.contains("\x1b[?2004l"), "{signal:?}: bracketed paste off");
}

#[tokio::test]
async fn sigterm_leaves_the_screen_and_exits_zero() {
    leaves_cleanly(Signal::SIGTERM).await;
}

#[tokio::test]
async fn sighup_leaves_the_screen_and_exits_zero() {
    leaves_cleanly(Signal::SIGHUP).await;
}

#[tokio::test]
async fn sigint_leaves_the_screen_and_exits_zero() {
    leaves_cleanly(Signal::SIGINT).await;
}
