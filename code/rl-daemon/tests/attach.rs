//! `run.attach` over the socket: scrollback then live, several watchers and
//! one joiner, input journaled, take-over, and an ended run's replay.

mod common;

use common::{Conn, Harness, fake_cli, wait_journal};
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
    async fn attach(home: &Home, id: &str, mode: &str, client: &str, take: bool) -> Result<(Value, Self), RpcError> {
        Self::attach_from(home, id, mode, client, None, take).await
    }

    /// Attaches from one terminal of the client's.
    async fn attach_from(
        home: &Home,
        id: &str,
        mode: &str,
        client: &str,
        terminal: Option<&str>,
        take: bool,
    ) -> Result<(Value, Self), RpcError> {
        let mut c = Conn::open(home).await;
        let res = c
            .call(
                "run.attach",
                json!({ "id": id, "mode": mode, "client": client, "terminal": terminal, "take": take, "scrollback": 4096 }),
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
async fn watchers_and_one_joiner() {
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

    // A watcher: scrollback from the journal, then the holder's live stream.
    let (info, mut watcher) = Viewer::attach(&h.home, &run.id, "watch", "ann@laptop", false)
        .await
        .unwrap();
    assert_eq!(info["live"], true);
    assert_eq!(info["cols"], 100);
    assert!(info["joiner"].is_null());
    assert!(info["journaled"].as_u64().unwrap() > 0);
    assert_eq!(
        info["replay_from"], info["journaled"],
        "no gap: replay picks up where the journal ends"
    );
    let frames = watcher.until("fake-cli ready").await;
    assert!(frames.is_empty() || matches!(frames[0], Frame::ReplayEnd));

    // A watcher's keystrokes go nowhere.
    watcher.input("nope\n").await;

    // The joiner: everyone else is told.
    let (info, mut joiner) = Viewer::attach(&h.home, &run.id, "join", "bob@desk", false)
        .await
        .unwrap();
    assert_eq!(info["joiner"], "bob@desk");
    joiner.until("fake-cli ready").await;
    let frames = watcher.until("fake-cli ready").await;
    let _ = frames;
    // The post-replay repaint nudge leaves the pty exactly as big as it was.
    joiner.input("size\n").await;
    joiner.until("30 100").await;
    joiner.input("hello\n").await;
    let seen_by_joiner = joiner.until("echo: hello").await;
    let _ = seen_by_joiner;
    let seen_by_watcher = watcher.until("echo: hello").await;
    assert_eq!(notes(&seen_by_watcher), ["bob@desk joined"]);
    assert!(!watcher.seen.contains("echo: nope"), "{}", watcher.seen);

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

    // A second joiner is refused, unless it takes over.
    let err = Viewer::attach(&h.home, &run.id, "join", "cat@phone", false)
        .await
        .err()
        .unwrap();
    assert_eq!(err.code, RpcError::BUSY);
    assert!(err.message.contains("bob@desk"), "{}", err.message);
    let (info, mut taker) = Viewer::attach(&h.home, &run.id, "join", "cat@phone", true)
        .await
        .unwrap();
    assert_eq!(info["joiner"], "cat@phone");
    taker.until("echo: hello").await;
    // Bob is a watcher now: told so, and muted.
    let frames = watcher.until("echo: hello").await;
    let _ = frames;
    joiner.input("from bob\n").await;
    taker.input("from cat\n").await;
    let frames = taker.until("echo: from cat").await;
    let _ = frames;
    let frames = joiner.until("echo: from cat").await;
    assert_eq!(notes(&frames), ["cat@phone took over from bob@desk"]);
    assert!(!joiner.seen.contains("echo: from bob"), "{}", joiner.seen);

    // The run ends while everyone is attached: each viewer gets the exit.
    taker.input("exit 4\n").await;
    for v in [&mut watcher, &mut joiner, &mut taker] {
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

    // Ended: scrollback, then the exit, no holder involved.
    let (info, mut late) = Viewer::attach(&h.home, &run.id, "watch", "ann@laptop", false)
        .await
        .unwrap();
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

    let err = Viewer::attach(&h.home, "nope0000", "watch", "x@y", false)
        .await
        .err()
        .unwrap();
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

    // A watcher sees the echo, and the input is on the record as input.
    let (_info, mut w) = Viewer::attach(&h.home, &run.id, "watch", "ann@laptop", false)
        .await
        .unwrap();
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

    let (_, mut first) = Viewer::attach_from(&h.home, &run.id, "join", "me@here", Some("111"), false)
        .await
        .unwrap();
    first.until("fake-cli ready").await;

    // The same terminal again is still the same seat.
    let (info, _again) = Viewer::attach_from(&h.home, &run.id, "join", "me@here", Some("111"), false)
        .await
        .unwrap();
    assert_eq!(info["joiner"], "me@here");

    // Another terminal of mine is refused, and told which.
    let err = Viewer::attach_from(&h.home, &run.id, "join", "me@here", Some("222"), false)
        .await
        .err()
        .unwrap();
    assert_eq!(err.code, RpcError::BUSY);
    assert!(err.message.contains("me@here in another terminal"), "{}", err.message);
    assert_eq!(err.data.unwrap()["joiner"], "me@here in another terminal");

    // Taking over: the first terminal hears it, and its keys stop counting.
    let (_, mut second) = Viewer::attach_from(&h.home, &run.id, "join", "me@here", Some("222"), true)
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
    let (_, mut joiner) = Viewer::attach(&h.home, &run.id, "join", "me@here", false)
        .await
        .unwrap();
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
