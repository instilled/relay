//! `run.start` / `run.stop` end to end: an in-process daemon spawning the real
//! `rl-session` binary, with the outcome read back through the socket and
//! the ledger.

use relay_core::Home;
use relay_ledger::{Ledger, session_io, sessions};
use relay_proto::codec::{read_message, write_message};
use relay_proto::rpc::{Id, Request, Response, RpcError};
use relay_proto::types::{Event, Run, RunState};
use rl_daemon::ServeOptions;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio_util::sync::CancellationToken;

fn session_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rld")).with_file_name("rl-session");
    p.is_file().then_some(p)
}

fn fake_cli() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../fixtures/fake-cli")
        .to_string_lossy()
        .into_owned()
}

struct Conn {
    rd: BufReader<OwnedReadHalf>,
    wr: OwnedWriteHalf,
    n: u64,
}

impl Conn {
    async fn open(home: &Home) -> Self {
        for _ in 0..100 {
            if let Ok(s) = UnixStream::connect(home.daemon_socket()).await {
                let (rd, wr) = s.into_split();
                return Self {
                    rd: BufReader::new(rd),
                    wr,
                    n: 0,
                };
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("daemon never came up");
    }
    async fn call(&mut self, method: &str, params: Value) -> Response {
        self.n += 1;
        write_message(&mut self.wr, &Request::new(Id::Num(self.n), method, Some(params)))
            .await
            .unwrap();
        read_message(&mut self.rd).await.unwrap().unwrap()
    }
    async fn ok<T: DeserializeOwned>(&mut self, method: &str, params: Value) -> T {
        let res = self.call(method, params).await;
        assert!(res.error.is_none(), "{method}: {:?}", res.error);
        serde_json::from_value(res.result.unwrap()).unwrap()
    }
    async fn wait_terminal(&mut self, id: &str) -> Run {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let run: Run = self.ok("run.get", json!({ "id": id })).await;
            if run.state.is_terminal() {
                return run;
            }
            assert!(Instant::now() < deadline, "run {id} never ended: {run:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

struct Harness {
    home: Home,
    token: CancellationToken,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
    _tmp: tempfile::TempDir,
}

async fn harness() -> Option<Harness> {
    let session_bin = session_bin()?;
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let token = CancellationToken::new();
    let opts = ServeOptions {
        session_bin: Some(session_bin),
    };
    let server = tokio::spawn(rl_daemon::serve_with(home.clone(), token.clone(), opts));
    Some(Harness {
        home,
        token,
        server,
        _tmp: tmp,
    })
}

impl Harness {
    async fn stop(self) {
        self.token.cancel();
        tokio::time::timeout(Duration::from_secs(3), self.server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn command_run_records_output_exit_and_events() {
    let Some(h) = harness().await else {
        eprintln!("skipping: rl-session not built (run `cargo test --workspace`)");
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = h.home.path().to_string_lossy().into_owned();

    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "echo hello from the run; exit 3"], "cwd": cwd, "tags": ["e2e"], "ref": "gh:1" }),
        )
        .await;
    assert_eq!(run.id.len(), 8);
    assert_eq!(run.state, RunState::Working);
    assert_eq!(run.title, "sh -c echo hello from the run; exit 3");
    assert_eq!(run.group_key(), "gh:1");
    assert!(run.workspace.is_absolute());

    let done = c.wait_terminal(&run.id).await;
    assert_eq!(done.state, RunState::Failed);
    assert_eq!(done.exit_code, Some(3));
    assert!(done.ended_at.is_some());
    assert_eq!(done.last_line.as_deref(), Some("hello from the run"));

    let evs: Vec<Event> = c.ok("event.list", json!({ "run": run.id })).await;
    let kinds: Vec<&str> = evs.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["run.started", "run.failed"]);
    assert!(evs[1].summary.starts_with("failed after"), "{}", evs[1].summary);
    assert_eq!(evs[1].data.as_ref().unwrap()["exit_code"], 3);
    assert_eq!(evs[1].group_key, "gh:1");

    let ledger = Ledger::open(&h.home.ledger()).unwrap();
    let id = run.id.clone();
    let out = ledger.call_blocking(move |db| session_io::tail(db, &id, 4096)).unwrap();
    assert!(String::from_utf8_lossy(&out).contains("hello from the run"));
    let id = run.id.clone();
    let sess = ledger.call_blocking(move |db| sessions::get(db, &id)).unwrap().unwrap();
    assert!(sess.closed_at.is_some(), "session should be closed once the run ended");
    assert!(sess.tty);

    // The holder was told to close; its socket goes away.
    for _ in 0..100 {
        if !sess.socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!sess.socket.exists(), "holder socket lingered");

    let bad = c
        .call("run.start", json!({ "argv": ["/nonexistent/nope"], "cwd": cwd }))
        .await;
    let err = bad.error.unwrap();
    assert_eq!(err.code, RpcError::INTERNAL);
    assert!(err.message.contains("rl-session failed to start"), "{}", err.message);

    h.stop().await;
}

#[tokio::test]
async fn stop_ends_in_stopped() {
    let Some(h) = harness().await else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = h.home.path().to_string_lossy().into_owned();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": [fake_cli()], "cwd": cwd, "kind": "agent" }),
        )
        .await;
    assert_eq!(run.cli.as_deref(), Some("fake-cli"));

    let listed: Vec<Run> = c.ok("run.list", json!({ "active_only": true })).await;
    assert_eq!(listed.len(), 1);

    let after: Run = c.ok("run.stop", json!({ "id": run.id })).await;
    assert!(!after.state.is_terminal(), "stop returns before the exit lands");
    let done = c.wait_terminal(&run.id).await;
    assert_eq!(done.state, RunState::Stopped);
    assert_eq!(done.exit_code, Some(143));

    let evs: Vec<Event> = c
        .ok("event.list", json!({ "run": run.id, "kinds": ["run.stopped"] }))
        .await;
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].data.as_ref().unwrap()["signal"], "SIGTERM");

    let again = c.call("run.stop", json!({ "id": run.id })).await;
    assert_eq!(again.error.unwrap().code, RpcError::NOT_FOUND);
    h.stop().await;
}

#[tokio::test]
async fn no_tty_run_captures_both_streams() {
    let Some(h) = harness().await else {
        return;
    };
    let mut c = Conn::open(&h.home).await;
    let cwd = h.home.path().to_string_lossy().into_owned();
    let run: Run = c
        .ok(
            "run.start",
            json!({ "argv": ["sh", "-c", "echo out; echo err 1>&2"], "cwd": cwd, "tty": false, "title": "both" }),
        )
        .await;
    assert_eq!(run.title, "both");
    let done = c.wait_terminal(&run.id).await;
    assert_eq!((done.state, done.exit_code), (RunState::Finished, Some(0)));
    let ledger = Ledger::open(&h.home.ledger()).unwrap();
    let id = run.id.clone();
    let out =
        String::from_utf8_lossy(&ledger.call_blocking(move |db| session_io::tail(db, &id, 4096)).unwrap()).into_owned();
    assert!(out.contains("out") && out.contains("err"), "{out:?}");
    h.stop().await;
}
