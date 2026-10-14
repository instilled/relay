//! Shared harness: an in-process daemon pointed at the real `rl-session`
//! binary, and a raw JSON-RPC connection to it.

#![allow(dead_code)]

use rl_core::Home;
use rl_daemon::ServeOptions;
use rl_detect::Registry;
use rl_ledger::{Ledger, session_io};
use rl_proto::codec::{read_message, write_message};
use rl_proto::rpc::{Id, Request, Response};
use rl_proto::types::{Run, RunState};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
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

/// `rl` next to `rld` in target/; built by `cargo build --workspace`.
pub fn rl_bin() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_rld")).with_file_name("rl");
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
        serde_json::from_value(res.result.unwrap_or(Value::Null)).unwrap()
    }
    /// The raw halves, once `run.attach` has upgraded the connection.
    pub fn into_parts(self) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
        (self.rd, self.wr)
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
        Self::start(home, registry, None)
    }

    /// Same, asking `claude agents --json` as `polling` says, of the fake
    /// `claude` at `claude`, its user-level hooks in `settings` (both the
    /// claude profile's, overridden in `home`). Every other harness never
    /// asks.
    pub fn with_claude(
        home: Home,
        claude: &Path,
        settings: &Path,
        polling: rl_daemon::outside::Polling,
    ) -> Option<Self> {
        let dir = home.detect_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("claude.yaml"),
            format!(
                "bins: [\"{}\"]\nsessions:\n  hooks:\n    file: [\"{}\"]\n",
                claude.display(),
                settings.display()
            ),
        )
        .unwrap();
        let registry = Registry::load(&home).unwrap();
        Self::start(home, Some(registry), Some(polling))
    }

    fn start(
        home: Home,
        registry: Option<Registry>,
        claude_sessions: Option<rl_daemon::outside::Polling>,
    ) -> Option<Self> {
        let session_bin = session_bin()?;
        let token = CancellationToken::new();
        let opts = ServeOptions {
            session_bin: Some(session_bin),
            rl_bin: Some(PathBuf::from("/usr/local/bin/rl")),
            step_rl: rl_bin(),
            registry: registry.map(Arc::new),
            claude_sessions,
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

/// The machine goes: run `id`'s holder, and its child with it if `child`, with no exit on
/// record unless the holder wrote one.
#[cfg(unix)]
pub async fn kill_holder(home: &Home, id: &str, child: bool) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use rl_ledger::sessions;
    let ledger = Ledger::open(&home.ledger()).unwrap();
    let id = id.to_string();
    let row = ledger.call_blocking(move |db| sessions::get(db, &id)).unwrap().unwrap();
    kill(Pid::from_raw(row.holder_pid as i32), Signal::SIGKILL).unwrap();
    for _ in 0..100 {
        if kill(Pid::from_raw(row.holder_pid as i32), None).is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if child {
        let _ = kill(Pid::from_raw(row.pid as i32), Signal::SIGKILL);
    }
}

/// Says yes to flow `name` as `flow.list` shows it now.
pub async fn approve(c: &mut Conn, name: &str) -> rl_proto::flow::Flow {
    let flows: Vec<rl_proto::flow::Flow> = c.ok("flow.list", json!({})).await;
    let hash = flows
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.hash.clone())
        .unwrap_or_default();
    c.ok("flow.approve", json!({ "name": name, "hash": hash })).await
}
