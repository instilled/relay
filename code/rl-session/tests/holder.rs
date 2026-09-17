//! The holder through its real binary and the client library, no daemon.

use nix::sys::signal::kill;
use nix::unistd::Pid;
use rl_session::client::Session;
use rl_session::exit::read_exit_file;
use rl_session::proto::{Frame, Hello};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn fake_cli() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fake-cli")
}

struct Holder {
    socket: PathBuf,
    exit_file: PathBuf,
    _tmp: tempfile::TempDir,
}

/// Spawns `rl-session` for `fake-cli` and waits for it to report ready.
fn start(tty: bool, linger: u64) -> Holder {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("s.sock");
    let exit_file = tmp.path().join("s.exit");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rl-session"));
    cmd.arg("--id")
        .arg("test0001")
        .arg("--socket")
        .arg(&socket)
        .arg("--exit-file")
        .arg(&exit_file)
        .arg("--cwd")
        .arg(tmp.path())
        .arg("--linger")
        .arg(linger.to_string())
        .arg("--env")
        .arg("RELAY_RUN=test0001");
    if !tty {
        cmd.arg("--no-tty");
    }
    cmd.arg("--").arg(fake_cli());
    // The detached holder keeps stderr for its log, so never `output()` here.
    let log = tmp.path().join("holder.log");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap());
    let status = cmd.status().unwrap();
    assert!(
        status.success(),
        "rl-session did not become ready: {}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    assert!(socket.exists(), "socket must exist once the parent returns");
    Holder {
        socket,
        exit_file,
        _tmp: tmp,
    }
}

async fn attach(h: &Holder, replay: bool) -> (Hello, Session) {
    Session::attach(&h.socket, replay).await.unwrap()
}

/// Reads frames until `needle` appears in the accumulated output.
async fn read_until(s: &mut Session, needle: &str) -> String {
    let mut acc = String::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !acc.contains(needle) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let frame = tokio::time::timeout(remaining, s.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}; got {acc:?}"))
            .unwrap();
        match frame {
            Some(Frame::Output(b)) => acc.push_str(&String::from_utf8_lossy(&b)),
            Some(Frame::Exit(e)) => panic!("exited ({e:?}) before {needle:?}; got {acc:?}"),
            Some(_) => {}
            None => panic!("holder closed before {needle:?}; got {acc:?}"),
        }
    }
    acc
}

async fn read_exit(s: &mut Session) -> rl_session::exit::Exit {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, s.recv())
            .await
            .expect("timed out waiting for exit")
            .unwrap()
        {
            Some(Frame::Exit(e)) => return e,
            Some(_) => {}
            None => panic!("holder closed before exit"),
        }
    }
}

fn alive(pid: u32) -> bool {
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

async fn wait_gone(path: &Path, pid: u32) {
    for _ in 0..100 {
        if !path.exists() && !alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("holder {pid} / socket {} still around", path.display());
}

#[tokio::test]
async fn pty_lifecycle() {
    let h = start(true, 60);
    let (hello, mut s) = attach(&h, true).await;
    assert!(hello.tty);
    assert_eq!(hello.id, "test0001");
    assert_ne!(hello.pid, hello.holder_pid);
    assert!(alive(hello.pid) && alive(hello.holder_pid));
    assert_eq!((hello.cols, hello.rows), (80, 24));

    read_until(&mut s, "fake-cli ready").await;
    s.input("hi\n").await.unwrap();
    read_until(&mut s, "echo: hi").await;

    s.resize(100, 40).await.unwrap();
    s.input("size\n").await.unwrap();
    read_until(&mut s, "40 100").await;

    s.input("exit 3\n").await.unwrap();
    let exit = read_exit(&mut s).await;
    assert_eq!((exit.code, exit.signal.as_deref()), (3, None));
    let on_disk = read_exit_file(&h.exit_file).unwrap().expect("exit file");
    assert_eq!(on_disk.code, 3);
    assert!(!alive(hello.pid));

    s.close().await.unwrap();
    wait_gone(&h.socket, hello.holder_pid).await;
}

#[tokio::test]
async fn survives_client_going_away_and_replays() {
    let h = start(true, 60);
    let (hello, mut s) = attach(&h, true).await;
    read_until(&mut s, "fake-cli ready").await;
    s.input("hi\n").await.unwrap();
    read_until(&mut s, "echo: hi").await;
    drop(s); // the daemon dies

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(alive(hello.pid), "child must outlive its client");
    assert!(alive(hello.holder_pid));

    let (hello2, mut s) = attach(&h, true).await;
    assert_eq!(hello2.pid, hello.pid);
    let replayed = read_until(&mut s, "echo: hi").await;
    assert!(replayed.contains("fake-cli ready"), "scrollback replayed: {replayed:?}");
    s.input("again\n").await.unwrap();
    read_until(&mut s, "echo: again").await;

    // A second, replay-less attach sees only what happens from now on.
    let (_, mut w) = attach(&h, false).await;
    s.input("both\n").await.unwrap();
    let seen = read_until(&mut w, "echo: both").await;
    assert!(!seen.contains("fake-cli ready"), "no replay requested: {seen:?}");

    s.input("exit\n").await.unwrap();
    assert_eq!(read_exit(&mut s).await.code, 0);
    assert_eq!(read_exit(&mut w).await.code, 0);
    s.close().await.unwrap();
    wait_gone(&h.socket, hello.holder_pid).await;
}

#[tokio::test]
async fn late_attach_sees_exit() {
    let h = start(true, 60);
    let (hello, mut s) = attach(&h, true).await;
    read_until(&mut s, "fake-cli ready").await;
    s.input("exit 5\n").await.unwrap();
    assert_eq!(read_exit(&mut s).await.code, 5);
    drop(s);

    let (hello2, mut s) = attach(&h, true).await;
    assert_eq!(hello2.exit.as_ref().map(|e| e.code), Some(5));
    let replayed = read_until(&mut s, "bye").await;
    assert!(replayed.contains("fake-cli ready"));
    assert_eq!(read_exit(&mut s).await.code, 5);
    s.close().await.unwrap();
    wait_gone(&h.socket, hello.holder_pid).await;
}

#[tokio::test]
async fn no_tty_uses_pipes() {
    let h = start(false, 60);
    let (hello, mut s) = attach(&h, true).await;
    assert!(!hello.tty);
    read_until(&mut s, "fake-cli ready").await;
    s.input("size\n").await.unwrap();
    read_until(&mut s, "no tty").await;
    s.resize(10, 10).await.unwrap(); // no-op, must not break anything
    s.input("still\n").await.unwrap();
    read_until(&mut s, "echo: still").await;
    s.input("exit\n").await.unwrap();
    assert_eq!(read_exit(&mut s).await.code, 0);
    assert_eq!(read_exit_file(&h.exit_file).unwrap().unwrap().code, 0);
    s.close().await.unwrap();
    wait_gone(&h.socket, hello.holder_pid).await;
}

#[tokio::test]
async fn signal_reaches_child() {
    let h = start(true, 60);
    let (hello, mut s) = attach(&h, true).await;
    read_until(&mut s, "fake-cli ready").await;
    s.signal(15).await.unwrap();
    let exit = read_exit(&mut s).await;
    assert_eq!(exit.signal.as_deref(), Some("SIGTERM"), "{exit:?}");
    assert!(!exit.success());
    s.close().await.unwrap();
    wait_gone(&h.socket, hello.holder_pid).await;
}

#[tokio::test]
async fn lingers_then_leaves() {
    let h = start(true, 1);
    let (hello, mut s) = attach(&h, true).await;
    read_until(&mut s, "fake-cli ready").await;
    s.input("exit\n").await.unwrap();
    assert_eq!(read_exit(&mut s).await.code, 0);
    drop(s);
    wait_gone(&h.socket, hello.holder_pid).await;
}

#[tokio::test]
async fn close_hangs_up_a_live_child() {
    let h = start(true, 60);
    let (hello, mut s) = attach(&h, true).await;
    read_until(&mut s, "fake-cli ready").await;
    s.close().await.unwrap();
    wait_gone(&h.socket, hello.holder_pid).await;
    assert!(!alive(hello.pid), "child should have been hung up");
}

#[tokio::test]
async fn leaves_when_its_socket_is_removed() {
    // The Relay home it lived in is gone: no daemon can come back for it, so
    // it hangs up a child that is still going and leaves, linger or not.
    let h = start(true, 60);
    let (hello, mut s) = attach(&h, true).await;
    read_until(&mut s, "fake-cli ready").await;
    drop(s);
    std::fs::remove_file(&h.socket).unwrap();
    for _ in 0..100 {
        if !alive(hello.holder_pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(hello.holder_pid), "holder should have left");
    assert!(!alive(hello.pid), "child should have been hung up");
}

#[tokio::test]
async fn replay_from_an_offset() {
    let h = start(false, 60); // pipes: byte-exact output
    let (hello, mut s) = attach(&h, true).await;
    // The child may have printed its banner before we attached; from 0 we
    // get all of it either way.
    assert_eq!(hello.replay_from, 0);
    read_until(&mut s, "fake-cli ready").await;
    s.input("hi\n").await.unwrap();
    read_until(&mut s, "echo: hi").await;
    drop(s);

    // Everything after the banner, and nothing of it.
    let banner = "fake-cli ready\n".len() as u64;
    let (hello2, mut s) = Session::attach_from(&h.socket, Some(banner)).await.unwrap();
    assert_eq!(hello2.replay_from, banner);
    assert_eq!(hello2.total, banner + "> echo: hi\n> ".len() as u64);
    let replayed = read_until(&mut s, "echo: hi").await;
    assert!(!replayed.contains("ready"), "{replayed:?}");

    // Past the end: nothing to replay, and the offset says so.
    let (hello3, mut w) = Session::attach_from(&h.socket, Some(u64::MAX)).await.unwrap();
    assert_eq!(hello3.replay_from, hello3.total);
    s.input("exit\n").await.unwrap();
    assert_eq!(read_exit(&mut s).await.code, 0);
    let last = read_exit(&mut w).await;
    assert_eq!(last.code, 0);
    s.close().await.unwrap();
    wait_gone(&h.socket, hello.holder_pid).await;
}

#[tokio::test]
async fn refuses_to_start_a_missing_command() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rl-session"))
        .args(["--id", "x", "--socket"])
        .arg(tmp.path().join("s.sock"))
        .args(["--", "/nonexistent/definitely-not-here"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("spawning"), "{err}");
}
