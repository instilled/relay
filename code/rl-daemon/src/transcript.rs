//! An agent's own session file, read for a recap. Claude Code writes its
//! transcript to `~/.claude/projects/<folder>/<session>.jsonl`, and the
//! recap it shows when you come back to a session is in there, as a
//! `system` entry of subtype `away_summary`. Codex writes a rollout to
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` and keeps no recap, but every
//! turn ends in a `task_complete` carrying its last message, and the commands
//! and patches in between are there too.
//!
//! Pure: file contents in, recaps and facts out; finding a file only reads
//! directories.

use jiff::{SignedDuration, Timestamp};
use rl_proto::types::Run;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The agents whose session files Relay can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn of(run: &Run) -> Option<Agent> {
        match run.cli.as_deref() {
            Some("claude") => Some(Agent::Claude),
            Some("codex") => Some(Agent::Codex),
            _ => None,
        }
    }
}

/// How early a session file may say it started and still be this run's: the
/// agent starts a moment after its holder, but clocks and rounding differ.
pub const SLACK: SignedDuration = SignedDuration::from_secs(10);

/// Claude's folder for a working directory: the path with `/` and `.` as `-`.
pub fn claude_dir(home: &Path, cwd: &Path) -> PathBuf {
    let name: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    home.join(".claude").join("projects").join(name)
}

/// The Claude transcript a run most likely wrote: the newest in its folder's
/// directory touched since the run started. A hook names it exactly; this is
/// for a run that has not called one yet.
pub fn find_claude(home: &Path, cwd: &Path, started: Timestamp) -> Option<PathBuf> {
    let since = std::time::SystemTime::from(started - SLACK);
    std::fs::read_dir(claude_dir(home, cwd))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .filter(|(m, _)| *m >= since)
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

/// The Codex rollout a run wrote: in the dated folders around its start, the
/// earliest whose `session_meta` names its folder and starts after it did.
pub fn find_codex(home: &Path, cwd: &Path, started: Timestamp) -> Option<PathBuf> {
    let root = home.join(".codex").join("sessions");
    let mut best: Option<(Timestamp, PathBuf)> = None;
    // The folders are dated in local time; a day either side covers any zone.
    for day in [-1i64, 0, 1] {
        let date = (started + SignedDuration::from_hours(24 * day))
            .to_zoned(jiff::tz::TimeZone::UTC)
            .date();
        let dir = root
            .join(format!("{:04}", date.year()))
            .join(format!("{:02}", date.month()))
            .join(format!("{:02}", date.day()));
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for p in entries.flatten().map(|e| e.path()) {
            if p.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            let Some((meta_cwd, at)) = codex_meta(&p) else { continue };
            if meta_cwd != cwd || at < started - SLACK {
                continue;
            }
            if best.as_ref().is_none_or(|(b, _)| at < *b) {
                best = Some((at, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// A rollout's first line: the folder it ran in and when it started.
fn codex_meta(p: &Path) -> Option<(PathBuf, Timestamp)> {
    use std::io::BufRead;
    let f = std::fs::File::open(p).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(f).read_line(&mut first).ok()?;
    let v: Value = serde_json::from_str(&first).ok()?;
    if v.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    let payload = v.get("payload")?;
    let cwd = PathBuf::from(payload.get("cwd")?.as_str()?);
    let at = payload
        .get("timestamp")
        .or_else(|| v.get("timestamp"))?
        .as_str()?
        .parse()
        .ok()?;
    Some((cwd, at))
}

/// The conversation a session file holds, as its agent's CLI names it to
/// resume it: Claude's is the file's own name, Codex's the id its first line
/// records.
pub fn session_id(agent: Agent, p: &Path) -> Option<String> {
    match agent {
        Agent::Claude => Some(p.file_stem()?.to_str()?.to_string()),
        Agent::Codex => {
            use std::io::BufRead;
            let f = std::fs::File::open(p).ok()?;
            let mut first = String::new();
            std::io::BufReader::new(f).read_line(&mut first).ok()?;
            codex_id(&first)
        }
    }
}

/// The session id in a Codex rollout's first line, its `session_meta`.
fn codex_id(first: &str) -> Option<String> {
    let v: Value = serde_json::from_str(first).ok()?;
    if v.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    Some(v.get("payload")?.get("id")?.as_str()?.to_string())
}

/// The entries of a JSONL file that parse, with their time.
fn entries(text: &str) -> impl Iterator<Item = (Timestamp, Value)> + '_ {
    text.lines().filter_map(|l| {
        let v: Value = serde_json::from_str(l).ok()?;
        let at: Timestamp = v.get("timestamp")?.as_str()?.parse().ok()?;
        Some((at, v))
    })
}

fn within(at: Timestamp, from: Option<Timestamp>) -> bool {
    from.is_none_or(|f| at >= f)
}

/// Tokens a session spent: fresh input, cached input and output. Tokens only;
/// no dollars. `tokens_in` is the fresh part, so the three do not overlap.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub tokens_in: u64,
    pub tokens_cached: u64,
    pub tokens_out: u64,
}

/// The tokens spent by entries at or after `since` (all of them for `None`).
/// Claude repeats a message's usage on every entry it streams in, so each
/// message id counts once, its last usage winning; an entry with no id counts
/// on its own. Fresh is input plus cache writes, cached is cache reads. Codex's
/// are on `token_count` events, one call's worth each, its input including the
/// cached part. An entry with none counts for nothing.
pub fn usage(body: &str, since: Option<Timestamp>) -> Usage {
    let mut sum = Usage::default();
    let n = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
    // Claude's usage per message id; the key is the entry's position when it has no id.
    let mut claude: HashMap<String, Usage> = HashMap::new();
    for (i, (_, v)) in entries(body).filter(|(at, _)| within(*at, since)).enumerate() {
        if let Some(u) = v.pointer("/message/usage") {
            let key = v
                .pointer("/message/id")
                .and_then(Value::as_str)
                .map_or_else(|| format!("#{i}"), str::to_string);
            claude.insert(
                key,
                Usage {
                    tokens_in: n(u, "input_tokens") + n(u, "cache_creation_input_tokens"),
                    tokens_cached: n(u, "cache_read_input_tokens"),
                    tokens_out: n(u, "output_tokens"),
                },
            );
        } else if v.pointer("/payload/type").and_then(Value::as_str) == Some("token_count")
            && let Some(u) = v.pointer("/payload/info/last_token_usage")
        {
            let cached = n(u, "cached_input_tokens");
            sum.tokens_in += n(u, "input_tokens").saturating_sub(cached);
            sum.tokens_cached += cached;
            sum.tokens_out += n(u, "output_tokens");
        }
    }
    for u in claude.values() {
        sum.tokens_in += u.tokens_in;
        sum.tokens_cached += u.tokens_cached;
        sum.tokens_out += u.tokens_out;
    }
    sum
}

/// Seconds of activity across the entries at or after `since`: the gaps
/// between consecutive entries, leaving out any longer than `gap` as idle.
/// `None` when fewer than two entries fall in the window.
pub fn active_span(body: &str, since: Option<Timestamp>, gap: Duration) -> Option<u64> {
    let mut times: Vec<Timestamp> = entries(body)
        .map(|(at, _)| at)
        .filter(|at| within(*at, since))
        .collect();
    if times.len() < 2 {
        return None;
    }
    times.sort();
    let gap = SignedDuration::try_from(gap).unwrap_or(SignedDuration::MAX);
    let secs = times
        .windows(2)
        .map(|w| w[1].duration_since(w[0]))
        .filter(|d| *d <= gap)
        .map(|d| d.as_secs())
        .sum::<i64>();
    Some(secs as u64)
}

/// The agent's own recap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recap {
    pub text: String,
    pub at: Timestamp,
}

/// Claude's latest recap written at or after `from`. One written before it
/// does not count: it is about something else.
pub fn claude_recap(text: &str, from: Option<Timestamp>) -> Option<Recap> {
    entries(text)
        .filter(|(at, _)| within(*at, from))
        .filter(|(_, v)| {
            v.get("type").and_then(Value::as_str) == Some("system")
                && v.get("subtype").and_then(Value::as_str) == Some("away_summary")
        })
        .filter_map(|(at, v)| {
            let t = without_hint(v.get("content")?.as_str()?.trim()).to_string();
            (!t.is_empty()).then_some(Recap { text: t, at })
        })
        .last()
}

/// Claude ends its recap with a hint for its own screen — `(disable recaps in
/// /config)` — that means nothing on Relay's.
fn without_hint(text: &str) -> &str {
    let t = text.trim_end();
    if let Some(body) = t.strip_suffix(')')
        && let Some(open) = body.rfind('(')
        && body[open + 1..]
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("disable recaps")
    {
        return t[..open].trim_end();
    }
    t
}

/// Codex's last turn that said something, at or after `from`, as its recap.
pub fn codex_recap(text: &str, from: Option<Timestamp>) -> Option<Recap> {
    codex_turns(text, from)
        .into_iter()
        .rfind(|t| !t.message.is_empty())
        .map(|t| Recap {
            text: t.message,
            at: t.at,
        })
}

/// The whole lines of a session file past `offset` that mention `word`, and
/// where the next read starts: after the last whole line, so one the agent is
/// still writing is read next time. A file shorter than `offset` has been
/// written anew, and is read from its start. Line by line, so a transcript of
/// many MB is never held at once.
pub fn read_on(p: &Path, offset: u64, word: &str) -> std::io::Result<(String, u64)> {
    use std::io::{BufRead, Seek, SeekFrom};
    let mut f = std::fs::File::open(p)?;
    let mut at = if f.metadata()?.len() < offset { 0 } else { offset };
    f.seek(SeekFrom::Start(at))?;
    let mut r = std::io::BufReader::new(f);
    let (mut out, mut line) = (String::new(), Vec::new());
    loop {
        line.clear();
        let n = r.read_until(b'\n', &mut line)?;
        if n == 0 || line.last() != Some(&b'\n') {
            return Ok((out, at));
        }
        at += n as u64;
        if line.windows(word.len()).any(|w| w == word.as_bytes()) {
            out.push_str(&String::from_utf8_lossy(&line));
        }
    }
}

/// What was said in a Claude session at or after `from`, a line a message:
/// what you typed and what Claude wrote back, without tool traffic.
pub fn claude_messages(text: &str, from: Option<Timestamp>) -> String {
    claude_said(text, from, false)
}

/// What was said and done in a Claude session at or after `from`: the
/// messages, and a line for each tool it used — its name and the start of
/// what it was given — and for each that failed.
pub fn claude_story(text: &str, from: Option<Timestamp>) -> String {
    claude_said(text, from, true)
}

/// How much of a tool's input, or of its error, a line carries.
const TOOL_LINE: usize = 120;

fn claude_said(text: &str, from: Option<Timestamp>, tools: bool) -> String {
    let mut out = String::new();
    for (at, v) in entries(text).filter(|(at, _)| within(*at, from)) {
        if v.get("isMeta").and_then(Value::as_bool) == Some(true)
            || v.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
            || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let who = match v.get("type").and_then(Value::as_str) {
            Some("user") => "you",
            Some("assistant") => "claude",
            _ => continue,
        };
        let blocks = match v.pointer("/message/content") {
            Some(Value::String(s)) => vec![json_text(s)],
            Some(Value::Array(blocks)) => blocks.clone(),
            _ => vec![],
        };
        for b in &blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let s = b.get("text").and_then(Value::as_str).unwrap_or_default().trim();
                    if !s.is_empty() {
                        let _ = writeln!(out, "{at} {who}: {s}");
                    }
                }
                Some("tool_use") if tools => {
                    let name = b.get("name").and_then(Value::as_str).unwrap_or("a tool");
                    let input = b.get("input").map(Value::to_string).unwrap_or_default();
                    let _ = writeln!(out, "{at} claude used {name}: {}", cut(&input, TOOL_LINE));
                }
                Some("tool_result") if tools && b.get("is_error").and_then(Value::as_bool) == Some(true) => {
                    let said = match b.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Array(parts)) => parts
                            .iter()
                            .filter_map(|p| p.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join(" "),
                        _ => String::new(),
                    };
                    let _ = writeln!(out, "{at} the tool failed: {}", cut(&said, TOOL_LINE));
                }
                _ => {}
            }
        }
    }
    out
}

fn json_text(s: &str) -> Value {
    serde_json::json!({ "type": "text", "text": s })
}

/// The first `max` characters of `s` on one line.
fn cut(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut c: String = flat.chars().take(max).collect();
    c.push('…');
    c
}

/// One Codex turn: when it ended, what it last said, and what it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub at: Timestamp,
    pub message: String,
    pub commands: usize,
    pub patches: usize,
}

/// The Codex turns that ended at or after `from`.
pub fn codex_turns(text: &str, from: Option<Timestamp>) -> Vec<Turn> {
    let mut turns = Vec::new();
    let (mut commands, mut patches) = (0, 0);
    for (at, v) in entries(text) {
        match v.pointer("/payload/type").and_then(Value::as_str) {
            Some("exec_command_end") => commands += 1,
            Some("patch_apply_end") => patches += 1,
            Some("task_complete") => {
                let message = v
                    .pointer("/payload/last_agent_message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if within(at, from) {
                    turns.push(Turn {
                        at,
                        message,
                        commands,
                        patches,
                    });
                }
                (commands, patches) = (0, 0);
            }
            _ => {}
        }
    }
    turns
}

/// The turns as facts: one block a turn, for an assistant to read.
pub fn codex_facts(turns: &[Turn]) -> String {
    let mut out = String::new();
    for t in turns {
        let _ = writeln!(
            out,
            "{} · {} commands · {} patches\n{}\n",
            t.at, t.commands, t.patches, t.message
        );
    }
    out
}

/// The turns in a line, with no assistant: what the last one said, and how
/// much was done.
pub fn codex_line(turns: &[Turn]) -> String {
    let Some(last) = turns.last() else {
        return String::new();
    };
    let said = last.message.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let commands: usize = turns.iter().map(|t| t.commands).sum();
    let patches: usize = turns.iter().map(|t| t.patches).sum();
    let n = turns.len();
    format!(
        "{said} ({n} turn{}, {commands} command{}, {patches} patch{})",
        if n == 1 { "" } else { "s" },
        if commands == 1 { "" } else { "s" },
        if patches == 1 { "" } else { "es" },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_is_named_as_its_cli_resumes_it() {
        let claude = Path::new("/h/.claude/projects/-w-app/8af077cd-5fe3.jsonl");
        assert_eq!(session_id(Agent::Claude, claude).as_deref(), Some("8af077cd-5fe3"));
        let meta = r#"{"type":"session_meta","payload":{"id":"01a0b3af-810e","cwd":"/w/app","timestamp":"2026-09-18T08:43:38Z"}}"#;
        assert_eq!(codex_id(meta).as_deref(), Some("01a0b3af-810e"));
        assert_eq!(
            codex_id(r#"{"type":"response_item","payload":{"id":"x"}}"#),
            None,
            "only the session's own line"
        );
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    const CLAUDE: &str = r#"{"type":"user","timestamp":"2026-09-17T10:00:00Z","message":{"role":"user","content":"fix the search"}}
{"type":"assistant","timestamp":"2026-09-17T10:01:00Z","message":{"role":"assistant","content":[{"type":"text","text":"On it."},{"type":"tool_use","name":"Bash"}]}}
{"type":"user","timestamp":"2026-09-17T10:01:05Z","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}
{"type":"system","subtype":"away_summary","timestamp":"2026-09-17T10:05:00Z","content":"Search fixed; nothing committed. Next: say commit."}
{"type":"user","isMeta":true,"timestamp":"2026-09-17T10:20:00Z","message":{"role":"user","content":"<meta>"}}
not json
{"type":"assistant","timestamp":"2026-09-17T10:30:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Committed."}]}}
"#;

    #[test]
    fn claudes_recap_is_its_latest_in_the_stretch() {
        let r = claude_recap(CLAUDE, None).unwrap();
        assert_eq!(r.text, "Search fixed; nothing committed. Next: say commit.");
        assert_eq!(r.at, at("2026-09-17T10:05:00Z"));
        assert!(claude_recap(CLAUDE, Some(at("2026-09-17T10:04:00Z"))).is_some());
        assert_eq!(
            claude_recap(CLAUDE, Some(at("2026-09-17T10:10:00Z"))),
            None,
            "a recap from before the stretch does not count"
        );
    }

    #[test]
    fn claudes_hint_for_its_own_screen_is_left_off() {
        let line = |content: &str| {
            format!(
                "{{\"type\":\"system\",\"subtype\":\"away_summary\",\"timestamp\":\"2026-09-17T10:05:00Z\",\"content\":{}}}\n",
                serde_json::to_string(content).unwrap()
            )
        };
        let recap = |content: &str| claude_recap(&line(content), None).map(|r| r.text);
        let said = "Goal: a shopping list. Next: add your credentials, then run it.";
        for raw in [
            format!("{said} (disable recaps in /config)"),
            format!("{said}  ( disable recaps in /config )  \n"),
            format!("{said}\n(Disable recaps in /config)"),
        ] {
            assert_eq!(recap(&raw).as_deref(), Some(said), "{raw:?}");
        }
        // Only that hint, and only at the end.
        let kept = "Ran the tests (all green).";
        assert_eq!(recap(kept).as_deref(), Some(kept));
        let inside = "(disable recaps in /config) was shown; nothing else.";
        assert_eq!(recap(inside).as_deref(), Some(inside));
        assert_eq!(recap("(disable recaps in /config)"), None, "nothing but the hint");
    }

    #[test]
    fn a_session_file_is_read_on_from_where_the_last_read_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        let first =
            r#"{"type":"system","subtype":"away_summary","timestamp":"2026-09-17T10:05:00Z","content":"First."}"#;
        let tool = r#"{"type":"user","timestamp":"2026-09-17T10:06:00Z","message":{"content":"x"}}"#;
        let second =
            r#"{"type":"system","subtype":"away_summary","timestamp":"2026-09-17T10:09:00Z","content":"Second."}"#;
        std::fs::write(&p, format!("{first}\n{tool}\n")).unwrap();
        let (got, at) = read_on(&p, 0, "away_summary").unwrap();
        assert_eq!(got, format!("{first}\n"), "only the lines that mention it");
        assert_eq!(at, std::fs::metadata(&p).unwrap().len());
        assert_eq!(claude_recap(&got, None).unwrap().text, "First.");

        // Nothing new: nothing read.
        assert_eq!(read_on(&p, at, "away_summary").unwrap(), (String::new(), at));

        // A line still being written waits for its end.
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        std::io::Write::write_all(&mut f, &second.as_bytes()[..20]).unwrap();
        assert_eq!(read_on(&p, at, "away_summary").unwrap(), (String::new(), at));
        std::io::Write::write_all(&mut f, format!("{}\n", &second[20..]).as_bytes()).unwrap();
        let (got, next) = read_on(&p, at, "away_summary").unwrap();
        assert_eq!(claude_recap(&got, None).unwrap().text, "Second.", "only the new tail");
        assert!(!got.contains("First."));
        assert_eq!(next, std::fs::metadata(&p).unwrap().len());

        // Written anew and shorter: read from its start.
        std::fs::write(&p, format!("{first}\n")).unwrap();
        let (got, _) = read_on(&p, next, "away_summary").unwrap();
        assert_eq!(claude_recap(&got, None).unwrap().text, "First.");
    }

    #[test]
    fn codex_recap_is_its_last_turn_read_on() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rollout.jsonl");
        let (head, tail) = CODEX.split_at(CODEX.find(r#"{"timestamp":"2026-09-17T09:10:00Z""#).unwrap());
        std::fs::write(&p, head).unwrap();
        let (got, upto) = read_on(&p, 0, "task_complete").unwrap();
        let r = codex_recap(&got, None).unwrap();
        assert_eq!((r.text.as_str(), r.at), ("Tests pass.", at("2026-09-17T09:02:00Z")));
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        std::io::Write::write_all(&mut f, tail.as_bytes()).unwrap();
        let (got, _) = read_on(&p, upto, "task_complete").unwrap();
        let r = codex_recap(&got, None).unwrap();
        assert_eq!(r.text, "Fixed the parser.\nDetails follow.");
        assert_eq!(r.at, at("2026-09-17T09:13:00Z"));
        assert_eq!(
            codex_recap(CODEX, Some(at("2026-09-17T10:00:00Z"))),
            None,
            "a turn from before the run is not its own"
        );
        let silent = r#"{"timestamp":"2026-09-17T09:20:00Z","type":"event_msg","payload":{"type":"task_complete"}}"#;
        assert_eq!(
            codex_recap(&format!("{CODEX}{silent}\n"), None).unwrap().text,
            "Fixed the parser.\nDetails follow.",
            "a turn that said nothing is passed over"
        );
    }

    #[test]
    fn claudes_messages_leave_out_tools_and_meta() {
        let m = claude_messages(CLAUDE, None);
        assert_eq!(
            m,
            "2026-09-17T10:00:00Z you: fix the search\n\
             2026-09-17T10:01:00Z claude: On it.\n\
             2026-09-17T10:30:00Z claude: Committed.\n"
        );
        assert_eq!(
            claude_messages(CLAUDE, Some(at("2026-09-17T10:10:00Z"))),
            "2026-09-17T10:30:00Z claude: Committed.\n"
        );
    }

    #[test]
    fn claudes_story_has_its_tools_too() {
        let t = r#"{"type":"user","timestamp":"2026-09-17T10:00:00Z","message":{"content":"fix the search"}}
{"type":"assistant","timestamp":"2026-09-17T10:01:00Z","message":{"content":[{"type":"text","text":"On it."},{"type":"tool_use","name":"Bash","input":{"command":"cargo   test\n-p search"}}]}}
{"type":"user","timestamp":"2026-09-17T10:01:05Z","message":{"content":[{"type":"tool_result","content":"ok"}]}}
{"type":"user","timestamp":"2026-09-17T10:02:00Z","message":{"content":[{"type":"tool_result","is_error":true,"content":[{"type":"text","text":"exit 101: 2 failed"}]}]}}
"#;
        assert_eq!(
            claude_story(t, None),
            "2026-09-17T10:00:00Z you: fix the search\n\
             2026-09-17T10:01:00Z claude: On it.\n\
             2026-09-17T10:01:00Z claude used Bash: {\"command\":\"cargo test\\n-p search\"}\n\
             2026-09-17T10:02:00Z the tool failed: exit 101: 2 failed\n"
        );
        assert_eq!(claude_messages(t, None).lines().count(), 2, "no tools in the messages");
        assert_eq!(cut(&"é".repeat(130), TOOL_LINE).chars().count(), TOOL_LINE + 1);
    }

    const CODEX: &str = r#"{"timestamp":"2026-09-17T09:00:00Z","type":"session_meta","payload":{"cwd":"/w/api","timestamp":"2026-09-17T09:00:00Z"}}
{"timestamp":"2026-09-17T09:01:00Z","type":"event_msg","payload":{"type":"exec_command_end"}}
{"timestamp":"2026-09-17T09:02:00Z","type":"event_msg","payload":{"type":"task_complete","last_agent_message":"Tests pass."}}
{"timestamp":"2026-09-17T09:10:00Z","type":"event_msg","payload":{"type":"exec_command_end"}}
{"timestamp":"2026-09-17T09:11:00Z","type":"event_msg","payload":{"type":"patch_apply_end"}}
{"timestamp":"2026-09-17T09:12:00Z","type":"event_msg","payload":{"type":"exec_command_end"}}
{"timestamp":"2026-09-17T09:13:00Z","type":"event_msg","payload":{"type":"task_complete","last_agent_message":"\nFixed the parser.\nDetails follow."}}
"#;

    #[test]
    fn codex_turns_count_what_each_did() {
        let turns = codex_turns(CODEX, None);
        assert_eq!(turns.len(), 2);
        assert_eq!((turns[0].commands, turns[0].patches), (1, 0));
        assert_eq!((turns[1].commands, turns[1].patches), (2, 1));
        assert_eq!(codex_line(&turns), "Fixed the parser. (2 turns, 3 commands, 1 patch)");
        let late = codex_turns(CODEX, Some(at("2026-09-17T09:05:00Z")));
        assert_eq!(late.len(), 1);
        assert_eq!(codex_line(&late), "Fixed the parser. (1 turn, 2 commands, 1 patch)");
        assert!(codex_facts(&late).contains("2 commands · 1 patches\nFixed the parser."));
        assert!(codex_line(&[]).is_empty());
    }

    #[test]
    fn a_session_file_is_found_by_folder_and_start() {
        let home = tempfile::tempdir().unwrap();
        let started = at("2026-09-17T09:00:00Z");

        let dir = claude_dir(home.path(), Path::new("/w/my.app"));
        assert!(dir.ends_with(".claude/projects/-w-my-app"));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(find_claude(home.path(), Path::new("/w/my.app"), started), None);
        std::fs::write(dir.join("s1.jsonl"), CLAUDE).unwrap();
        assert_eq!(
            find_claude(
                home.path(),
                Path::new("/w/my.app"),
                Timestamp::now() - SignedDuration::from_secs(60)
            ),
            Some(dir.join("s1.jsonl"))
        );
        assert_eq!(
            find_claude(
                home.path(),
                Path::new("/w/my.app"),
                Timestamp::now() + SignedDuration::from_hours(1)
            ),
            None,
            "touched before the run started"
        );

        let day = home.path().join(".codex/sessions/2026/09/17");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("rollout-a.jsonl"), CODEX).unwrap();
        std::fs::write(day.join("rollout-b.jsonl"), CODEX.replace("/w/api", "/w/web")).unwrap();
        assert_eq!(
            find_codex(home.path(), Path::new("/w/api"), started),
            Some(day.join("rollout-a.jsonl"))
        );
        assert_eq!(find_codex(home.path(), Path::new("/w/other"), started), None);
        assert_eq!(
            find_codex(home.path(), Path::new("/w/api"), at("2026-09-17T10:00:00Z")),
            None,
            "a session from before the run is not its own"
        );
    }

    #[test]
    fn usage_sums_in_window() {
        let t = r#"{"type":"assistant","timestamp":"2026-09-17T09:00:00Z","message":{"usage":{"input_tokens":1,"cache_creation_input_tokens":1,"cache_read_input_tokens":1,"output_tokens":1}}}
{"type":"assistant","timestamp":"2026-09-17T10:00:00Z","message":{"usage":{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":30,"output_tokens":5}}}
{"type":"user","timestamp":"2026-09-17T10:01:00Z","message":{"content":"hi"}}
{"type":"assistant","timestamp":"2026-09-17T10:02:00Z","message":{"usage":{"input_tokens":2,"output_tokens":7}}}
"#;
        assert_eq!(
            usage(t, Some(at("2026-09-17T09:30:00Z"))),
            Usage {
                tokens_in: 62 - 30,
                tokens_cached: 30,
                tokens_out: 12
            }
        );
        assert_eq!(usage(t, None).tokens_in, 34);
        let c = r#"{"timestamp":"2026-09-17T09:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":9}}}}
{"timestamp":"2026-09-17T09:01:00Z","type":"event_msg","payload":{"type":"token_count","info":null}}
"#;
        assert_eq!(
            usage(c, None),
            Usage {
                tokens_in: 100,
                tokens_cached: 0,
                tokens_out: 9
            }
        );
    }

    #[test]
    fn usage_counts_a_repeated_claude_message_once() {
        // three entries, one message id "m1", same usage on each; one more message "m2"
        let body = [
            r#"{"timestamp":"2026-10-01T10:00:00Z","message":{"id":"m1","usage":{"input_tokens":10,"cache_creation_input_tokens":5,"cache_read_input_tokens":1000,"output_tokens":7}}}"#,
            r#"{"timestamp":"2026-10-01T10:00:01Z","message":{"id":"m1","usage":{"input_tokens":10,"cache_creation_input_tokens":5,"cache_read_input_tokens":1000,"output_tokens":7}}}"#,
            r#"{"timestamp":"2026-10-01T10:00:02Z","message":{"id":"m1","usage":{"input_tokens":10,"cache_creation_input_tokens":5,"cache_read_input_tokens":1000,"output_tokens":9}}}"#,
            r#"{"timestamp":"2026-10-01T10:01:00Z","message":{"id":"m2","usage":{"input_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":1015,"output_tokens":2}}}"#,
        ]
        .join("\n");
        let u = usage(&body, None);
        assert_eq!(
            (u.tokens_in, u.tokens_cached, u.tokens_out),
            (15 + 3, 1000 + 1015, 9 + 2)
        );
    }

    #[test]
    fn codex_cached_input_is_not_fresh() {
        let body = r#"{"timestamp":"2026-10-01T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":9841,"cached_input_tokens":4736,"output_tokens":71}}}}"#;
        let u = usage(body, None);
        assert_eq!((u.tokens_in, u.tokens_cached, u.tokens_out), (9841 - 4736, 4736, 71));
    }

    #[test]
    fn active_span_collapses_idle_gaps() {
        let t = r#"{"timestamp":"2026-09-17T10:00:00Z"}
{"timestamp":"2026-09-17T10:00:10Z"}
{"timestamp":"2026-09-17T12:00:10Z"}
{"timestamp":"2026-09-17T12:00:15Z"}
"#;
        let gap = Duration::from_secs(120);
        assert_eq!(active_span(t, None, gap), Some(15));
        assert_eq!(active_span(t, Some(at("2026-09-17T12:00:00Z")), gap), Some(5));
    }

    #[test]
    fn active_span_needs_two_points() {
        let t = "{\"timestamp\":\"2026-09-17T10:00:00Z\"}\n";
        assert_eq!(active_span(t, None, Duration::from_secs(120)), None);
    }
}
