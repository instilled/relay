//! `run.*`, `event.*` and `presence.*` over the socket, with rows written
//! through a second ledger handle on the same file — the shape a CLI test or
//! a hook process has.

use jiff::Timestamp;
use rl_core::Home;
use rl_ledger::events::NewEvent;
use rl_ledger::{Ledger, events, runs};
use rl_proto::codec::{read_message, write_message};
use rl_proto::rpc::{Id, Request, Response, RpcError};
use rl_proto::types::{Event, Run, RunKind, event_kind};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio_util::sync::CancellationToken;

struct Conn {
    rd: BufReader<OwnedReadHalf>,
    wr: OwnedWriteHalf,
    n: u64,
}

impl Conn {
    async fn open(home: &Home) -> Self {
        for _ in 0..50 {
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
}

#[tokio::test]
async fn list_get_events_presence() {
    let tmp = tempfile::tempdir().unwrap();
    let home = Home::at(tmp.path());
    let token = CancellationToken::new();
    let server = tokio::spawn(rl_daemon::serve(home.clone(), token.clone()));
    let mut c = Conn::open(&home).await;

    let empty: Vec<Run> = c.ok("run.list", json!({})).await;
    assert!(empty.is_empty());

    // Write through a second handle, as an external process would.
    let side = Ledger::open(&home.ledger()).unwrap();
    let now: Timestamp = "2026-09-08T10:00:00Z".parse().unwrap();
    let mut run = Run::new(
        "k7x2m9qa",
        RunKind::Agent,
        "fix the token refresh",
        vec!["claude".into()],
        "/w/repo",
        "/w/repo",
        now,
    );
    run.branch = Some("fl-123-auth-fix".into());
    let ev = NewEvent::for_run(&run, now, event_kind::RUN_STARTED, "started claude");
    side.call_blocking(move |db| {
        runs::insert(db, &run)?;
        events::append(db, &ev)?;
        Ok(())
    })
    .unwrap();

    let listed: Vec<Run> = c.ok("run.list", json!({"active_only": true})).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "k7x2m9qa");
    assert_eq!(listed[0].group_key(), "fl-123-auth-fix");

    let got: Run = c.ok("run.get", json!({"id": "k7x2m9qa"})).await;
    assert_eq!(got.title, "fix the token refresh");
    let missing = c.call("run.get", json!({"id": "zzzzzzzz"})).await;
    assert_eq!(missing.error.unwrap().code, RpcError::NOT_FOUND);
    let bad = c.call("run.get", json!({"nope": 1})).await;
    assert_eq!(bad.error.unwrap().code, RpcError::INVALID_PARAMS);

    let evs: Vec<Event> = c.ok("event.list", json!({"group": "fl-123-auth-fix"})).await;
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].kind, "run.started");
    let evs: Vec<Event> = c.ok("event.list", json!({"since": "2026-09-08T10:00:01Z"})).await;
    assert!(evs.is_empty());

    let p: Value = c.ok("presence.touch", json!({"client": "tui"})).await;
    assert_eq!(p["client"], "tui");
    assert!(p.get("last_seen_at").is_none(), "first touch has no previous: {p}");
    let p: Value = c.ok("presence.touch", json!({"client": "tui"})).await;
    assert!(p["last_seen_at"].is_string(), "second touch returns previous: {p}");

    token.cancel();
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
