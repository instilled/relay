//! `rl mcp --harness <h>`: a stdio MCP server so agents can message each
//! other. JSON-RPC 2.0, one message per line. Three tools: `who` (the list's
//! rows), `send` (drop a file in the recipient's mail folder) and `inbox`
//! (take your own mail). A recipient whose harness can be told from outside
//! is told it has mail: `send` runs its harness's `ping_argv` (Codex's
//! `codex queue … --resume` on its thread). One of a harness's own sessions
//! that its harness can wake is woken (`Harness::wake`, Claude's: an idle
//! one has the mail's line typed into it, a stopped one is carried on with
//! it as its prompt); one working or waiting on you hears of its mail from
//! Relay's hooks (`rl hook`'s `news`), as a held Claude does. A ping or a
//! wake that fails never fails the send.
//!
//! Only protocol goes to stdout; logs go to stderr.
//!
//! An address is what the list calls a session: a Relay-held session's name;
//! a harness's own session's name, or its id when names clash; an agent
//! only its hooks told of, its session id. A title you gave a row (`^r`)
//! addresses it too. Mail is kept under what does not change while the
//! session lives: a held session's name, else its conversation id.

use crate::config::Config;
use crate::sources::{self, NativeLister, NativeRow, Row, Source};
use crate::store::mail::{self, Message};
use crate::store::sessions::{self, Held};
use crate::store::state;
use anyhow::Result;
use jiff::Timestamp;
use rl_core::Home;
use rl_detect::harness::{self, Harness, Woke};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, Write};
use std::time::Duration;

/// Newest first: what an unknown client version is answered with.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// How long a ping may take: `codex queue` queues and resumes the thread on
/// Codex's daemon.
pub const PING_WAIT: Duration = Duration::from_secs(10);

/// Finds a harness by its name: what pings one of its sessions, and what
/// advises waking one.
pub type Harnesses = Box<dyn Fn(&str) -> Option<&'static dyn Harness> + Send + Sync>;

/// Who the caller is, as far as its environment says.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    /// The harness the server is run for (`--harness`).
    pub harness: String,
    /// The held session this server is the agent of: `RL_SESSION`, where
    /// the session runs this harness (`RL_SESSION_AGENT`). An agent
    /// started inside a held shell is not that shell.
    pub relay_session: Option<String>,
    /// The conversation id the harness sets for its sessions, where it does
    /// (`Harness::caller_id_var`: Claude's `CLAUDE_CODE_SESSION_ID`), the
    /// same one its hooks report.
    pub session_id: Option<String>,
}

impl Caller {
    /// The caller of a server `harness` runs (`h`, where Relay knows it),
    /// its variables read by `var`.
    pub fn of(harness: &str, h: Option<&dyn Harness>, var: impl Fn(&str) -> Option<String>) -> Self {
        let var = |k: &str| var(k).filter(|v| !v.is_empty());
        Self {
            harness: harness.to_string(),
            relay_session: var(rl_core::SESSION_ENV).filter(|_| var(rl_core::AGENT_ENV).as_deref() == Some(harness)),
            session_id: h.and_then(|h| h.caller_id_var()).and_then(var),
        }
    }
}

/// One session that can be written to.
struct Entry {
    address: String,
    /// Your title for its row (`^r`), if you gave one.
    title: Option<String>,
    row: Row,
    /// Where its mail is kept.
    key: String,
    /// The conversation ids the harness knows it by.
    session_ids: Vec<String>,
    /// The id its harness pings it by (Codex's thread id), if one is known.
    ping_id: Option<String>,
    /// One of a harness's own sessions, as its harness listed it.
    native: Option<NativeRow>,
}

impl Entry {
    /// Every folder its mail can be in: its key, its address, its ids.
    fn keys(&self) -> Vec<String> {
        let mut keys = vec![self.key.clone(), self.address.clone()];
        keys.extend(self.session_ids.iter().cloned());
        dedup(keys)
    }
}

pub struct Server {
    home: Home,
    listers: Vec<Box<dyn NativeLister>>,
    caller: Caller,
    harnesses: Harnesses,
}

/// Runs the server for `harness` on this process's stdio until stdin closes.
pub fn run(home: &Home, harness: &str) -> Result<()> {
    let server = Server::new(
        home.clone(),
        sources::native_listers(),
        Caller::of(harness, sources::harness(harness), |k| std::env::var(k).ok()),
    )
    .with_harnesses(Box::new(sources::harness));
    server.serve(std::io::stdin().lock(), std::io::stdout().lock())?;
    Ok(())
}

impl Server {
    /// A server that pings no one until given the harnesses
    /// (`with_harnesses`).
    pub fn new(home: Home, listers: Vec<Box<dyn NativeLister>>, caller: Caller) -> Self {
        Self {
            home,
            listers,
            caller,
            harnesses: Box::new(|_| None),
        }
    }

    /// The same, pinging a recipient through its harness as `harnesses`
    /// finds it.
    pub fn with_harnesses(mut self, harnesses: Harnesses) -> Self {
        self.harnesses = harnesses;
        self
    }

    /// Answers each line of `input` on `out` until `input` ends.
    pub fn serve(&self, input: impl BufRead, mut out: impl Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle(&line) {
                writeln!(out, "{reply}")?;
                out.flush()?;
            }
        }
        Ok(())
    }

    /// The reply to one message; None for a notification.
    fn handle(&self, line: &str) -> Option<Value> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response to something we never asked, or not a request.
            return msg
                .get("id")
                .filter(|_| msg.get("result").is_none() && msg.get("error").is_none())
                .map(|id| error(id.clone(), -32600, "invalid request"));
        };
        let id = msg.get("id").cloned()?;
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => ok(id, initialize(&params)),
            "ping" => ok(id, json!({})),
            "tools/list" => ok(id, json!({ "tools": tools() })),
            "tools/call" => self.call(id, &params),
            _ => error(id, -32601, &format!("method not found: {method}")),
        })
    }

    fn call(&self, id: Value, params: &Value) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
        let result = match name {
            "who" => Ok(self.who()),
            "send" => self.send(&args),
            "inbox" => self.inbox(&args),
            _ => return error(id, -32602, &format!("unknown tool: {name}")),
        };
        ok(
            id,
            match result {
                Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
                Err(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": true }),
            },
        )
    }

    /// Every session on the list, one shot: the harnesses are asked once, each
    /// with its own timeout, and one that fails is left out.
    fn entries(&self, held: Vec<Held>) -> Vec<Entry> {
        let hooked = state::list(&self.home, Timestamp::now());
        let mut native = Vec::new();
        let mut answered = BTreeSet::new();
        for l in &self.listers {
            match l.list() {
                Ok(rows) => {
                    answered.insert(l.harness().to_string());
                    native.extend(rows);
                }
                Err(e) => eprintln!("rl mcp: {} did not list: {e:#}", l.harness()),
            }
        }
        // Each row's mail key, conversation ids, ping id and native row, by
        // row key.
        type Ids = (String, Vec<String>, Option<String>, Option<NativeRow>);
        let mut ids: HashMap<String, Ids> = HashMap::new();
        for h in &held {
            ids.insert(
                format!("r:{}", h.meta.name),
                (h.meta.name.clone(), held_ids(h), held_ping_id(h), None),
            );
        }
        for n in &native {
            ids.insert(
                format!("n:{}:{}", n.harness, n.id),
                (
                    n.session_id.clone(),
                    vec![n.session_id.clone()],
                    Some(n.id.clone()),
                    Some(n.clone()),
                ),
            );
        }
        let titles = Config::load(&self.home).map(|c| c.list.names).unwrap_or_default();
        let rows = sources::rows(&held, &hooked, &native, &answered);
        let mut taken: HashMap<&str, usize> = HashMap::new();
        for r in rows.iter().filter(|r| r.source != Source::Hooked) {
            *taken.entry(r.name.as_str()).or_default() += 1;
        }
        let last = |r: &Row| r.key.rsplit(':').next().unwrap_or_default().to_string();
        let addresses: Vec<String> = rows
            .iter()
            .map(|r| match r.source {
                Source::Held => r.name.clone(),
                Source::Hooked => last(r),
                Source::Native if taken[r.name.as_str()] == 1 && !r.name.is_empty() => r.name.clone(),
                Source::Native => last(r),
            })
            .collect();
        rows.into_iter()
            .zip(addresses)
            .map(|(row, address)| {
                // One its hooks told of: its id is its conversation's.
                let (key, session_ids, ping_id, native) = ids
                    .remove(&row.key)
                    .unwrap_or_else(|| (address.clone(), vec![address.clone()], Some(address.clone()), None));
                Entry {
                    title: titles.get(&row.key).filter(|t| !t.is_empty()).cloned(),
                    address,
                    row,
                    key,
                    session_ids,
                    ping_id,
                    native,
                }
            })
            .collect()
    }

    /// The caller's own entry, if its environment tells.
    fn me<'a>(&self, entries: &'a [Entry]) -> Option<&'a Entry> {
        if let Some(name) = &self.caller.relay_session {
            return entries
                .iter()
                .find(|e| e.row.source == Source::Held && &e.address == name);
        }
        let id = self.caller.session_id.as_ref()?;
        entries.iter().find(|e| e.session_ids.contains(id))
    }

    /// The caller's own address from what costs no harness a run: a held
    /// session's name, or the id of an agent its hooks told of.
    fn me_cheaply(&self, held: &[Held]) -> Option<String> {
        if let Some(name) = &self.caller.relay_session {
            return Some(name.clone());
        }
        let id = self.caller.session_id.as_ref()?;
        if let Some(h) = held.iter().find(|h| held_ids(h).contains(id)) {
            return Some(h.meta.name.clone());
        }
        state::list(&self.home, Timestamp::now())
            .into_iter()
            .any(|h| &h.session_id == id)
            .then(|| id.clone())
    }

    fn who(&self) -> String {
        let entries = self.entries(sessions::list(&self.home));
        if entries.is_empty() {
            return "no sessions".into();
        }
        let me = self.me(&entries).map(|e| e.row.key.as_str());
        entries
            .iter()
            .map(|e| {
                let state = if e.row.words.is_empty() {
                    "running"
                } else {
                    &e.row.words
                };
                let title = e.title.as_ref().map(|t| format!(" ({t})")).unwrap_or_default();
                let you = if me == Some(e.row.key.as_str()) { " · you" } else { "" };
                format!(
                    "{}{} · {} · {} · {}{}",
                    e.address,
                    title,
                    e.row.harness,
                    state,
                    rl_core::display_path(&e.row.cwd),
                    you
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn send(&self, args: &Value) -> Result<String, String> {
        let to = arg(args, "to").ok_or("send needs `to`: an address from who")?;
        let text = arg(args, "text").ok_or("send needs `text`")?;
        let held = sessions::list(&self.home);
        // A held session is found by its name alone; the harnesses are not
        // asked.
        let (key, address, me, to) = if let Some(h) = held.iter().find(|h| h.meta.name == to) {
            let recipient = Recipient {
                harness: h.meta.harness.clone(),
                ping_id: held_ping_id(h),
                native: None,
            };
            (to.to_string(), to.to_string(), self.me_cheaply(&held), recipient)
        } else {
            let entries = self.entries(held);
            let e = resolve(&entries, to)?;
            let me = (self.caller.relay_session.clone()).or_else(|| self.me(&entries).map(|e| e.address.clone()));
            let recipient = Recipient {
                harness: e.row.harness.clone(),
                ping_id: e.ping_id.clone(),
                native: e.native.clone().filter(|_| e.row.source == Source::Native),
            };
            (e.key.clone(), e.address.clone(), me, recipient)
        };
        let from = me
            .or_else(|| arg(args, "from").map(str::to_string))
            .ok_or("send needs `from`: your own address (who lists it); this session's address is not known")?;
        let m = Message {
            from,
            to: address.clone(),
            text: text.to_string(),
            at: Timestamp::now(),
        };
        mail::send(&self.home, &key, &m).map_err(|e| format!("could not leave the message: {e}"))?;
        let line = format!("relay mail from {}: call the relay inbox tool to read it", m.from);
        let pinged = self.ping(&to, &line).unwrap_or_default();
        let woke = self.wake(&to, &line);
        if matches!(woke, Some(Woke::Typed | Woke::Resumed)) {
            // Told now: its `Stop` hook does not tell of the same mail again.
            let _ = mail::set_told(&self.home, &key, m.at);
        }
        let woken = match &woke {
            Some(Woke::Typed) => format!(" · woke {}", to.harness),
            Some(Woke::Resumed) => format!(" · resumed {}", to.harness),
            Some(Woke::Left(why) | Woke::Failed(why)) => format!(" · {why}"),
            None => String::new(),
        };
        // Advice only, where Relay could not wake it: from a session of one
        // harness, another of its own background can be woken now, where it
        // has a way (Claude's `SendMessage`). A harness lists its background
        // sessions by the name their row has (`claude agents`).
        let advice = to
            .native
            .as_ref()
            .filter(|n| !n.name.is_empty() && matches!(woke, None | Some(Woke::Failed(_))))
            .filter(|_| self.caller.harness == to.harness)
            .and_then(|n| (self.harnesses)(&to.harness)?.wake_advice(&n.name))
            .map(|w| format!("\n{w}"))
            .unwrap_or_default();
        Ok(format!("sent to {address}{pinged}{woken}{advice}"))
    }

    /// Wakes `to`, one of a harness's own sessions, for its mail where its
    /// harness has a way (`Harness::wake`: Claude's, an idle one typed
    /// into, a stopped one carried on); None where it has none.
    fn wake(&self, to: &Recipient, line: &str) -> Option<Woke> {
        let n = to.native.as_ref()?;
        (self.harnesses)(&to.harness)?.wake(n, line, &self.home.typing_dir())
    }

    /// Tells `to` it has mail, `text` the words, where its harness can be
    /// told from outside and its id for that is known; what came of it, to
    /// add to `send`'s word. None where it was not pinged.
    fn ping(&self, to: &Recipient, text: &str) -> Option<String> {
        let id = to.ping_id.as_deref().filter(|i| !i.is_empty())?;
        let argv = (self.harnesses)(&to.harness)?.ping_argv(id, text)?;
        // Asking: the harness's own hooks let a run of Relay's go.
        Some(match harness::run(&argv, None, true, PING_WAIT) {
            Ok(_) => format!(" · pinged {}", to.harness),
            Err(e) => format!(" · {} didn't take the ping: {e:#}", to.harness),
        })
    }

    fn inbox(&self, args: &Value) -> Result<String, String> {
        let keys = match &self.caller.relay_session {
            // Held: its name, and the ids it has had; no harness is asked.
            Some(_) => own_keys(&self.home, &self.caller),
            None => {
                let entries = self.entries(sessions::list(&self.home));
                match self.me(&entries) {
                    Some(e) => e.keys(),
                    None => {
                        let me = arg(args, "as").ok_or(
                            "inbox needs `as`: your own address (who lists it); this session's address is not known",
                        )?;
                        resolve(&entries, me).map_or_else(|_| vec![me.to_string()], Entry::keys)
                    }
                }
            }
        };
        let mut messages = Vec::new();
        for key in &keys {
            let taken = mail::take(&self.home, key).map_err(|e| format!("could not read the mail: {e}"))?;
            for p in &taken.unreadable {
                eprintln!("rl mcp: left unreadable mail {}", p.display());
            }
            messages.extend(taken.messages);
        }
        if messages.is_empty() {
            return Ok("no messages".into());
        }
        messages.sort_by_key(|m| m.at);
        Ok(messages
            .iter()
            .map(|m| format!("from {} at {}:\n{}", m.from, m.at, m.text))
            .collect::<Vec<_>>()
            .join("\n\n"))
    }
}

/// The caller's own mail folders, from what costs no harness a run (a
/// hook's budget): a held session's name and the conversation ids it has
/// had; else its conversation id, or, where a held session has had that id,
/// that session's. None where neither is known. Only the sessions' files
/// are read: no holder is asked how its session stands.
pub fn own_keys(home: &Home, caller: &Caller) -> Vec<String> {
    // A held session's name, and the conversation id its hooks told of.
    let ids_of = |dir: &std::path::Path| -> Option<(String, Vec<String>)> {
        let meta = sessions::read_meta(dir).ok()?;
        let hook = state::read(&dir.join(sessions::HOOK)).ok().flatten();
        Some((meta.name, hook.map(|k| k.session_id).into_iter().collect()))
    };
    let keys = |(name, ids): (String, Vec<String>)| dedup(std::iter::once(name).chain(ids).collect());
    if let Some(name) = &caller.relay_session {
        return Some(name)
            .filter(|n| !n.contains('/'))
            .and_then(|n| ids_of(&home.session_dir(n)))
            .filter(|(n, _)| n == name)
            .map_or_else(|| vec![name.clone()], keys);
    }
    let Some(id) = &caller.session_id else {
        return Vec::new();
    };
    std::fs::read_dir(home.sessions_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| ids_of(&e.path()))
        .find(|(_, ids)| ids.contains(id))
        .map_or_else(|| vec![id.clone()], keys)
}

/// Who a message went to, as far as pinging it goes.
struct Recipient {
    harness: String,
    ping_id: Option<String>,
    /// One of a harness's background sessions, as its harness listed it:
    /// what wakes it, and the name it goes by there (`claude agents`, and
    /// so Claude's `ListAgents` and `SendMessage`).
    native: Option<NativeRow>,
}

/// The id a held session's harness pings it by: its conversation's, as its
/// hooks said it.
fn held_ping_id(h: &Held) -> Option<String> {
    h.hook.as_ref().map(|k| k.session_id.clone())
}

/// The conversation ids a held session has: its hooks'.
fn held_ids(h: &Held) -> Vec<String> {
    h.hook.iter().map(|k| k.session_id.clone()).collect()
}

/// The entry `to` names: its address, else your title for it, else one of
/// its conversation ids.
fn resolve<'a>(entries: &'a [Entry], to: &str) -> Result<&'a Entry, String> {
    if let Some(e) = entries.iter().find(|e| e.address == to) {
        return Ok(e);
    }
    let titled: Vec<&Entry> = entries.iter().filter(|e| e.title.as_deref() == Some(to)).collect();
    match titled[..] {
        [e] => return Ok(e),
        [_, _, ..] => {
            let all: Vec<String> = titled.iter().map(|e| format!("`{}`", e.address)).collect();
            return Err(format!(
                "`{to}` is the title of {}; send to one's address",
                all.join(", ")
            ));
        }
        [] => {}
    }
    if let Some(e) = entries.iter().find(|e| e.session_ids.iter().any(|i| i == to)) {
        return Ok(e);
    }
    let names = entries
        .iter()
        .flat_map(|e| std::iter::once(e.address.as_str()).chain(e.title.as_deref()));
    let close = close_matches(to, names);
    Err(if close.is_empty() {
        format!("no session is addressed `{to}`; who lists the addresses")
    } else {
        format!("no session is addressed `{to}`; did you mean {}?", close.join(", "))
    })
}

fn dedup(keys: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    keys.into_iter().filter(|k| seen.insert(k.clone())).collect()
}

fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked.filter(|v| VERSIONS.contains(v)).unwrap_or(VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "relay", "version": env!("CARGO_PKG_VERSION") },
    })
}

fn tools() -> Value {
    json!([
        {
            "name": "who",
            "description": "List the agent sessions you can message: address (and your title for it), harness, state and folder. Marks yours.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        },
        {
            "name": "send",
            "description": "Leave a message for another session. They read it when they call inbox; a Codex session is also told it has mail, an idle Claude one is woken to read it, and a busy one hears of it from its hooks.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "The recipient's address or title, as who lists it." },
                    "text": { "type": "string", "description": "The message." },
                    "from": {
                        "type": "string",
                        "description": "Your own address. Only needed where this session's address is not known (see who).",
                    },
                },
                "required": ["to", "text"],
                "additionalProperties": false,
            },
        },
        {
            "name": "inbox",
            "description": "Read and clear the messages left for you, oldest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "as": {
                        "type": "string",
                        "description": "Your own address. Only needed where this session's address is not known (see who).",
                    },
                },
                "additionalProperties": false,
            },
        },
    ])
}

/// The addresses that nearly say `to`: one holds the other, or they differ by
/// at most two edits.
fn close_matches<'a>(to: &str, addresses: impl Iterator<Item = &'a str>) -> Vec<String> {
    let to = to.to_lowercase();
    addresses
        .filter(|a| {
            let a = a.to_lowercase();
            a.contains(&to) || to.contains(&a) || distance(&a, &to) <= 2
        })
        .map(|a| format!("`{a}`"))
        .collect()
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::{NativeRow, NativeState};
    use crate::store::sessions::Meta;
    use crate::store::state::{HookState, Said};
    use rl_core::state::{RunKind, RunState};
    use std::io::Cursor;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture {
        _tmp: tempfile::TempDir,
        home: Home,
    }

    /// A home with two agents that hooks told of, `sess-aaaa` and `sess-bbbb`.
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("relay"));
        home.ensure_store().unwrap();
        for id in ["sess-aaaa", "sess-bbbb"] {
            hooked(&home, id);
        }
        Fixture { _tmp: tmp, home }
    }

    fn hooked(home: &Home, id: &str) -> HookState {
        let at = Timestamp::now();
        let h = HookState {
            agent: "claude".into(),
            session_id: id.into(),
            cwd: home.path().to_path_buf(),
            state: Said::Idle,
            detail: None,
            pid: Some(std::process::id()),
            transcript_path: None,
            started: at,
            at,
        };
        state::write(&state::path(home, "claude", id).unwrap(), &h).unwrap();
        h
    }

    /// A session Relay holds, named `name`, its conversation `conversation`
    /// as its hooks told of it.
    fn held(home: &Home, name: &str, conversation: Option<&str>) {
        held_as(home, name, "claude", conversation);
    }

    /// A session Relay holds that runs `harness`, its conversation
    /// `conversation` as its hooks told of it.
    fn held_as(home: &Home, name: &str, harness: &str, conversation: Option<&str>) {
        let dir = home.session_dir(name);
        std::fs::create_dir_all(&dir).unwrap();
        let meta = Meta {
            name: name.into(),
            harness: harness.into(),
            kind: RunKind::Agent,
            cwd: home.path().to_path_buf(),
            argv: vec!["claude".into()],
            started: Timestamp::now(),
            socket: dir.join("sock"),
            holder_pid: None,
        };
        sessions::write_meta(&dir, &meta).unwrap();
        if let Some(id) = conversation {
            let mut hook = hooked(home, id);
            std::fs::remove_file(state::path(home, "claude", id).unwrap()).unwrap();
            hook.agent = harness.into();
            state::write(&dir.join(sessions::HOOK), &hook).unwrap();
        }
    }

    fn native(id: &str, session_id: &str, name: &str) -> NativeRow {
        native_of("claude", id, session_id, name)
    }

    fn native_of(harness: &str, id: &str, session_id: &str, name: &str) -> NativeRow {
        NativeRow {
            harness: harness.into(),
            id: id.into(),
            session_id: session_id.into(),
            name: name.into(),
            cwd: "/w".into(),
            state: NativeState {
                words: "working".into(),
                state: RunState::Working,
            },
            summary: None,
            started: None,
            ended: None,
            at: None,
            pr: None,
            color: None,
        }
    }

    /// A harness's lister that says `rows`, and counts how often it is asked.
    struct Fake {
        rows: Vec<NativeRow>,
        asked: Arc<AtomicUsize>,
    }

    impl NativeLister for Fake {
        fn harness(&self) -> &str {
            self.rows.first().map_or("claude", |r| r.harness.as_str())
        }

        fn list(&self) -> anyhow::Result<Vec<NativeRow>> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            Ok(self.rows.clone())
        }
    }

    fn server(f: &Fixture, caller: Caller) -> Server {
        Server::new(f.home.clone(), Vec::new(), caller)
    }

    /// A server whose harness lists `rows`; and the count of its asks.
    fn listing(f: &Fixture, caller: Caller, rows: Vec<NativeRow>) -> (Server, Arc<AtomicUsize>) {
        let asked = Arc::new(AtomicUsize::new(0));
        let lister = Fake {
            rows,
            asked: asked.clone(),
        };
        (Server::new(f.home.clone(), vec![Box::new(lister)], caller), asked)
    }

    fn caller(session_id: Option<&str>) -> Caller {
        Caller {
            relay_session: None,
            session_id: session_id.map(Into::into),
            ..Caller::default()
        }
    }

    fn held_caller(name: &str) -> Caller {
        Caller {
            relay_session: Some(name.into()),
            ..Caller::default()
        }
    }

    /// A Claude session's caller, by its conversation id.
    fn claude_caller(session_id: &str) -> Caller {
        Caller {
            harness: "claude".into(),
            ..caller(Some(session_id))
        }
    }

    /// The harnesses, each run as a copy of `fixtures/stand-in-codex` in
    /// `dir`; and that copy's path, beside which it writes what it was run
    /// with (`.argv`, `.asking`).
    fn stand_in_harnesses(dir: &std::path::Path) -> (Harnesses, std::path::PathBuf) {
        let bin = dir.join("codex");
        std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-codex"), &bin).unwrap();
        let found = bin.clone();
        let all: Vec<&'static dyn Harness> =
            rl_detect::harness::harnesses(&rl_detect::Registry::builtin(), move |_| Some(found.clone()))
                .into_iter()
                .map(|h| &*Box::leak(h))
                .collect();
        let harnesses: Harnesses = Box::new(move |n| all.iter().copied().find(|h| h.name() == n));
        (harnesses, bin)
    }

    /// What the stand-in was run with, one word to a line; None if it was not.
    fn ran(bin: &std::path::Path) -> Option<Vec<String>> {
        let said = std::fs::read_to_string(bin.with_extension("argv")).ok()?;
        Some(said.lines().map(String::from).collect())
    }

    /// Sends the messages through `serve` and returns the replies.
    fn drive(s: &Server, messages: &[Value]) -> Vec<Value> {
        let input: String = messages.iter().map(|m| format!("{m}\n")).collect();
        let mut out = Vec::new();
        s.serve(Cursor::new(input), &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn call(id: u64, tool: &str, args: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": tool, "arguments": args } })
    }

    fn text(reply: &Value) -> &str {
        reply["result"]["content"][0]["text"].as_str().unwrap()
    }

    /// The one call's reply text, failing on an error result.
    fn ok_text(s: &Server, tool: &str, args: Value) -> String {
        let r = drive(s, &[call(1, tool, args)]);
        assert_eq!(r[0]["result"].get("isError"), None, "{r:?}");
        text(&r[0]).to_string()
    }

    /// The mail folders there are, as their names on disk.
    fn folders(home: &Home) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(home.mail_dir())
            .map(|rd| {
                rd.flatten()
                    .filter(|e| std::fs::read_dir(e.path()).is_ok_and(|mut d| d.next().is_some()))
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    #[test]
    fn initialize_echoes_a_supported_version_and_offers_tools() {
        let f = fixture();
        let s = server(&f, Caller::default());
        let replies = drive(
            &s,
            &[
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } }),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "1999-01-01" } }),
                json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }),
                json!({ "jsonrpc": "2.0", "id": 4, "method": "nope" }),
            ],
        );
        assert_eq!(replies.len(), 4, "the notification is not answered: {replies:?}");
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(replies[0]["result"]["capabilities"], json!({ "tools": {} }));
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "relay");
        assert_eq!(replies[1]["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(replies[2]["result"], json!({}));
        assert_eq!(replies[3]["error"]["code"], -32601);
        assert_eq!(replies[3]["id"], 4);
    }

    #[test]
    fn garbage_is_a_parse_error_not_a_crash() {
        let f = fixture();
        let s = server(&f, Caller::default());
        let mut out = Vec::new();
        s.serve(Cursor::new("not json\n"), &mut out).unwrap();
        let reply: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reply["error"]["code"], -32700);
    }

    #[test]
    fn tools_list_has_three_tools_with_schemas() {
        let f = fixture();
        let s = server(&f, Caller::default());
        let r = drive(&s, &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })]);
        let tools = r[0]["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["who", "send", "inbox"]);
        assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object"));
        assert_eq!(tools[1]["inputSchema"]["required"], json!(["to", "text"]));
    }

    #[test]
    fn who_lists_the_sessions_and_marks_the_caller() {
        let f = fixture();
        let s = server(&f, caller(Some("sess-aaaa")));
        let t = ok_text(&s, "who", json!({}));
        let lines: Vec<_> = t.lines().collect();
        assert_eq!(lines.len(), 2, "{t}");
        let a = lines.iter().find(|l| l.starts_with("sess-aaaa · claude")).unwrap();
        assert!(a.ends_with(" · you"), "{a}");
        assert!(lines.iter().any(|l| l.starts_with("sess-bbbb") && !l.ends_with("you")));
    }

    #[test]
    fn a_message_sent_is_a_file_and_the_inbox_returns_and_removes_it() {
        let f = fixture();
        let sender = server(&f, caller(Some("sess-aaaa")));
        ok_text(&sender, "send", json!({ "to": "sess-bbbb", "text": "hello" }));
        let dir = f.home.mail_dir().join("sess-bbbb");
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        assert!(files[0].file_name().to_string_lossy().ends_with("-sess-aaaa.json"));
        let on_disk: Value = serde_json::from_slice(&std::fs::read(files[0].path()).unwrap()).unwrap();
        assert_eq!(on_disk["from"], "sess-aaaa");
        assert_eq!(on_disk["to"], "sess-bbbb");
        assert_eq!(on_disk["text"], "hello");
        assert!(on_disk["at"].is_string());

        let receiver = server(&f, caller(Some("sess-bbbb")));
        let r = drive(&receiver, &[call(1, "inbox", json!({})), call(2, "inbox", json!({}))]);
        assert!(text(&r[0]).contains("from sess-aaaa"), "{}", text(&r[0]));
        assert!(text(&r[0]).ends_with("hello"));
        assert_eq!(text(&r[1]), "no messages");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn an_unknown_recipient_is_an_error_naming_close_addresses() {
        let f = fixture();
        let s = server(&f, caller(Some("sess-aaaa")));
        let r = drive(&s, &[call(1, "send", json!({ "to": "sess-bbb", "text": "x" }))]);
        assert_eq!(r[0]["result"]["isError"], true);
        assert!(text(&r[0]).contains("`sess-bbbb`"), "{}", text(&r[0]));
        assert!(folders(&f.home).is_empty());
    }

    #[test]
    fn where_the_caller_is_not_known_send_needs_from_and_inbox_needs_as() {
        let f = fixture();
        let s = server(&f, Caller::default());
        let r = drive(
            &s,
            &[
                call(1, "send", json!({ "to": "sess-bbbb", "text": "x" })),
                call(2, "send", json!({ "to": "sess-bbbb", "text": "x", "from": "me" })),
                call(3, "inbox", json!({})),
                call(4, "inbox", json!({ "as": "sess-bbbb" })),
            ],
        );
        assert_eq!(r[0]["result"]["isError"], true);
        assert!(text(&r[0]).contains("from"));
        assert_eq!(r[1]["result"].get("isError"), None);
        assert_eq!(r[2]["result"]["isError"], true);
        assert!(text(&r[3]).contains("from me"));
    }

    #[test]
    fn a_relay_held_session_is_known_by_its_env() {
        let f = fixture();
        let s = server(&f, held_caller("api-claude"));
        ok_text(&s, "send", json!({ "to": "sess-aaaa", "text": "hi" }));
        let got = mail::take(&f.home, "sess-aaaa").unwrap();
        assert_eq!(got.messages[0].from, "api-claude");
    }

    #[test]
    fn a_held_session_is_the_caller_only_for_the_harness_it_runs() {
        let env = |agent: &'static str| {
            move |k: &str| match k {
                "RL_SESSION" => Some("api-sh".to_string()),
                "RL_SESSION_AGENT" => Some(agent.to_string()),
                "CLAUDE_CODE_SESSION_ID" => Some("sess-aaaa".to_string()),
                _ => None,
            }
        };
        let of = |h: &str, env| Caller::of(h, sources::harness(h), env);
        // A claude run in a held shell: not the shell; its own id instead.
        let c = of("claude", env("sh"));
        assert_eq!(c.relay_session, None);
        assert_eq!(c.session_id.as_deref(), Some("sess-aaaa"));
        let c = of("claude", env("claude"));
        assert_eq!(c.relay_session.as_deref(), Some("api-sh"));
        // Claude's id says nothing of a codex it started.
        let c = of("codex", env("sh"));
        assert_eq!((c.relay_session, c.session_id), (None, None));

        let f = fixture();
        let s = server(&f, of("claude", env("sh")));
        ok_text(&s, "send", json!({ "to": "sess-bbbb", "text": "x" }));
        assert_eq!(mail::take(&f.home, "sess-bbbb").unwrap().messages[0].from, "sess-aaaa");
    }

    #[test]
    fn mail_to_a_native_session_is_kept_by_its_conversation_id_and_found_by_it() {
        let f = fixture();
        let rows = vec![native("0a1b", "conv-1", "fix login")];
        let (s, _) = listing(&f, caller(Some("sess-aaaa")), rows.clone());
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "fix login", "text": "by name" })),
            "sent to fix login"
        );
        ok_text(&s, "send", json!({ "to": "conv-1", "text": "by id" }));
        assert_eq!(folders(&f.home), [mail::component("conv-1")]);

        // Its name clashes now; its address is its id, its mail where it was.
        held(&f.home, "fix login", None);
        let (s, _) = listing(&f, caller(Some("conv-1")), rows);
        let who = ok_text(&s, "who", json!({}));
        assert!(
            who.lines()
                .any(|l| l.starts_with("0a1b · claude") && l.ends_with("you")),
            "{who}"
        );
        let got = ok_text(&s, "inbox", json!({}));
        assert!(got.contains("by name") && got.contains("by id"), "{got}");
        assert!(got.find("by name") < got.find("by id"), "oldest first: {got}");
    }

    #[test]
    fn native_names_that_clash_are_addressed_by_id() {
        let f = fixture();
        let rows = vec![native("aaa1", "conv-a", "same"), native("bbb2", "conv-b", "same")];
        let (s, _) = listing(&f, Caller::default(), rows);
        let who = ok_text(&s, "who", json!({}));
        assert!(who.lines().any(|l| l.starts_with("aaa1 · ")), "{who}");
        assert!(who.lines().any(|l| l.starts_with("bbb2 · ")), "{who}");
        let r = drive(
            &s,
            &[call(1, "send", json!({ "to": "same", "text": "x", "from": "me" }))],
        );
        assert_eq!(r[0]["result"]["isError"], true, "{r:?}");
        ok_text(&s, "send", json!({ "to": "bbb2", "text": "x", "from": "me" }));
        assert_eq!(folders(&f.home), [mail::component("conv-b")]);
    }

    #[test]
    fn a_held_session_keeps_its_mail_by_name_and_reads_every_folder_it_has() {
        let f = fixture();
        held(&f.home, "api-claude", Some("conv-h"));
        // Left under its conversation id before it was held.
        let old = Message {
            from: "x".into(),
            to: "conv-h".into(),
            text: "before".into(),
            at: Timestamp::from_second(1).unwrap(),
        };
        mail::send(&f.home, "conv-h", &old).unwrap();
        let (s, asked) = listing(&f, caller(Some("sess-aaaa")), Vec::new());
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "api-claude", "text": "now" })),
            "sent to api-claude"
        );
        assert_eq!(asked.load(Ordering::SeqCst), 0, "a held address asks no harness");
        assert!(folders(&f.home).contains(&mail::component("api-claude")));

        let (s, asked) = listing(&f, held_caller("api-claude"), Vec::new());
        let got = ok_text(&s, "inbox", json!({}));
        assert!(got.starts_with("from x") && got.ends_with("now"), "{got}");
        assert!(got.contains("from sess-aaaa"), "{got}");
        assert_eq!(asked.load(Ordering::SeqCst), 0);
        assert!(folders(&f.home).is_empty());
    }

    #[test]
    fn a_title_shows_in_who_and_addresses_the_row() {
        let f = fixture();
        let mut config = Config::load(&f.home).unwrap();
        config.list.names.insert("o:claude:sess-bbbb".into(), "reviewer".into());
        config.save(&f.home).unwrap();
        let s = server(&f, caller(Some("sess-aaaa")));
        let who = ok_text(&s, "who", json!({}));
        assert!(
            who.lines().any(|l| l.starts_with("sess-bbbb (reviewer) · claude")),
            "{who}"
        );
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "reviewer", "text": "x" })),
            "sent to sess-bbbb"
        );
        assert_eq!(folders(&f.home), ["sess-bbbb"]);
        // The harness's own address stays good: a Relay name is only a title.
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "sess-bbbb", "text": "y" })),
            "sent to sess-bbbb"
        );
    }

    #[test]
    fn mail_for_a_codex_thread_is_queued_on_it_too() {
        let f = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let (harnesses, bin) = stand_in_harnesses(tmp.path());
        let rows = vec![native_of("codex", "019a-thread", "019a-thread", "codex fix")];
        let (s, _) = listing(&f, claude_caller("sess-aaaa"), rows);
        let s = s.with_harnesses(harnesses);
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "codex fix", "text": "hello" })),
            "sent to codex fix · pinged codex"
        );
        assert_eq!(
            ran(&bin).unwrap(),
            [
                "queue",
                "--thread",
                "019a-thread",
                "--message",
                "relay mail from sess-aaaa: call the relay inbox tool to read it",
                "--resume",
            ]
        );
        let asking = std::fs::read_to_string(bin.with_extension("asking")).unwrap();
        assert!(
            asking.trim().parse::<u64>().is_ok(),
            "codex's hooks let it go: {asking:?}"
        );
        assert_eq!(mail::take(&f.home, "019a-thread").unwrap().messages[0].text, "hello");
    }

    #[test]
    fn a_held_codex_is_pinged_by_the_thread_its_hooks_told_of() {
        let f = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let (harnesses, bin) = stand_in_harnesses(tmp.path());
        held_as(&f.home, "api-codex", "codex", None);
        let s = server(&f, caller(Some("sess-aaaa"))).with_harnesses(harnesses);
        // Nothing yet says its thread: delivered, not pinged.
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "api-codex", "text": "one" })),
            "sent to api-codex"
        );
        assert_eq!(ran(&bin), None);
        let mut hook = hooked(&f.home, "t-held");
        hook.agent = "codex".into();
        state::write(&f.home.session_dir("api-codex").join(sessions::HOOK), &hook).unwrap();
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "api-codex", "text": "two" })),
            "sent to api-codex · pinged codex"
        );
        assert_eq!(ran(&bin).unwrap()[2], "t-held");
        assert_eq!(mail::take(&f.home, "api-codex").unwrap().messages.len(), 2);
    }

    #[test]
    fn a_ping_codex_refuses_still_delivers_and_says_why() {
        let f = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let (harnesses, _) = stand_in_harnesses(tmp.path());
        let rows = vec![native_of("codex", "gone", "gone", "old")];
        let (s, _) = listing(&f, caller(Some("sess-aaaa")), rows);
        let s = s.with_harnesses(harnesses);
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "old", "text": "x" })),
            "sent to old · codex didn't take the ping: `codex queue` failed: no thread named gone"
        );
        assert_eq!(mail::take(&f.home, "gone").unwrap().messages.len(), 1);
    }

    /// The harnesses, Claude's binary a stand-in in `dir`: its `attach`
    /// draws an idle Claude (`fixtures/stand-in-claude-attach`, as
    /// `variant`) and keeps what is typed in `dir/typed`; any other run is
    /// kept in `dir/argv`, a word a line.
    fn claude_harnesses(dir: &std::path::Path, variant: &str) -> Harnesses {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("claude");
        let attach = concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-claude-attach");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\n[ \"$1\" = attach ] && exec python3 '{attach}' {variant} '{d}'\nprintf '%s\\n' \"$@\" > '{d}/argv'\n",
                d = dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let all: Vec<&'static dyn Harness> =
            rl_detect::harness::harnesses(&rl_detect::Registry::builtin(), move |_| Some(bin.clone()))
                .into_iter()
                .map(|h| &*Box::leak(h))
                .collect();
        Box::new(move |n| all.iter().copied().find(|h| h.name() == n))
    }

    /// One of Claude's own, `fix login`, in `cwd`, standing as said.
    fn claude_row(cwd: &std::path::Path, words: &str, state: RunState) -> NativeRow {
        NativeRow {
            cwd: cwd.into(),
            state: NativeState {
                words: words.into(),
                state,
            },
            ..native("0a1b", "conv-1", "fix login")
        }
    }

    /// What `who` typed into the stand-in Claude, a line each.
    fn typed_into(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("typed"))
            .map(|t| t.lines().map(String::from).collect())
            .unwrap_or_default()
    }

    const WAKE_LINE: &str = "relay mail from sess-aaaa: call the relay inbox tool to read it";

    #[test]
    fn an_idle_claude_is_woken_by_the_mails_line_typed_into_it() {
        let f = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let rows = vec![claude_row(&dir, "idle", RunState::Idle)];
        let (s, _) = listing(&f, claude_caller("sess-aaaa"), rows);
        let s = s.with_harnesses(claude_harnesses(&dir, "idle"));
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "fix login", "text": "hello" })),
            "sent to fix login · woke claude",
            "no SendMessage advice: it is woken"
        );
        assert_eq!(typed_into(&dir), [WAKE_LINE]);
        assert_eq!(mail::take(&f.home, "conv-1").unwrap().messages[0].text, "hello");
    }

    #[test]
    fn a_stopped_claude_is_carried_on_with_the_mails_line_as_its_prompt() {
        let f = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let rows = vec![claude_row(&dir, "completed", RunState::Finished)];
        let (s, _) = listing(&f, caller(Some("sess-aaaa")), rows);
        let s = s.with_harnesses(claude_harnesses(&dir, "idle"));
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "fix login", "text": "hello" })),
            "sent to fix login · resumed claude"
        );
        let argv = std::fs::read_to_string(dir.join("argv")).unwrap();
        assert_eq!(argv, format!("--resume\nconv-1\n--bg\n{WAKE_LINE}\n"));
        assert!(typed_into(&dir).is_empty());
    }

    #[test]
    fn a_working_claude_is_left_to_its_hooks_and_another_claude_is_told_to_wake_it_where_relay_could_not() {
        let f = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let working = vec![claude_row(&dir, "working", RunState::Working)];
        let (s, _) = listing(&f, claude_caller("sess-aaaa"), working);
        let s = s.with_harnesses(claude_harnesses(&dir, "idle"));
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "conv-1", "text": "x" })),
            "sent to fix login · claude is busy; it will see it at the end of its turn"
        );
        assert!(typed_into(&dir).is_empty() && !dir.join("argv").exists(), "nothing run");
        // Idle, but you are attached: not typed into, and the sending Claude
        // is told how it can wake it.
        let lock = rl_detect::typing::lock_path(&f.home.typing_dir(), "claude", "0a1b");
        let you = rl_detect::typing::Attached::hold(&lock).unwrap();
        let idle = vec![claude_row(&dir, "idle", RunState::Idle)];
        let (s, _) = listing(&f, claude_caller("sess-aaaa"), idle.clone());
        let s = s.with_harnesses(claude_harnesses(&dir, "idle"));
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "conv-1", "text": "x" })),
            "sent to fix login · claude wasn't woken: you are attached to it\nfix login is a Claude session — to wake it now, also SendMessage it (\"relay mail waiting: call inbox\")."
        );
        assert!(typed_into(&dir).is_empty(), "never typed while attached");
        drop(you);
        // Not from another harness, nor to one only its hooks told of.
        let (s, _) = listing(&f, caller(Some("sess-aaaa")), idle);
        let s = s.with_harnesses(claude_harnesses(&dir, "busy"));
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "conv-1", "text": "x" })),
            "sent to fix login · claude wasn't woken: it is busy"
        );
        let (s, _) = listing(&f, claude_caller("sess-aaaa"), Vec::new());
        assert_eq!(
            ok_text(&s, "send", json!({ "to": "sess-bbbb", "text": "x" })),
            "sent to sess-bbbb"
        );
    }

    #[test]
    fn mail_relay_typed_in_is_not_told_again_at_the_stop() {
        let f = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let stop = json!({ "session_id": "conv-1", "stop_hook_active": false });
        let news = || crate::hook::news(&f.home, "claude", "Stop", None, &stop);
        // Working: left to its hooks, which tell of it once at its stop.
        let (s, _) = listing(
            &f,
            claude_caller("sess-aaaa"),
            vec![claude_row(&dir, "working", RunState::Working)],
        );
        let s = s.with_harnesses(claude_harnesses(&dir, "idle"));
        ok_text(&s, "send", json!({ "to": "fix login", "text": "one" }));
        assert!(news().is_some_and(|n| n["decision"] == "block"));
        assert_eq!(news(), None, "once");
        // Idle: typed in, so this mail is told of already.
        let (s, _) = listing(
            &f,
            claude_caller("sess-aaaa"),
            vec![claude_row(&dir, "idle", RunState::Idle)],
        );
        let s = s.with_harnesses(claude_harnesses(&dir, "idle"));
        ok_text(&s, "send", json!({ "to": "fix login", "text": "two" }));
        assert_eq!(typed_into(&dir).len(), 1);
        assert_eq!(news(), None, "the line typed told it");
    }

    #[test]
    fn a_callers_own_folders_are_its_held_sessions_or_its_conversations() {
        let f = fixture();
        held(&f.home, "api-claude", Some("conv-h"));
        let both = ["api-claude", "conv-h"];
        assert_eq!(own_keys(&f.home, &held_caller("api-claude")), both);
        assert_eq!(own_keys(&f.home, &caller(Some("conv-h"))), both, "a held one's id");
        assert_eq!(own_keys(&f.home, &caller(Some("sess-aaaa"))), ["sess-aaaa"]);
        assert_eq!(own_keys(&f.home, &held_caller("gone")), ["gone"]);
        assert!(own_keys(&f.home, &Caller::default()).is_empty());
    }

    #[test]
    fn an_unknown_tool_is_a_protocol_error() {
        let f = fixture();
        let s = server(&f, Caller::default());
        let r = drive(&s, &[call(1, "nope", json!({}))]);
        assert_eq!(r[0]["error"]["code"], -32602);
    }
}
