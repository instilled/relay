//! `run.attach` over the socket: scrollback then live, several watchers and
//! one joiner, input journaled, take-over, and an ended run's replay.

mod common;

use common::{Conn, Harness, fake_cli, journal, wait_journal};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use rl_core::Home;
use rl_ledger::session_io::{self, Direction};
use rl_ledger::{Ledger, presence, sessions};
use rl_proto::rpc::RpcError;
use rl_proto::types::{Run, RunState};
use rl_session::proto::{Frame, read_frame, write_frame};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::io::BufReader;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

struct Viewer {
    rd: BufReader<OwnedReadHalf>,
    wr: OwnedWriteHalf,
    seen: String,
}

impl Viewer {
    /// Attaches a fresh connection; `Err` is the RPC error.
    async fn attach(home: &Home, id: &str, client: &str, take: bool) -> Result<(Value, Self), RpcError> {
        Self::attach_from(home, id, client, None, take).await
    }

    /// Attaches from one terminal of the client's.
    async fn attach_from(
        home: &Home,
        id: &str,
        client: &str,
        terminal: Option<&str>,
        take: bool,
    ) -> Result<(Value, Self), RpcError> {
        let mut c = Conn::open(home).await;
        let res = c
            .call(
                "run.attach",
                json!({ "id": id, "client": client, "terminal": terminal, "take": take, "scrollback": 4096 }),
            )
            .await;
        if let Some(e) = res.error {
            return Err(e);
        }
        let (rd, wr) = c.into_parts();
        Ok((
            res.result.unwrap(),
            Self {
                rd,
                wr,
                seen: String::new(),
            },
        ))
    }

    /// Reads frames until `needle` has appeared in the output, returning
    /// every non-output frame seen on the way.
    async fn until(&mut self, needle: &str) -> Vec<Frame> {
        let mut others = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.seen.contains(needle) {
            let left = deadline.saturating_duration_since(Instant::now());
            let f = tokio::time::timeout(left, read_frame(&mut self.rd))
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}; seen {:?}", self.seen))
                .unwrap()
                .unwrap_or_else(|| panic!("stream ended waiting for {needle:?}; seen {:?}", self.seen));
            match f {
                Frame::Output(b) => self.seen.push_str(&String::from_utf8_lossy(&b)),
                other => others.push(other),
            }
        }
        others
    }

    async fn next(&mut self) -> Option<Frame> {
        tokio::time::timeout(Duration::from_secs(5), read_frame(&mut self.rd))
            .await
            .expect("frame in time")
            .unwrap()
    }

    async fn input(&mut self, s: &str) {
        write_frame(&mut self.wr, &Frame::Input(s.as_bytes().to_vec()))
            .await
            .unwrap();
    }
}

fn notes(frames: &[Frame]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|f| match f {
            Frame::Note(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn one_viewer_who_is_the_joiner_and_a_take_over() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(h) = Harness::on(Home::at(tmp.path())) else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent", "cols": 100, "rows": 30 }),
        )
        .await;
    wait_journal(&h.home, &run.id, "> ").await;

    // Attaching to a live run joins it: scrollback from the journal, then the
    // holder's live stream, and the keyboard. It comes once the run is past
    // the age at which a join is nudged (once, at that age), and says its size
    // as a terminal does.
    past_young().await;
    let (info, mut joiner) = Viewer::attach(&h.home, &run.id, "bob@desk", false).await.unwrap();
    assert_eq!(info["live"], true);
    assert_eq!(info["cols"], 100);
    assert_eq!(info["joiner"], "bob@desk");
    assert!(info["journaled"].as_u64().unwrap() > 0);
    assert_eq!(
        info["replay_from"], info["journaled"],
        "no gap: replay picks up where the journal ends"
    );
    let frames = joiner.until("fake-cli ready").await;
    assert!(frames.is_empty() || matches!(frames[0], Frame::ReplayEnd));
    resize(&mut joiner, 100, 30).await;
    joiner.input("size\n").await;
    joiner.until("30 100").await;
    joiner.input("hello\n").await;
    joiner.until("echo: hello").await;

    // Input is journaled as such.
    let ledger = Ledger::open(&h.home.ledger()).unwrap();
    let id = run.id.clone();
    let ins: Vec<Vec<u8>> = ledger
        .call_blocking(move |db| session_io::read(db, &id, 0, 10_000))
        .unwrap()
        .into_iter()
        .filter(|c| c.direction == Direction::In)
        .map(|c| c.data)
        .collect();
    assert_eq!(ins, [b"size\n".to_vec(), b"hello\n".to_vec()]);
    assert!(
        ledger
            .call_blocking(|db| presence::get(db, "bob@desk"))
            .unwrap()
            .is_some(),
        "joining is presence"
    );

    // A second viewer is refused, unless it takes over.
    let err = Viewer::attach(&h.home, &run.id, "cat@phone", false)
        .await
        .err()
        .unwrap();
    assert_eq!(err.code, RpcError::BUSY);
    assert!(err.message.contains("bob@desk"), "{}", err.message);
    let (info, mut taker) = Viewer::attach(&h.home, &run.id, "cat@phone", true).await.unwrap();
    assert_eq!(info["joiner"], "cat@phone");
    taker.until("echo: hello").await;
    // Bob keeps the stream he is on, told he has lost the keyboard and muted.
    joiner.input("from bob\n").await;
    taker.input("from cat\n").await;
    taker.until("echo: from cat").await;
    let frames = joiner.until("echo: from cat").await;
    assert_eq!(notes(&frames), ["cat@phone took over from bob@desk"]);
    assert!(!joiner.seen.contains("echo: from bob"), "{}", joiner.seen);

    // The run ends while both are attached: each viewer gets the exit.
    taker.input("exit 4\n").await;
    for v in [&mut joiner, &mut taker] {
        v.until("bye").await;
        let exit = loop {
            match v.next().await {
                Some(Frame::Exit(e)) => break e,
                Some(_) => continue,
                None => panic!("stream ended without an exit frame"),
            }
        };
        assert_eq!(exit.code, 4);
    }
    let done = c.wait_terminal(&run.id).await;
    assert_eq!((done.state, done.exit_code), (RunState::Failed, Some(4)));

    // Ended: scrollback, then the exit, no holder involved — and no seat to
    // claim, so anyone may read it.
    let (info, mut late) = Viewer::attach(&h.home, &run.id, "ann@laptop", false).await.unwrap();
    assert_eq!(info["live"], false);
    let frames = late.until("echo: from cat").await;
    let _ = frames;
    let mut got_exit = None;
    while let Some(f) = late.next().await {
        if let Frame::Exit(e) = f {
            got_exit = Some(e);
            break;
        }
    }
    assert_eq!(got_exit.unwrap().code, 4);
    assert!(late.seen.contains("fake-cli ready"), "{}", late.seen);

    let err = Viewer::attach(&h.home, "nope0000", "x@y", false).await.err().unwrap();
    assert_eq!(err.code, RpcError::NOT_FOUND);
    h.stop().await;
}

#[tokio::test]
async fn run_input_writes_to_the_pty_without_a_join() {
    // The warm-shell path: `run.input` feeds a run's stdin over the daemon's
    // own holder connection — no interactive join — and it is journaled.
    let tmp = tempfile::tempdir().unwrap();
    let Some(h) = Harness::on(Home::at(tmp.path())) else {
        eprintln!("skipping: rl-session not built");
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent" }),
        )
        .await;
    wait_journal(&h.home, &run.id, "> ").await;

    // No join, just the write — fake-cli answers a line with `echo: <line>`.
    let res = c
        .call("run.input", json!({ "id": run.id, "bytes": b"hi\n".to_vec() }))
        .await;
    assert!(res.error.is_none(), "run.input: {:?}", res.error);

    // A viewer attaching after sees the echo, and the input is on the record as input.
    let (_info, mut w) = Viewer::attach(&h.home, &run.id, "ann@laptop", false).await.unwrap();
    w.until("echo: hi").await;
    let ledger = Ledger::open(&h.home.ledger()).unwrap();
    let id = run.id.clone();
    let ins: Vec<Vec<u8>> = ledger
        .call_blocking(move |db| session_io::read(db, &id, 0, 10_000))
        .unwrap()
        .into_iter()
        .filter(|c| c.direction == Direction::In)
        .map(|c| c.data)
        .collect();
    assert_eq!(ins, [b"hi\n".to_vec()]);
    h.stop().await;
}

#[tokio::test]
async fn two_terminals_of_one_person_are_two_seats() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(h) = Harness::on(Home::at(tmp.path())) else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent" }),
        )
        .await;
    wait_journal(&h.home, &run.id, "> ").await;

    let (_, mut first) = Viewer::attach_from(&h.home, &run.id, "me@here", Some("111"), false)
        .await
        .unwrap();
    first.until("fake-cli ready").await;

    // The same terminal again is still the same seat.
    let (info, _again) = Viewer::attach_from(&h.home, &run.id, "me@here", Some("111"), false)
        .await
        .unwrap();
    assert_eq!(info["joiner"], "me@here");

    // Another terminal of mine is refused, and told which.
    let err = Viewer::attach_from(&h.home, &run.id, "me@here", Some("222"), false)
        .await
        .err()
        .unwrap();
    assert_eq!(err.code, RpcError::BUSY);
    assert!(err.message.contains("me@here in another terminal"), "{}", err.message);
    assert_eq!(err.data.unwrap()["joiner"], "me@here in another terminal");

    // Taking over: the first terminal hears it, and its keys stop counting.
    let (_, mut second) = Viewer::attach_from(&h.home, &run.id, "me@here", Some("222"), true)
        .await
        .unwrap();
    second.until("fake-cli ready").await;
    first.input("from the first\n").await;
    second.input("from the second\n").await;
    let heard = first.until("echo: from the second").await;
    assert!(
        notes(&heard)
            .iter()
            .any(|n| n == "me@here took over in another terminal"),
        "{:?}",
        notes(&heard)
    );
    assert!(!first.seen.contains("echo: from the first"), "{}", first.seen);
    let _ = c.call("run.stop", json!({ "id": run.id })).await;
}

#[tokio::test]
async fn joiner_resizes_the_pty_and_daemon_stop_detaches() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(h) = Harness::on(Home::at(tmp.path())) else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = tmp.path().to_string_lossy().into_owned();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent" }),
        )
        .await;
    let (_, mut joiner) = Viewer::attach(&h.home, &run.id, "me@here", false).await.unwrap();
    write_frame(&mut joiner.wr, &Frame::Resize { cols: 77, rows: 21 })
        .await
        .unwrap();
    joiner.input("size\n").await;
    joiner.until("21 77").await;

    // The daemon leaving is not the run leaving.
    let home = h.home.clone();
    h.stop().await;
    let mut said_so = false;
    while let Some(f) = joiner.next().await {
        if let Frame::Note(s) = f {
            said_so |= s.contains("daemon stopping");
        }
    }
    assert!(said_so);

    // Proven: the holder survived. This test has no further use for either
    // it or the child it was holding — they are not the daemon's to kill, so
    // reach for them the way `kill -9` would.
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let id = run.id.clone();
    let row = ledger.call_blocking(move |c| sessions::get(c, &id)).unwrap().unwrap();
    let _ = kill(Pid::from_raw(row.holder_pid as i32), Signal::SIGKILL);
    let _ = kill(Pid::from_raw(row.pid as i32), Signal::SIGKILL);
}

/// Says each size it is told of, and `end` once the test drops a file named
/// so in its folder: what came before that is everything it was told.
const SIZES: &str = r#"trap 'echo "winch $(stty size)"' WINCH; echo ready; while :; do sleep 0.02; if [ -e end ]; then rm end; echo end; fi; done"#;

/// Every resize the run heard up to `end`, in order.
async fn sizes_heard(home: &Home, id: &str, cwd: &std::path::Path) -> Vec<String> {
    let before = journal(home, id).matches("end").count();
    std::fs::write(cwd.join("end"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let text = loop {
        let text = journal(home, id);
        if text.matches("end").count() > before {
            break text;
        }
        assert!(Instant::now() < deadline, "the run never said end: {text:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("winch "))
        .map(str::to_string)
        .collect()
}

/// Past the age at which a joined run is nudged once, at that age (`YOUNG` in
/// `attach.rs`), so a test counting resizes counts only the join's own.
async fn past_young() {
    tokio::time::sleep(Duration::from_millis(3200)).await;
}

async fn resize(v: &mut Viewer, cols: u16, rows: u16) {
    write_frame(&mut v.wr, &Frame::Resize { cols, rows }).await.unwrap();
}

/// A joiner at a size the run does not have is one resize, and so one
/// repaint: an app that redraws everything on a resize (Claude Code redraws
/// its whole conversation) does it once.
#[tokio::test]
async fn a_join_at_a_new_size_is_one_repaint() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(h) = Harness::on(Home::at(tmp.path())) else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["/bin/sh", "-c", SIZES], "cwd": tmp.path().to_string_lossy() }),
        )
        .await;
    wait_journal(&h.home, &run.id, "ready").await;
    past_young().await;
    let (_, mut joiner) = Viewer::attach(&h.home, &run.id, "me@here", false).await.unwrap();
    resize(&mut joiner, 101, 33).await;
    wait_journal(&h.home, &run.id, "winch 33 101").await;
    // Long enough for any other resize to have been heard.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(sizes_heard(&h.home, &run.id, tmp.path()).await, ["33 101"]);
    let _: Run = c.ok("run.stop", json!({ "id": run.id, "force": true })).await;
    c.wait_terminal(&run.id).await;
    h.stop().await;
}

/// A run joined the moment it starts may not be listening for a resize yet —
/// Claude Code sets that up half a second in — and one sent before is lost:
/// it draws for the size it started at, the bottom of the screen empty. So a
/// run joined young is nudged once, at that age: a few seconds after it started.
#[tokio::test]
async fn a_run_joined_before_it_listens_still_hears_its_size() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(h) = Harness::on(Home::at(tmp.path())) else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    // Listens only after a while, as a Node or Bun app does.
    let late = r#"sleep 0.8; trap 'echo "winch $(stty size)"' WINCH; echo ready; while :; do sleep 0.02; done"#;
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["/bin/sh", "-c", late], "cwd": tmp.path().to_string_lossy() }),
        )
        .await;
    let (_, mut joiner) = Viewer::attach(&h.home, &run.id, "me@here", false).await.unwrap();
    resize(&mut joiner, 101, 33).await;
    wait_journal(&h.home, &run.id, "ready").await;
    wait_journal(&h.home, &run.id, "winch 33 101").await;
    let _: Run = c.ok("run.stop", json!({ "id": run.id, "force": true })).await;
    c.wait_terminal(&run.id).await;
    h.stop().await;
}

/// Joining at the size the run has and letting go again is nothing it hears:
/// what a joiner is handed is the screen, so nothing has to make it repaint.
#[tokio::test]
async fn joining_and_letting_go_at_the_runs_size_is_no_resize() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(h) = Harness::on(Home::at(tmp.path())) else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["/bin/sh", "-c", SIZES], "cwd": tmp.path().to_string_lossy() }),
        )
        .await;
    wait_journal(&h.home, &run.id, "ready").await;
    past_young().await;
    let (info, mut joiner) = Viewer::attach(&h.home, &run.id, "me@here", false).await.unwrap();
    let (cols, rows) = (
        info["cols"].as_u64().unwrap() as u16,
        info["rows"].as_u64().unwrap() as u16,
    );
    resize(&mut joiner, cols, rows).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(joiner);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(sizes_heard(&h.home, &run.id, tmp.path()).await.is_empty());
    let _: Run = c.ok("run.stop", json!({ "id": run.id, "force": true })).await;
    c.wait_terminal(&run.id).await;
    h.stop().await;
}

/// A watcher coming to a run that redrew one line a hundred times is handed
/// the line as it stands, once, not every redraw on the way.
#[tokio::test]
async fn a_viewer_sees_the_screen_not_every_redraw() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(h) = Harness::on(Home::at(tmp.path())) else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let script = r#"i=0; while [ $i -lt 100 ]; do printf 'n=%03d\r' $i; i=$((i+1)); done; echo; echo drawn; while :; do sleep 1; done"#;
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["/bin/sh", "-c", script], "cwd": tmp.path().to_string_lossy() }),
        )
        .await;
    wait_journal(&h.home, &run.id, "drawn").await;
    let (info, mut v) = Viewer::attach(&h.home, &run.id, "ann@laptop", false).await.unwrap();
    assert_eq!(info["live"], true);
    v.until("drawn").await;
    assert!(v.seen.contains("n=099"), "{}", v.seen);
    assert!(!v.seen.contains("n=098"), "{}", v.seen);
    let _: Run = c.ok("run.stop", json!({ "id": run.id, "force": true })).await;
    c.wait_terminal(&run.id).await;
    // Ended, the same: drawn from the journal, not replayed from it.
    let (info, mut late) = Viewer::attach(&h.home, &run.id, "ann@laptop", false).await.unwrap();
    assert_eq!(info["live"], false);
    late.until("drawn").await;
    assert!(late.seen.contains("n=099"), "{}", late.seen);
    assert!(!late.seen.contains("n=098"), "{}", late.seen);
    h.stop().await;
}
