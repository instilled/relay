//! The attach loop against a real `rl-session` holder, with in-memory stdin/stdout.

use rl::attach::Ended;
use rl::attach::held::{View, drive};
use rl::spawn::{self, Start};
use rl::store::sessions::Meta;
use rl_core::Home;
use rl_session::client::Session;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc;

fn session_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rl-new")).with_file_name("rl-session");
    p.is_file().then_some(p)
}

struct Env {
    _tmp: tempfile::TempDir,
    home: Home,
    cwd: PathBuf,
}

impl Env {
    fn new() -> Option<Self> {
        let Some(bin) = session_bin() else {
            eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
            return None;
        };
        // SAFETY: set before any thread of this test reads it, and never changed.
        unsafe { std::env::set_var(spawn::SESSION_BIN_ENV, bin) };
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        Some(Self {
            home: Home::at(tmp.path().join("relay")),
            cwd,
            _tmp: tmp,
        })
    }

    async fn start(&self, script: &str) -> Meta {
        spawn::start(
            &self.home,
            Start {
                harness: None,
                cwd: self.cwd.clone(),
                argv: vec!["sh".into(), "-c".into(), script.into()],
                cols: 80,
                rows: 24,
            },
        )
        .await
        .unwrap()
    }
}

struct Run {
    ended: Ended,
    out: String,
}

/// Drives the loop with `typed` sent as stdin (each chunk a moment apart),
/// holding stdin open after, as a terminal's would be.
async fn run(meta: &Meta, typed: &[&[u8]], sizes: &[(u16, u16)], size: Option<(u16, u16)>) -> Run {
    let (hello, session) = Session::attach_screen(&meta.socket, 0).await.unwrap();
    let (mut stdin_w, stdin_r): (DuplexStream, DuplexStream) = tokio::io::duplex(4096);
    let (sizes_tx, sizes_rx) = mpsc::channel(8);
    let typed: Vec<Vec<u8>> = typed.iter().map(|t| t.to_vec()).collect();
    let sizes = sizes.to_vec();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        for s in sizes {
            sizes_tx.send(s).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        for t in typed {
            stdin_w.write_all(&t).await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop((stdin_w, sizes_tx));
    });
    let mut out = Vec::new();
    let view = View {
        name: &meta.name,
        interactive: false,
        size,
    };
    let ended = tokio::time::timeout(
        Duration::from_secs(15),
        drive(session, &hello, stdin_r, &mut out, sizes_rx, view),
    )
    .await
    .expect("came back")
    .unwrap();
    Run {
        ended,
        out: String::from_utf8_lossy(&out).into_owned(),
    }
}

async fn stop(meta: &Meta) {
    let (_, mut s) = Session::attach(&meta.socket, false).await.unwrap();
    s.close().await.unwrap();
}

#[tokio::test]
async fn typed_input_reaches_the_session_and_its_exit_ends_the_attach() {
    let Some(env) = Env::new() else { return };
    let meta = env.start("read x; echo got $x").await;
    let r = run(&meta, &[b"hi\r"], &[], None).await;
    assert!(r.out.contains("got hi"), "{}", r.out);
    assert!(matches!(r.ended, Ended::Exit { code: 0, .. }), "{:?}", r.ended);
    assert!(r.out.contains("ended (exit 0)"), "{}", r.out);
}

#[tokio::test]
async fn ctrl_backslash_detaches_and_the_session_keeps_going() {
    let Some(env) = Env::new() else { return };
    let meta = env.start("read x; echo got $x; sleep 30").await;
    let r = run(&meta, &[b"ab\x1c"], &[], None).await;
    assert_eq!(r.ended, Ended::Detached);
    // It is still there to be attached to again.
    Session::attach(&meta.socket, false).await.unwrap();
    stop(&meta).await;
}

#[tokio::test]
async fn a_new_window_size_reaches_the_session() {
    let Some(env) = Env::new() else { return };
    let meta = env.start("read x; stty size").await;
    let r = run(&meta, &[b"\r"], &[(100, 40)], Some((80, 24))).await;
    assert!(r.out.contains("40 100"), "{}", r.out);
    assert!(matches!(r.ended, Ended::Exit { code: 0, .. }), "{:?}", r.ended);
}

#[tokio::test]
async fn an_ended_session_is_read_from_its_exit_file() {
    let Some(env) = Env::new() else { return };
    let meta = env.start("exit 3").await;
    let dir = env.home.session_dir(&meta.name);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !dir.join("exit").is_file() {
        assert!(std::time::Instant::now() < deadline, "no exit file");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (ended, words) = rl::attach::held::from_exit_file(&dir, &meta.name).unwrap();
    assert!(matches!(ended, Ended::Exit { code: 3, .. }), "{ended:?}");
    assert_eq!(ended.code(), 3);
    assert!(words.contains("ended (exit 3)"), "{words}");
}
