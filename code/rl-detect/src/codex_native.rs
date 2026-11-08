//! Codex's own sessions, 1:1 as its shared app-server daemon has them: the
//! threads it has loaded (`thread/loaded/list`), each read as it stands
//! (`thread/read`) — its name, its folder, whether it waits on you, works
//! or is ready, and when it last moved. A thread only on disk is not a
//! session; a sub-agent's thread is its parent's, not one of its own.
//!
//! The daemon speaks JSON-RPC over a WebSocket on its control socket;
//! `codex app-server proxy` (the profile's `list` args) bridges that
//! socket's bytes to stdio, so Relay speaks the WebSocket itself — the
//! handshake, and one text frame per message. With no daemon running the
//! proxy cannot connect, and Codex has no sessions of its own to list.
//!
//! Enter on one runs the profile's `sessions.attach` with the thread's id
//! (`codex resume <id>`), which opens it through the same daemon: the
//! app-server rejoins a thread that is running, so attaching to one is
//! resuming it. Codex has no background start, so what Relay starts or
//! carries on for it, Relay holds.

use crate::adapter::{Adapter, NativeRow, NativeState};
use crate::registry::Profile;
use crate::sessions::ID;
use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use rl_core::state::RunState;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// The reader a profile's `sessions.list` names for this listing.
pub const READER: &str = "codex-threads";

/// How long a list may take, all of it: the proxy, the handshake and every
/// answer.
pub const LIST_WAIT: Duration = Duration::from_secs(5);

/// What the proxy says when there is no daemon to connect to.
const NO_DAEMON: &str = "failed to connect to socket";

/// Codex's adapter: its binary and the arguments its profile gives.
#[derive(Debug, Clone)]
pub struct Codex {
    harness: String,
    bin: String,
    list: Vec<String>,
    /// The profile's `sessions.attach`: `{id}` is the thread's id.
    attach: Vec<String>,
}

impl Codex {
    /// The adapter of `profile`, run as `bin`. None where its profile says
    /// nothing of how it lists its sessions.
    pub fn of(profile: &Profile, bin: PathBuf) -> Option<Codex> {
        let s = profile.sessions.as_ref()?;
        Some(Codex {
            harness: profile.name.clone(),
            bin: bin.to_string_lossy().into_owned(),
            list: s.list.as_ref()?.args.clone(),
            attach: s.attach.clone(),
        })
    }

    fn argv<'a>(&self, args: impl IntoIterator<Item = &'a String>) -> Vec<String> {
        std::iter::once(self.bin.clone())
            .chain(args.into_iter().cloned())
            .collect()
    }
}

impl Adapter for Codex {
    fn harness(&self) -> &str {
        &self.harness
    }

    fn list_within(&self, wait: Duration) -> Result<Vec<NativeRow>> {
        let threads = ask(&self.argv(&self.list), wait.min(LIST_WAIT))?;
        Ok(rows(threads)
            .into_iter()
            .map(|mut r| {
                r.harness = self.harness.clone();
                r
            })
            .collect())
    }

    fn attach_argv(&self, id: &str) -> Vec<String> {
        let args: Vec<String> = self.attach.iter().map(|a| a.replace(ID, id)).collect();
        self.argv(&args)
    }

    fn start_argv(&self, _: Option<&str>, _: Option<&str>) -> Option<Vec<String>> {
        None
    }

    fn carry_on_argv(&self, _: &str) -> Option<Vec<String>> {
        None
    }

    /// None: the app-server's `turn/interrupt` stops a turn, not a session,
    /// and `thread/unsubscribe` only lets go of it; nothing stops a thread
    /// from outside it.
    fn stop_argv(&self, _: &str) -> Option<Vec<String>> {
        None
    }

    fn started_ids(&self, _: &str) -> Vec<String> {
        Vec::new()
    }
}

/// A thread as the app-server writes it: what of it the list shows.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// Usually its first prompt.
    #[serde(default)]
    pub preview: Option<String>,
    pub cwd: PathBuf,
    pub status: Status,
    /// Set on a sub-agent's thread: its parent's.
    #[serde(default)]
    pub parent_thread_id: Option<String>,
    /// Unix seconds.
    #[serde(default)]
    pub created_at: Option<i64>,
    /// Unix seconds: when it last moved.
    #[serde(default)]
    pub updated_at: Option<i64>,
}

/// How a thread stands at runtime, as the app-server says.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub active_flags: Vec<String>,
}

/// How Codex says a thread stands: one active and waiting on an approval or
/// an answer `needs input`, one active `working`, one loaded at rest
/// `ready`, one the app-server lost to an error `error`. A word it says
/// that is not known here is kept as said.
pub fn state(status: &Status) -> NativeState {
    let waiting = status
        .active_flags
        .iter()
        .any(|f| f == "waitingOnApproval" || f == "waitingOnUserInput");
    let (words, state) = match status.kind.as_str() {
        "active" if waiting => ("needs input", RunState::NeedsInput),
        "active" => ("working", RunState::Working),
        "idle" => ("ready", RunState::Idle),
        "systemError" => ("error", RunState::Failed),
        other => (other, RunState::Idle),
    };
    NativeState {
        words: words.to_string(),
        state,
    }
}

/// The threads as rows: Codex's sessions, not its sub-agents' threads, not
/// one only on disk, not one without a whole folder.
pub fn rows(threads: Vec<Thread>) -> Vec<NativeRow> {
    threads
        .into_iter()
        .filter(|t| {
            !t.id.is_empty() && t.parent_thread_id.is_none() && t.status.kind != "notLoaded" && t.cwd.is_absolute()
        })
        .map(row)
        .collect()
}

/// A thread, as a row: its name, else its first prompt's first line, else
/// its folder's.
pub fn row(t: Thread) -> NativeRow {
    let named = |n: &Option<String>| {
        n.as_deref()
            .and_then(|n| n.lines().map(str::trim).find(|l| !l.is_empty()))
            .map(str::to_string)
    };
    let name = named(&t.name).or_else(|| named(&t.preview)).unwrap_or_else(|| {
        t.cwd
            .file_name()
            .map_or_else(|| t.id.clone(), |f| f.to_string_lossy().into_owned())
    });
    let second = |s: Option<i64>| s.and_then(|s| Timestamp::from_second(s).ok());
    let started = second(t.created_at);
    NativeRow {
        harness: crate::user_hooks::CODEX.into(),
        session_id: t.session_id.filter(|s| !s.is_empty()).unwrap_or_else(|| t.id.clone()),
        state: state(&t.status),
        id: t.id,
        name,
        cwd: t.cwd,
        summary: None,
        started,
        ended: None,
        at: second(t.updated_at).or(started),
        pr: None,
        color: None,
    }
}

/// Runs `argv` (the proxy) and asks the daemon behind it for its loaded
/// threads, for no longer than `wait`. No daemon: none. Its hooks know the
/// run for Relay's own ask.
pub fn ask(argv: &[String], wait: Duration) -> Result<Vec<Thread>> {
    let (bin, args) = argv.split_first().ok_or_else(|| anyhow!("nothing to run"))?;
    let mut child = Command::new(bin);
    child
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("RELAY_SESSION")
        .env_remove("RELAY_SESSION_AGENT")
        .env(crate::user_hooks::ASKING, crate::user_hooks::asking_since())
        // A group of its own, so what it starts goes with it at the wait.
        .process_group(0);
    let mut child = child.spawn().context("running `codex app-server`")?;
    let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
    let mut stderr = child.stderr.take();
    let (err_tx, err_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        let _ = err_tx.send(s);
    });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let said = match (stdout, stdin) {
            (Some(r), Some(w)) => threads(r, w),
            _ => Err(anyhow!("no pipe to `codex app-server proxy`")),
        };
        let _ = tx.send(said);
    });
    let said = rx.recv_timeout(wait);
    // Done or not, the proxy goes: its input closes, and it is killed.
    crate::adapter::kill_group(&mut child);
    // Its stderr has ended with it, or something it left holds the pipe: no
    // reason to wait long.
    let err = err_rx.recv_timeout(Duration::from_millis(200)).unwrap_or_default();
    match said {
        Ok(Ok(threads)) => Ok(threads),
        Ok(Err(_)) if err.contains(NO_DAEMON) => Ok(Vec::new()),
        Ok(Err(e)) => match err.lines().find(|l| !l.trim().is_empty()) {
            Some(why) => bail!("`codex app-server proxy` failed: {why}"),
            None => Err(e.context("`codex app-server proxy`")),
        },
        Err(_) => bail!("`codex app-server proxy` did not answer in {}s", wait.as_secs()),
    }
}

/// The daemon's loaded threads, asked over the WebSocket that `r` and `w`
/// carry: `initialize`, then `thread/loaded/list`, then `thread/read` for
/// each. One gone before it is read is left out.
pub fn threads(r: impl Read, w: impl Write) -> Result<Vec<Thread>> {
    let mut rpc = Rpc {
        ws: Ws::open(r, w)?,
        next: 0,
    };
    rpc.call_all(vec![(
        "initialize",
        json!({"clientInfo": {"name": "relay", "title": "Relay", "version": env!("CARGO_PKG_VERSION")}}),
    )])?
    .remove(0)
    .map_err(|e| anyhow!("initialize: {e}"))?;
    rpc.ws.send(&json!({"jsonrpc": "2.0", "method": "initialized"}))?;
    let mut ids: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = rpc
            .call_all(vec![("thread/loaded/list", json!({"cursor": cursor}))])?
            .remove(0)
            .map_err(|e| anyhow!("thread/loaded/list: {e}"))?;
        let page: Loaded = serde_json::from_value(page).context("reading `thread/loaded/list`")?;
        ids.extend(page.data);
        match page.next_cursor {
            Some(c) if !c.is_empty() => cursor = Some(c),
            _ => break,
        }
    }
    let reads = ids
        .iter()
        .map(|id| ("thread/read", json!({"threadId": id, "includeTurns": false})))
        .collect();
    Ok(rpc
        .call_all(reads)?
        .into_iter()
        .filter_map(|said| {
            let read: Read1 = serde_json::from_value(said.ok()?).ok()?;
            Some(read.thread)
        })
        .collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Loaded {
    data: Vec<String>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct Read1 {
    thread: Thread,
}

/// JSON-RPC calls over the WebSocket, answered in any order, among
/// notifications.
struct Rpc<R: Read, W: Write> {
    ws: Ws<R, W>,
    next: u64,
}

impl<R: Read, W: Write> Rpc<R, W> {
    /// Sends every call, then reads until each is answered: its result, or
    /// the error's message, in the order called.
    fn call_all(&mut self, calls: Vec<(&str, Value)>) -> Result<Vec<Result<Value, String>>> {
        let first = self.next + 1;
        for (method, params) in calls.iter() {
            self.next += 1;
            self.ws
                .send(&json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}))?;
        }
        let mut answers: HashMap<u64, Result<Value, String>> = HashMap::new();
        while answers.len() < calls.len() {
            let m: Value = serde_json::from_str(&self.ws.recv()?).context("reading an app-server message")?;
            let Some(id) = m.get("id").and_then(Value::as_u64) else {
                continue;
            };
            if !(first..=self.next).contains(&id) || m.get("method").is_some() {
                continue;
            }
            let said = match m.get("error") {
                Some(e) => Err(e
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("no reason given")
                    .to_string()),
                None => Ok(m.get("result").cloned().unwrap_or(Value::Null)),
            };
            answers.insert(id, said);
        }
        Ok((first..=self.next).filter_map(|id| answers.remove(&id)).collect())
    }
}

/// The client end of a WebSocket (RFC 6455), as much of it as the
/// app-server needs: text messages, a ping answered, a close an error.
struct Ws<R: Read, W: Write> {
    r: BufReader<R>,
    w: W,
}

/// The largest message read; the app-server says it sends none larger
/// unfragmented (`x-codex-websocket-max-unfragmented-message-bytes`).
const MAX_MESSAGE: u64 = 16 << 20;

/// The handshake's key. Any sixteen bytes do: it proves nothing on a
/// socket only this user can open (RFC 6455's own sample).
const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

/// What every frame Relay sends is masked with; a client must mask, and
/// nothing between it and the daemon reads it.
const MASK: [u8; 4] = [0x52, 0x6c, 0x61, 0x79];

impl<R: Read, W: Write> Ws<R, W> {
    /// Asks to upgrade, and reads the answer's head to its end.
    fn open(r: R, mut w: W) -> Result<Ws<R, W>> {
        write!(
            w,
            "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )?;
        w.flush()?;
        let mut r = BufReader::new(r);
        let mut status = String::new();
        if r.read_line(&mut status)? == 0 {
            bail!("the app-server closed before it answered");
        }
        if status.split_whitespace().nth(1) != Some("101") {
            bail!("the app-server did not take the WebSocket: {}", status.trim());
        }
        loop {
            let mut line = String::new();
            if r.read_line(&mut line)? == 0 {
                bail!("the app-server closed in its answer");
            }
            if line.trim().is_empty() {
                break;
            }
        }
        Ok(Ws { r, w })
    }

    fn frame(&mut self, opcode: u8, data: &[u8]) -> Result<()> {
        let mut f = vec![0x80 | opcode];
        let n = data.len();
        match n {
            0..=125 => f.push(0x80 | n as u8),
            126..=0xffff => {
                f.push(0x80 | 126);
                f.extend((n as u16).to_be_bytes());
            }
            _ => {
                f.push(0x80 | 127);
                f.extend((n as u64).to_be_bytes());
            }
        }
        f.extend(MASK);
        f.extend(data.iter().enumerate().map(|(i, b)| b ^ MASK[i % 4]));
        self.w.write_all(&f)?;
        self.w.flush()?;
        Ok(())
    }

    fn send(&mut self, m: &Value) -> Result<()> {
        self.frame(0x1, m.to_string().as_bytes())
    }

    /// The next text message, whole.
    fn recv(&mut self) -> Result<String> {
        let mut message = Vec::new();
        loop {
            let mut head = [0u8; 2];
            self.r.read_exact(&mut head).context("the app-server closed")?;
            let (fin, opcode, masked) = (head[0] & 0x80 != 0, head[0] & 0x0f, head[1] & 0x80 != 0);
            let n = match head[1] & 0x7f {
                126 => {
                    let mut b = [0u8; 2];
                    self.r.read_exact(&mut b)?;
                    u64::from(u16::from_be_bytes(b))
                }
                127 => {
                    let mut b = [0u8; 8];
                    self.r.read_exact(&mut b)?;
                    u64::from_be_bytes(b)
                }
                n => u64::from(n),
            };
            if n + message.len() as u64 > MAX_MESSAGE {
                bail!("the app-server sent a message larger than {MAX_MESSAGE} bytes");
            }
            let mut mask = [0u8; 4];
            if masked {
                self.r.read_exact(&mut mask)?;
            }
            let mut data = vec![0u8; n as usize];
            self.r.read_exact(&mut data)?;
            if masked {
                data.iter_mut().enumerate().for_each(|(i, b)| *b ^= mask[i % 4]);
            }
            match opcode {
                0x0 | 0x1 => {
                    message.extend(data);
                    if fin {
                        return String::from_utf8(message).context("the app-server sent no text");
                    }
                }
                0x8 => bail!("the app-server closed the connection"),
                0x9 => self.frame(0xa, &data)?,
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Registry;
    use std::os::unix::net::UnixStream;
    use std::path::Path;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/codex-native")
    }

    /// What the app-server answered, recorded (`answers.json`): by method,
    /// and `thread/read` by thread id.
    fn answers() -> Value {
        serde_json::from_str(&std::fs::read_to_string(fixtures().join("answers.json")).unwrap()).unwrap()
    }

    /// A stand-in app-server: takes the upgrade, then answers each call as
    /// recorded (its id its caller's), the notifications first. Returns
    /// the methods it was called with.
    fn serve(s: UnixStream) -> std::thread::JoinHandle<Vec<String>> {
        std::thread::spawn(move || {
            let answers = answers();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut w = s;
            let mut asked = String::new();
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                asked.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            assert!(asked.contains("Upgrade: websocket"), "{asked}");
            w.write_all(b"HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\n\r\n")
                .unwrap();
            let send = |w: &mut UnixStream, m: &Value| {
                let data = m.to_string().into_bytes();
                let mut f = vec![0x81];
                match data.len() {
                    n @ 0..=125 => f.push(n as u8),
                    n => {
                        f.push(126);
                        f.extend((n as u16).to_be_bytes());
                    }
                }
                f.extend(data);
                w.write_all(&f).unwrap();
            };
            for n in answers["notifications"].as_array().unwrap() {
                send(&mut w, n);
            }
            let mut methods = Vec::new();
            let mut ws = Ws { r, w: std::io::sink() };
            while let Ok(text) = ws.recv() {
                let m: Value = serde_json::from_str(&text).unwrap();
                let method = m["method"].as_str().unwrap().to_string();
                methods.push(method.clone());
                let Some(id) = m.get("id") else { continue };
                let mut answer = match method.as_str() {
                    "thread/read" => answers["thread/read"][m["params"]["threadId"].as_str().unwrap()].clone(),
                    _ => answers[method.as_str()].clone(),
                };
                answer["id"] = id.clone();
                send(&mut w, &answer);
            }
            methods
        })
    }

    #[test]
    fn it_asks_the_daemon_for_its_loaded_threads_over_a_websocket() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let server = serve(theirs);
        let threads = threads(ours.try_clone().unwrap(), ours.try_clone().unwrap()).unwrap();
        ours.shutdown(std::net::Shutdown::Both).unwrap();
        let ids: Vec<&str> = threads.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "01a1b669-4a41-7f31-977b-e8435c8a708a",
                "01a11d20-8cab-7061-885d-f86e8e313f11",
                "01a0b3af-810e-7c22-a2e1-4f7a99ebbd3a",
                "019eb0b3-b03b-7393-915f-967456b2082a",
                "01a1b670-0000-7000-8000-000000000001",
            ],
            "one gone before it was read is left out"
        );
        let methods = server.join().unwrap();
        assert_eq!(&methods[..3], ["initialize", "initialized", "thread/loaded/list"]);
        assert!(
            methods[3..].iter().all(|m| m == "thread/read"),
            "only reads: {methods:?}"
        );
        assert_eq!(methods.len(), 3 + 6);
    }

    fn listed() -> Vec<NativeRow> {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let _server = serve(theirs);
        let t = threads(ours.try_clone().unwrap(), ours.try_clone().unwrap()).unwrap();
        ours.shutdown(std::net::Shutdown::Both).unwrap();
        rows(t)
    }

    #[test]
    fn the_rows_are_codexs_sessions_in_its_own_words() {
        let rows = listed();
        let seen: Vec<(&str, &str, &str, RunState)> = rows
            .iter()
            .map(|r| (r.id.as_str(), r.name.as_str(), r.state.words.as_str(), r.state.state))
            .collect();
        assert_eq!(
            seen,
            [
                (
                    "01a1b669-4a41-7f31-977b-e8435c8a708a",
                    "What’s worthwhile doing",
                    "needs input",
                    RunState::NeedsInput
                ),
                (
                    "01a11d20-8cab-7061-885d-f86e8e313f11",
                    "Assess this project",
                    "working",
                    RunState::Working
                ),
                (
                    "01a0b3af-810e-7c22-a2e1-4f7a99ebbd3a",
                    "what   can oyu do",
                    "ready",
                    RunState::Idle
                ),
                (
                    "019eb0b3-b03b-7393-915f-967456b2082a",
                    "Review PR 243 changes",
                    "error",
                    RunState::Failed
                ),
            ],
            "unnamed: its first prompt's first line; a sub-agent's thread is not a session"
        );
        let r = &rows[1];
        assert_eq!((r.harness.as_str(), r.cwd.as_path()), ("codex", Path::new("/w/relay")));
        assert_eq!(r.session_id, r.id, "what its hooks tell of");
        assert_eq!(r.started, Timestamp::from_second(1791490034).ok());
        assert_eq!(r.at, Timestamp::from_second(1791720371).ok(), "when it last moved");
        assert_eq!((r.ended, r.summary.as_ref(), r.pr.as_ref()), (None, None, None));
    }

    #[test]
    fn the_state_words_cover_what_codex_says() {
        let s = |kind: &str, flags: &[&str]| {
            state(&Status {
                kind: kind.into(),
                active_flags: flags.iter().map(|f| f.to_string()).collect(),
            })
        };
        assert_eq!(s("active", &["waitingOnUserInput"]).words, "needs input");
        assert_eq!(s("active", &["somethingNew"]).state, RunState::Working);
        let new = s("pondering", &[]);
        assert_eq!(
            (new.words.as_str(), new.state),
            ("pondering", RunState::Idle),
            "kept as said"
        );
        let disk = Thread {
            id: "t".into(),
            session_id: None,
            name: None,
            preview: None,
            cwd: "/w/x".into(),
            status: Status {
                kind: "notLoaded".into(),
                active_flags: Vec::new(),
            },
            parent_thread_id: None,
            created_at: None,
            updated_at: None,
        };
        let relative = Thread {
            cwd: "w/x".into(),
            status: Status {
                kind: "idle".into(),
                active_flags: Vec::new(),
            },
            ..disk.clone()
        };
        assert!(
            rows(vec![disk.clone(), relative.clone()]).is_empty(),
            "only on disk; no whole folder"
        );
        let bare = Thread {
            cwd: "/w/x".into(),
            ..relative
        };
        let r = &rows(vec![bare])[0];
        assert_eq!(
            (r.name.as_str(), r.session_id.as_str(), r.at),
            ("x", "t", None),
            "its folder's name"
        );
    }

    fn codex(bin: PathBuf) -> Codex {
        let r = Registry::builtin();
        Codex::of(r.profile("codex").unwrap(), bin).unwrap()
    }

    #[test]
    fn it_runs_what_the_profile_says() {
        let c = codex("codex".into());
        assert_eq!(c.argv(&c.list), ["codex", "app-server", "proxy"]);
        assert_eq!(
            c.attach_argv("01a11d20-8cab-7061-885d-f86e8e313f11"),
            ["codex", "resume", "01a11d20-8cab-7061-885d-f86e8e313f11"]
        );
        assert_eq!(c.start_argv(Some("x"), Some("y")), None, "Relay holds what it starts");
        assert_eq!(c.carry_on_argv("s"), None);
        assert!(c.started_ids("anything").is_empty());
        assert_eq!(c.stop_argv("t"), None, "nothing stops a thread from outside");
    }

    fn script(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("codex");
        std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[test]
    fn with_no_daemon_codex_has_no_sessions_of_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        // What the proxy says with no socket to connect to.
        let bin = script(
            tmp.path(),
            "[ \"$*\" = 'app-server proxy' ] && [ -n \"$RELAY_ASKING\" ] || exit 9\n\
             echo 'Error: failed to connect to socket at /h/.codex/app-server-control/app-server-control.sock' >&2\n\
             exit 1",
        );
        assert_eq!(codex(bin).list().unwrap(), Vec::new());
    }

    #[test]
    fn a_proxy_that_fails_or_hangs_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = script(tmp.path(), "echo 'Error: something else' >&2; exit 2");
        let e = codex(bin).list().unwrap_err();
        assert_eq!(e.to_string(), "`codex app-server proxy` failed: Error: something else");
        let bin = script(tmp.path(), "exec sleep 30");
        let at = std::time::Instant::now();
        let e = ask(&[bin.to_string_lossy().into_owned()], Duration::from_millis(300)).unwrap_err();
        assert!(e.to_string().contains("did not answer"), "{e}");
        assert!(at.elapsed() < Duration::from_secs(10), "given up at the wait");
    }

    #[test]
    fn a_hung_proxy_goes_with_what_it_started() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = tmp.path().join("pid");
        let bin = script(tmp.path(), &format!("sleep 20 &\necho $! > '{}'\nwait", pid.display()));
        let at = std::time::Instant::now();
        let e = ask(&[bin.to_string_lossy().into_owned()], Duration::from_millis(500)).unwrap_err();
        assert!(e.to_string().contains("did not answer"), "{e}");
        assert!(at.elapsed() < Duration::from_secs(3), "{:?}", at.elapsed());
        let pid: i32 = std::fs::read_to_string(&pid).unwrap().trim().parse().unwrap();
        let gone = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err()
        });
        assert!(gone, "its whole group is killed");
    }

    /// Against the real daemon: `cargo test -p rl-detect -- --ignored`.
    /// Only reads.
    #[test]
    #[ignore = "asks the real codex app-server daemon"]
    fn the_real_codex_lists_its_loaded_threads() {
        let r = Registry::builtin();
        let p = r.profile("codex").unwrap();
        let bin = crate::registry::find_bin(&p.bins).expect("codex on PATH");
        for row in Codex::of(p, bin).unwrap().list().unwrap() {
            assert!(row.cwd.is_absolute(), "{row:?}");
            assert!(!row.state.words.is_empty(), "{row:?}");
        }
    }
}
