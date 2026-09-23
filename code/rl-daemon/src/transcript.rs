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
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

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
const SLACK: SignedDuration = SignedDuration::from_secs(10);

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
            let t = v.get("content")?.as_str()?.trim().to_string();
            (!t.is_empty()).then_some(Recap { text: t, at })
        })
        .last()
}

/// What was said in a Claude session at or after `from`, a line a message:
/// what you typed and what Claude wrote back, without tool traffic.
pub fn claude_messages(text: &str, from: Option<Timestamp>) -> String {
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
        let content = v.pointer("/message/content");
        let said: Vec<String> = match content {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_string))
                .collect(),
            _ => vec![],
        };
        for s in said.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let _ = writeln!(out, "{at} {who}: {s}");
        }
    }
    out
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
}
