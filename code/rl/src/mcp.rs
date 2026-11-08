//! `rl mcp`: a stdio MCP server so agents can message each other. JSON-RPC
//! 2.0, one message per line. Three tools: `who` (the list's rows),
//! `send` (drop a file in the recipient's mail folder) and `inbox` (take
//! your own mail). Pull only; nothing here types into an agent.
//!
//! Only protocol goes to stdout; logs go to stderr.
//!
//! An address is what the list calls a session: a Relay-held session's name;
//! a harness's own session's name, or its id when names clash; an agent
//! only its hooks told of, its session id.

use crate::sources::{self, NativeLister, Row, Source};
use crate::store::mail::{self, Message};
use crate::store::{sessions, state};
use anyhow::Result;
use jiff::Timestamp;
use rl_core::Home;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, Write};

/// Newest first: what an unknown client version is answered with.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Who the caller is, as far as its environment says.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    /// `RELAY_SESSION`: set on sessions Relay holds; the session's name.
    pub relay_session: Option<String>,
    /// `CLAUDE_CODE_SESSION_ID`: Claude Code's conversation id, the same one
    /// its hooks report.
    pub session_id: Option<String>,
}

impl Caller {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            relay_session: var("RELAY_SESSION"),
            session_id: var("CLAUDE_CODE_SESSION_ID"),
        }
    }
}

/// One session that can be written to.
struct Entry {
    address: String,
    row: Row,
    /// The conversation ids the harness knows it by.
    session_ids: Vec<String>,
}

pub struct Server {
    home: Home,
    listers: Vec<Box<dyn NativeLister>>,
    caller: Caller,
}

/// Runs the server on this process's stdio until stdin closes.
pub fn run(home: &Home) -> Result<()> {
    let server = Server {
        home: home.clone(),
        listers: sources::native_listers(),
        caller: Caller::from_env(),
    };
    server.serve(std::io::stdin().lock(), std::io::stdout().lock())?;
    Ok(())
}

impl Server {
    pub fn new(home: Home, listers: Vec<Box<dyn NativeLister>>, caller: Caller) -> Self {
        Self { home, listers, caller }
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

    /// Every session on the list, one shot: the adapters are asked once, each
    /// with its own timeout, and one that fails is left out.
    fn entries(&self) -> Vec<Entry> {
        let held = sessions::list(&self.home);
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
        let mut ids: HashMap<String, Vec<String>> = HashMap::new();
        for h in &held {
            let held_ids = ids.entry(format!("r:{}", h.meta.name)).or_default();
            held_ids.extend(h.hook.iter().map(|k| k.session_id.clone()));
            held_ids.extend(h.meta.native_id.clone());
        }
        for n in &native {
            ids.insert(format!("n:{}:{}", n.harness, n.id), vec![n.session_id.clone()]);
        }
        let rows = sources::rows(&held, &hooked, &native, &answered);
        let mut taken: HashMap<&str, usize> = HashMap::new();
        for r in rows.iter().filter(|r| r.source != Source::Hooked) {
            *taken.entry(r.name.as_str()).or_default() += 1;
        }
        let addresses: Vec<String> = rows
            .iter()
            .map(|r| match r.source {
                Source::Held => r.name.clone(),
                Source::Hooked => r.key.rsplit(':').next().unwrap_or_default().to_string(),
                Source::Native if taken[r.name.as_str()] == 1 && !r.name.is_empty() => r.name.clone(),
                Source::Native => r.key.rsplit(':').next().unwrap_or_default().to_string(),
            })
            .collect();
        rows.into_iter()
            .zip(addresses)
            .map(|(row, address)| {
                let mut session_ids = ids.remove(&row.key).unwrap_or_default();
                if row.source == Source::Hooked {
                    session_ids.push(address.clone());
                }
                Entry {
                    address,
                    row,
                    session_ids,
                }
            })
            .collect()
    }

    /// The caller's own address, if its environment tells.
    fn me(&self, entries: &[Entry]) -> Option<String> {
        if let Some(name) = &self.caller.relay_session {
            return Some(name.clone());
        }
        let id = self.caller.session_id.as_ref()?;
        entries
            .iter()
            .find(|e| e.session_ids.contains(id))
            .map(|e| e.address.clone())
    }

    fn who(&self) -> String {
        let entries = self.entries();
        if entries.is_empty() {
            return "no sessions".into();
        }
        let me = self.me(&entries);
        entries
            .iter()
            .map(|e| {
                let state = if e.row.words.is_empty() {
                    "running"
                } else {
                    &e.row.words
                };
                let you = if me.as_deref() == Some(e.address.as_str()) {
                    " · you"
                } else {
                    ""
                };
                format!(
                    "{} · {} · {} · {}{}",
                    e.address,
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
        let entries = self.entries();
        if !entries.iter().any(|e| e.address == to) {
            let close = close_matches(to, entries.iter().map(|e| e.address.as_str()));
            return Err(if close.is_empty() {
                format!("no session is addressed `{to}`; who lists the addresses")
            } else {
                format!("no session is addressed `{to}`; did you mean {}?", close.join(", "))
            });
        }
        let from = self
            .me(&entries)
            .or_else(|| arg(args, "from").map(str::to_string))
            .ok_or("send needs `from`: your own address (who lists it); this session's address is not known")?;
        let m = Message {
            from,
            to: to.to_string(),
            text: text.to_string(),
            at: Timestamp::now(),
        };
        mail::send(&self.home, &m).map_err(|e| format!("could not leave the message: {e}"))?;
        Ok(format!("sent to {to}"))
    }

    fn inbox(&self, args: &Value) -> Result<String, String> {
        let me = match self.caller.relay_session.clone() {
            Some(me) => me,
            None => match self.me(&self.entries()).or_else(|| arg(args, "as").map(str::to_string)) {
                Some(me) => me,
                None => {
                    return Err(
                        "inbox needs `as`: your own address (who lists it); this session's address is not known".into(),
                    );
                }
            },
        };
        let taken = mail::take(&self.home, &me).map_err(|e| format!("could not read the mail: {e}"))?;
        for p in &taken.unreadable {
            eprintln!("rl mcp: left unreadable mail {}", p.display());
        }
        if taken.messages.is_empty() {
            return Ok("no messages".into());
        }
        Ok(taken
            .messages
            .iter()
            .map(|m| format!("from {} at {}:\n{}", m.from, m.at, m.text))
            .collect::<Vec<_>>()
            .join("\n\n"))
    }
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
            "description": "List the agent sessions you can message: address, harness, state and folder. Marks yours.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        },
        {
            "name": "send",
            "description": "Leave a message for another session. They read it when they call inbox.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "The recipient's address, as who lists it." },
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
    use crate::store::state::{HookState, Said};
    use std::io::Cursor;

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
            let at = Timestamp::now();
            let h = HookState {
                agent: "claude".into(),
                session_id: id.into(),
                cwd: tmp.path().to_path_buf(),
                state: Said::Idle,
                detail: None,
                pid: Some(std::process::id()),
                transcript_path: None,
                started: at,
                at,
            };
            state::write(&state::path(&home, "claude", id).unwrap(), &h).unwrap();
        }
        Fixture { _tmp: tmp, home }
    }

    fn server(f: &Fixture, caller: Caller) -> Server {
        Server::new(f.home.clone(), Vec::new(), caller)
    }

    fn caller(session_id: Option<&str>) -> Caller {
        Caller {
            relay_session: None,
            session_id: session_id.map(Into::into),
        }
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
        let r = drive(&s, &[call(1, "who", json!({}))]);
        let t = text(&r[0]);
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
        let r = drive(
            &sender,
            &[call(1, "send", json!({ "to": "sess-bbbb", "text": "hello" }))],
        );
        assert_eq!(r[0]["result"].get("isError"), None, "{r:?}");
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
        assert!(!f.home.mail_dir().join("sess-bbb").exists());
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
        let s = server(
            &f,
            Caller {
                relay_session: Some("api-claude".into()),
                session_id: None,
            },
        );
        drive(&s, &[call(1, "send", json!({ "to": "sess-aaaa", "text": "hi" }))]);
        let got = mail::take(&f.home, "sess-aaaa").unwrap();
        assert_eq!(got.messages[0].from, "api-claude");
    }

    #[test]
    fn an_unknown_tool_is_a_protocol_error() {
        let f = fixture();
        let s = server(&f, Caller::default());
        let r = drive(&s, &[call(1, "nope", json!({}))]);
        assert_eq!(r[0]["error"]["code"], -32602);
    }
}
