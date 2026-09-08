//! Shared harness: an in-process daemon pointed at the real `rl-session`
//! binary, and a raw JSON-RPC connection to it.

#![allow(dead_code)]

use relay_core::Home;
use relay_detect::Registry;
use relay_ledger::{Ledger, session_io};
use relay_proto::codec::{read_message, write_message};
use relay_proto::rpc::{Id, Request, Response};
use relay_proto::types::{Run, RunState};
use rl_daemon::ServeOptions;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio_util::sync::CancellationToken;

/// `rl-session` next to `rld` in target/; built by `cargo test --workspace`.
pub fn session_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rld")).with_file_name("rl-session");
    p.is_file().then_some(p)
}

pub fn fake_cli() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../fixtures/fake-cli")
        .to_string_lossy()
        .into_owned()
}

pub struct Conn {
    rd: BufReader<OwnedReadHalf>,
    wr: OwnedWriteHalf,
    n: u64,
}

impl Conn {
    pub async fn open(home: &Home) -> Self {
        for _ in 0..200 {
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
    pub async fn call(&mut self, method: &str, params: Value) -> Response {
        self.n += 1;
        write_message(&mut self.wr, &Request::new(Id::Num(self.n), method, Some(params)))
            .await
            .unwrap();
        read_message(&mut self.rd).await.unwrap().unwrap()
    }
    pub async fn ok<T: DeserializeOwned>(&mut self, method: &str, params: Value) -> T {
        let res = self.call(method, params).await;
        assert!(res.error.is_none(), "{method}: {:?}", res.error);
        serde_json::from_value(res.result.unwrap()).unwrap()
    }
    pub async fn get(&mut self, id: &str) -> Run {
        self.ok("run.get", json!({ "id": id })).await
    }
    pub async fn wait_terminal(&mut self, id: &str) -> Run {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let run = self.get(id).await;
            if run.state.is_terminal() {
                return run;
            }
            assert!(Instant::now() < deadline, "run {id} never ended: {run:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    pub async fn wait_state(&mut self, id: &str, state: RunState) -> Run {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let run = self.get(id).await;
            if run.state == state {
                return run;
            }
            assert!(Instant::now() < deadline, "run {id} never reached {state}: {run:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

pub struct Harness {
    pub home: Home,
    token: CancellationToken,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Harness {
    /// Starts a daemon on `home`. `None` when `rl-session` is not built.
    pub fn on(home: Home) -> Option<Self> {
        Self::with_registry(home, None)
    }

    /// Same, with a detection registry of the test's choosing (short timers).
    pub fn with_registry(home: Home, registry: Option<Registry>) -> Option<Self> {
        let session_bin = session_bin()?;
        let token = CancellationToken::new();
        let opts = ServeOptions {
            session_bin: Some(session_bin),
            rl_bin: Some(PathBuf::from("/usr/local/bin/rl")),
            registry: registry.map(Arc::new),
        };
        let server = tokio::spawn(rl_daemon::serve_with(home.clone(), token.clone(), opts));
        Some(Self { home, token, server })
    }

    /// The built-in registry with `generic` timers short enough for tests
    /// (idle after 300ms, stale after 1s), written as a user override so the
    /// override path is exercised too.
    pub fn quick_registry(home: &Home) -> Registry {
        let dir = home.detect_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("generic.yaml"),
            "timing:\n  idle_after: 300ms\n  stale_after: 1s\n",
        )
        .unwrap();
        std::fs::write(dir.join("sh.yaml"), "timing:\n  idle_after: 300ms\n").unwrap();
        Registry::load(home).unwrap()
    }

    /// Graceful stop, as `rl daemon stop` does it: follow tasks flush and
    /// detach; runs keep going.
    pub async fn stop(self) {
        self.token.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.server)
            .await
            .expect("server stops")
            .unwrap()
            .unwrap();
    }
}

/// The journaled output of a run, as text.
pub fn journal(home: &Home, id: &str) -> String {
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let id = id.to_string();
    let bytes = ledger
        .call_blocking(move |db| session_io::tail(db, &id, 8 << 20))
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Polls the journal until `needle` shows up.
pub async fn wait_journal(home: &Home, id: &str, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = journal(home, id);
        if text.contains(needle) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "journal never contained {needle:?}: {text:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
