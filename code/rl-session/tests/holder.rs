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
    start_with(tty, linger, &[])
}

fn start_with(tty: bool, linger: u64, extra: &[&str]) -> Holder {
    start_argv(tty, linger, extra, &fake_cli())
}

/// As [`start_with`], running `program` in place of the fake CLI.
fn start_argv(tty: bool, linger: u64, extra: &[&str], program: &Path) -> Holder {
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
        .arg("--socket-check")
        .arg("1")
        .arg("--env")
        .arg("RELAY_RUN=test0001");
    if !tty {
        cmd.arg("--no-tty");
    }
    cmd.args(extra);
    cmd.arg("--").arg(program);
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
async fn nobody_for_a_while_lets_the_run_go() {
    // A throwaway home left on disk: the socket is there, but no daemon will
    // ever come back for the run.
    let h = start_with(true, 60, &["--reap-after", "1"]);
    let (hello, mut s) = attach(&h, true).await;
    read_until(&mut s, "fake-cli ready").await;
    // Attached: it stays.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(alive(hello.holder_pid) && alive(hello.pid), "someone is here");
    drop(s);
    // Alone: the child is hung up and the holder goes.
    wait_gone(&h.socket, hello.holder_pid).await;
    assert!(!alive(hello.pid), "the child went with it");
}

/// Reads whole frames until `needle` has arrived, and says how many it took.
async fn count_until(s: &mut Session, needle: &str) -> (usize, String) {
    let mut acc = String::new();
    let mut frames = 0;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !acc.contains(needle) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let frame = tokio::time::timeout(remaining, s.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}; got {acc:?}"))
            .unwrap();
        match frame {
            Some(Frame::Output(b)) => {
                frames += 1;
                acc.push_str(&String::from_utf8_lossy(&b));
            }
            Some(_) => {}
            None => panic!("holder closed before {needle:?}; got {acc:?}"),
        }
    }
    (frames, acc)
}

/// A child writing as fast as it can used to wake the holder once per `printf`
/// — a wake-up and an allocation for a few bytes — and, because the holder
/// drained the pty the instant anything landed, the child never met the
/// backpressure a terminal would have given it. Settling collapses the burst
/// into a handful of reads.
#[tokio::test]
async fn a_burst_of_small_writes_arrives_in_a_few_chunks() {
    const LINES: usize = 60;
    let h = start_with(true, 60, &["--settle", "200"]);
    let (_, mut s) = attach(&h, true).await;
    read_until(&mut s, "fake-cli ready").await;
    s.input(format!("burst {LINES}\n")).await.unwrap();
    let (frames, out) = count_until(&mut s, &format!("tick {LINES}")).await;
    assert!(out.matches("tick ").count() >= LINES, "every line arrives: {out:?}");
    assert!(frames <= LINES / 4, "{LINES} writes should not cost {frames} chunks");
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

/// Reads frames until the first foreground frame arrives, returning its value.
async fn read_foreground(s: &mut Session) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, s.recv())
            .await
            .expect("timed out waiting for a foreground frame")
            .unwrap()
        {
            Some(Frame::Foreground(b)) => return b,
            Some(Frame::Exit(e)) => panic!("exited before a foreground frame: {e:?}"),
            Some(_) => {}
            None => panic!("holder closed before a foreground frame"),
        }
    }
}

/// A tty run reports the foreground state to every client that attaches: the
/// fake CLI rests owning its own terminal, so the first thing a client learns
/// after the replay is `Foreground(true)`.
#[tokio::test]
async fn a_tty_attach_learns_the_foreground_state() {
    let h = start(true, 60);
    let (hello, mut s) = attach(&h, true).await;
    assert!(hello.tty);
    assert!(read_foreground(&mut s).await, "a resting shell owns its own terminal");
    s.input("exit\n").await.unwrap();
    assert_eq!(read_exit(&mut s).await.code, 0);
    s.close().await.unwrap();
    wait_gone(&h.socket, hello.holder_pid).await;
}

/// A pipe run has no terminal to read a foreground group from, so a client is
/// never sent a foreground frame — not on attach, not ever.
#[tokio::test]
async fn a_pipes_run_never_frames_foreground() {
    let h = start(false, 60);
    let (hello, mut s) = attach(&h, true).await;
    assert!(!hello.tty);
    read_until(&mut s, "fake-cli ready").await;
    s.input("exit\n").await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, s.recv())
            .await
            .expect("timed out draining a pipe run")
            .unwrap()
        {
            Some(Frame::Foreground(_)) => panic!("a pipe run must not frame foreground"),
            Some(Frame::Exit(_)) | None => break,
            Some(_) => {}
        }
    }
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

/// With no input, the child's read ends at once: `fake-cli` leaves on its own,
/// and what is typed goes nowhere without breaking anything.
#[tokio::test]
async fn no_input_ends_the_childs_input_at_once() {
    let h = start_with(false, 60, &["--no-input"]);
    let (hello, mut s) = attach(&h, true).await;
    s.input("echo me\n").await.unwrap();
    let out = read_until(&mut s, "fake-cli ready").await;
    assert_eq!(read_exit(&mut s).await.code, 0);
    assert!(!out.contains("echo: echo me"), "{out}");
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

#[tokio::test]
async fn a_screen_attach_draws_the_screen_once() {
    let h = start_argv(true, 5, &[], Path::new("/bin/sh"));
    let (_, mut s) = attach(&h, false).await;
    // A counter redrawn in place: every value is in the bytes, only the
    // last is on the screen. The empty quotes keep the echoed command line
    // from containing the word it waits for.
    s.input(b"i=0; while [ $i -lt 100 ]; do printf 'n=%03d\\r' $i; i=$((i+1)); done; echo; echo dr''awn\n".to_vec())
        .await
        .unwrap();
    read_until(&mut s, "drawn").await;
    let (hello, mut v) = Session::attach_screen(&h.socket, 0).await.unwrap();
    assert_eq!(hello.replay_from, hello.total);
    let mut drawn = Vec::new();
    loop {
        match v.recv().await.unwrap() {
            Some(Frame::Output(b)) => drawn.extend(b),
            Some(Frame::ReplayEnd) => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    let drawn = String::from_utf8_lossy(&drawn);
    assert!(drawn.contains("n=099"), "{drawn:?}");
    assert!(!drawn.contains("n=098"), "{drawn:?}");
}

#[tokio::test]
async fn a_pipes_run_answers_a_screen_attach_with_bytes() {
    let h = start_argv(false, 5, &[], Path::new("/bin/sh"));
    let (_, mut s) = attach(&h, false).await;
    s.input(b"echo pi''ped\n".to_vec()).await.unwrap();
    read_until(&mut s, "piped").await;
    let (hello, mut v) = Session::attach_screen(&h.socket, 0).await.unwrap();
    assert_eq!(hello.replay_from, 0, "the bytes replay from where it asked");
    assert!(read_until(&mut v, "piped").await.contains("piped"));
}

// --- the holder's own state, with nobody attached ---------------------------

use rl_core::state::{RunKind, RunState};
use rl_session::detect::spec::DetectSpec;
use rl_session::state::{StateFile, read_state_file};

/// Spawns `rl-session` running `argv` with `--detect`/`--state-file`; nobody
/// attaches. Returns the holder and the state file's path.
fn start_detecting(tty: bool, argv: &[&str], spec: &DetectSpec) -> (Holder, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("s.sock");
    let exit_file = tmp.path().join("s.exit");
    let state = tmp.path().join("s.state.json");
    let detect = tmp.path().join("detect.json");
    std::fs::write(&detect, serde_json::to_vec(spec).unwrap()).unwrap();
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
        .arg("60")
        .arg("--socket-check")
        .arg("1")
        .arg("--detect")
        .arg(&detect)
        .arg("--state-file")
        .arg(&state);
    if !tty {
        cmd.arg("--no-tty");
    }
    cmd.arg("--").args(argv);
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
    (
        Holder {
            socket,
            exit_file,
            _tmp: tmp,
        },
        state,
    )
}

fn spec(kind: RunKind, needs_input: &[&str], idle_ms: u64, stale_ms: u64) -> DetectSpec {
    DetectSpec {
        kind,
        needs_input: needs_input.iter().map(|s| s.to_string()).collect(),
        idle: vec![],
        idle_after_ms: idle_ms,
        stale_after_ms: stale_ms,
    }
}

/// Polls the state file every 50 ms, up to 10 s, until `pred` holds.
async fn wait_state(path: &Path, pred: impl Fn(&StateFile) -> bool) -> StateFile {
    let mut last = None;
    for _ in 0..200 {
        if let Ok(Some(s)) = read_state_file(path) {
            if pred(&s) {
                return s;
            }
            last = Some(s);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("state never matched; last seen {last:?}");
}

#[tokio::test]
async fn state_advances_with_nobody_attached() {
    let sp = spec(RunKind::Agent, &[r"\[y/N\]"], 200, 600_000);
    let (_h, state) = start_detecting(false, &["sh", "-c", r#"printf "Continue? [y/N] "; sleep 30"#], &sp);
    let s = wait_state(&state, |s| s.state == RunState::NeedsInput).await;
    assert_eq!(s.detail.as_deref(), Some("Continue? [y/N]"));
}

#[tokio::test]
async fn a_quiet_worker_goes_stale_with_nobody_attached() {
    let sp = spec(RunKind::Command, &[], 200, 500);
    let (_h, state) = start_detecting(false, &["sh", "-c", "echo working; sleep 30"], &sp);
    wait_state(&state, |s| s.state == RunState::Stale).await;
}

#[tokio::test]
async fn resting_shell_reads_idle_with_nobody_attached() {
    // The pattern would match the PS1; the shell owning the foreground must win.
    let sp = spec(RunKind::Shell, &[r"\$\s*$"], 200, 600_000);
    let (_h, state) = start_detecting(true, &["/bin/sh", "-i"], &sp);
    let s = wait_state(&state, |s| s.state == RunState::Idle && s.foreground == Some(true)).await;
    assert_eq!(s.foreground, Some(true));
}

#[tokio::test]
async fn exit_leaves_state_and_exit_files() {
    let sp = spec(RunKind::Command, &[], 200, 600_000);
    let (h, state) = start_detecting(false, &["sh", "-c", "echo bye; exit 3"], &sp);
    for _ in 0..200 {
        if h.exit_file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(read_exit_file(&h.exit_file).unwrap().unwrap().code, 3);
    let s = wait_state(&state, |s| s.last_line.as_deref() == Some("bye")).await;
    assert_eq!(s.last_line.as_deref(), Some("bye"));
}

#[tokio::test]
async fn unwritable_state_file_does_not_stop_the_holder() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing-dir").join("state.json");
    let detect = tmp.path().join("detect.json");
    let sp = spec(RunKind::Command, &[], 200, 600_000);
    std::fs::write(&detect, serde_json::to_vec(&sp).unwrap()).unwrap();
    let h = start_with(
        true,
        60,
        &[
            "--detect",
            detect.to_str().unwrap(),
            "--state-file",
            missing.to_str().unwrap(),
        ],
    );
    let (_hello, mut s) = attach(&h, true).await;
    read_until(&mut s, "fake-cli ready").await;
    assert!(!missing.exists());
}

#[tokio::test]
async fn without_detect_no_state_file_appears() {
    let h = start(true, 5);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let dir = h.socket.parent().unwrap();
    let stray: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("state"))
        .collect();
    assert!(stray.is_empty(), "unexpected {stray:?}");
}
