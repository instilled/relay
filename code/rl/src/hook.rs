//! `rl hook <agent> <Event>`: what an agent's hook calls, with the hook's
//! JSON on stdin. It writes a file and is gone; there is nothing to post to.
//!
//! What each agent's events say is in its profile (`sessions.hooks`), read
//! from the built-in profiles and yours in `$RELAY_HOME/detect`: a hook of
//! an agent with none there tells of no session.
//!
//! Where the word goes. Under a session Relay holds (`RL_SESSION` set) and
//! the call is of the agent that session runs (`RL_SESSION_AGENT`) it is
//! that session's own: `sessions/<name>/hook.json`. Otherwise (an agent run
//! inside a held shell, say) it is one of the agent's own sessions:
//! `state/<agent>/<session_id>.json`. `ended` removes the file.
//!
//! Claude's user-level hook says `--user` after the event (its profile's
//! `marker`): a Claude session of Relay's calls both it and the session's own
//! hook (`rl hook claude <Event>`, from the settings Relay passes it). Under
//! a held session only the session's own is heard, and the `--user` copy is
//! let go, so one call counts once. Outside Relay only the `--user` copy
//! tells, and the session's own says nothing.
//!
//! A hook called from a `claude` that Relay itself runs (`claude agents`,
//! `user_hooks::ASKING` in its environment, set under 30s ago) does nothing
//! at all.
//!
//! The agent's process is looked for (`ps`) where the file knows none, for an
//! event that is worth it (`UserHooks::asks_pid`).
//!
//! A hook must never slow down or break the agent that called it: all of
//! it, reading stdin and finding the agent's process and writing, is done
//! within `WITHIN`, or given up on; nothing is printed (some agents feed a
//! hook's stdout back to the model, and Gemini parses it); and the caller
//! always exits 0.
//!
//! The one thing printed: the news of relay mail (`news`), for a harness
//! with no way to tell one of its sessions anything from outside, whose
//! hooks carry it instead (`Harness::mail_by_hooks`: Claude Code). With mail waiting in the session's own folders
//! (`mcp::own_keys`), its `UserPromptSubmit` prints the JSON that adds a
//! line to the prompt's context, and its `Stop` the JSON that keeps it going
//! one more turn to read the mail — once for the same mail (`mail::told`),
//! never where a stop hook already keeps it going (`stop_hook_active`).
//! Only the copy that is heard tells (`target`), so one event tells once.

use crate::mcp::{self, Caller};
use crate::store::mail;
use crate::store::state::{self, HookState, Said};
use jiff::Timestamp;
use rl_core::Home;
use rl_detect::Registry;
use rl_detect::sessions::{Status, Told, UserHooks};
use rl_detect::user_hooks;
use serde_json::Value;
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::Instant;

use crate::store::sessions::HOOK as HOOK_FILE;

/// How long a hook may take, all told. Under Gemini's 2s, which it waits in
/// the agent's loop, and Codex's 3s.
pub const WITHIN: Duration = Duration::from_millis(1500);

/// `rl hook …`'s words, read before clap reads anything: a hook must never
/// fail the CLI that called it — Claude takes exit 2 from a `PreToolUse` or
/// `Stop` hook as "block" — nor print, so no word clap would refuse, and no
/// `--help`, ever reaches it. None when the command is not `rl hook`.
pub fn args() -> Option<Vec<String>> {
    let mut args = std::env::args_os().skip(1);
    (args.next()? == "hook").then(|| args.map(|a| a.to_string_lossy().into_owned()).collect())
}

/// What `rl hook` was called with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Words {
    pub agent: String,
    pub event: String,
    /// `--user`: the copy in the user's own settings, not the session's own
    /// (`user_hooks::USER`). Only Claude's says it; any agent's may.
    pub user: bool,
}

/// What `rl hook` was called with, from its words: exactly `<agent> <Event>`
/// or `<agent> <Event> --user`. None for any other shape, and the hook then
/// does nothing.
pub fn words(words: &[String]) -> Option<Words> {
    let (agent, event, user) = match words {
        [agent, event] => (agent, event, false),
        [agent, event, flag] if flag == user_hooks::USER => (agent, event, true),
        _ => return None,
    };
    let word = |w: &String| !w.is_empty() && !w.starts_with('-');
    (word(agent) && word(event)).then(|| Words {
        agent: agent.clone(),
        event: event.clone(),
        user,
    })
}

/// Where a word goes: the held session's folder, or `state/`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Held(PathBuf),
    Own,
}

/// Where this call's word goes, or None when it says nothing: an agent whose
/// hooks Relay does not know, the copy that is not heard here, a held
/// session that is not there.
fn target(home: &Home, hooks: &UserHooks, words: &Words, held: Option<&str>) -> Option<Target> {
    // Where the agent's user-level copy is marked, only that copy tells of
    // the agent's own sessions.
    let marked = hooks.marker.is_some();
    let user = words.user && marked;
    match held {
        Some(name) => {
            let dir = home.session_dir(name);
            (!user && !name.contains('/') && dir.is_dir()).then(|| Target::Held(dir.join(HOOK_FILE)))
        }
        None => (user || !marked).then_some(Target::Own),
    }
}

/// The held session this call is the own word of: `RL_SESSION`, when the
/// call is of the agent that session runs (`RL_SESSION_AGENT`). An agent
/// started inside a held shell is not that session's.
fn held_session(session: Option<String>, session_agent: Option<String>, agent: &str) -> Option<String> {
    session
        .filter(|s| !s.is_empty())
        .filter(|_| session_agent.as_deref() == Some(agent))
}

/// Does what the hook asks, or nothing, within `WITHIN`. Never fails.
pub async fn hook(home: &Home, words: &Words) {
    let deadline = Instant::now() + WITHIN;
    // Relay's own `claude agents`: nothing to tell of.
    if asking() {
        drain(deadline);
        return;
    }
    let registry = registry(home);
    let Some(hooks) = registry.user_hooks(&words.agent) else {
        drain(deadline);
        return;
    };
    let held = held_session(
        std::env::var(rl_core::SESSION_ENV).ok(),
        std::env::var(rl_core::AGENT_ENV).ok(),
        &words.agent,
    );
    let Some(target) = target(home, hooks, words, held.as_deref()) else {
        drain(deadline);
        return;
    };
    let Some(raw) = stdin_until(deadline) else { return };
    let payload: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let said = async {
        let env = |k: &str| std::env::var(k).ok();
        let Some(word) = Word::of(hooks, &words.event, &payload, env) else {
            return Ok(());
        };
        let path = match &target {
            Target::Held(path) => path.clone(),
            Target::Own => {
                state::path(home, &hooks.agent, &word.session_id).ok_or_else(|| io::Error::other("bad id"))?
            }
        };
        // A held session's agent that went on to another conversation
        // (`/resume`) starts afresh.
        let known = state::read(&path)
            .ok()
            .flatten()
            .filter(|k| !matches!(target, Target::Held(_)) || k.session_id == word.session_id);
        let pid = if known.as_ref().is_none_or(|k| k.pid.is_none()) && hooks.asks_pid(&words.event) {
            agent_pid(&words.agent).await
        } else {
            None
        };
        let heard = hear(hooks, &words.event, word, known, pid, Timestamp::now());
        match heard {
            Heard::Ended => match std::fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
            Heard::Stands(s) => {
                if matches!(target, Target::Own) && hooks.starts(&words.event) && s.pid.is_some() {
                    forget_same_process(home, &s);
                }
                state::write(&path, &s)
            }
        }
    };
    if let Ok(Err(e)) = tokio::time::timeout_at(deadline, said).await
        && std::env::var_os("RELAY_HOOK_DEBUG").is_some()
    {
        eprintln!("rl hook: {e:#}");
    }
    // A listing of a folder or two: done, unless the time is already gone.
    let by_hooks = registry
        .profile(&words.agent)
        .is_some_and(|p| rl_detect::harness::of(p, None).mail_by_hooks());
    if by_hooks
        && Instant::now() < deadline
        && let Some(said) = news(home, &words.agent, &words.event, held.as_deref(), &payload)
    {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{said}").and_then(|()| out.flush());
    }
}

/// What `agent` (Claude) is told of the relay mail waiting for the session
/// a hook of `event` is of (the held session `held`, else the payload's
/// conversation), as the JSON Claude reads from a hook's output; None when
/// it is told nothing. A `Stop` blocks once for the same mail: the newest
/// it told of is kept (`mail::set_told`), and only newer mail blocks again.
pub(crate) fn news(home: &Home, agent: &str, event: &str, held: Option<&str>, payload: &Value) -> Option<Value> {
    if event != PROMPT && event != STOP {
        return None;
    }
    let caller = Caller {
        harness: agent.into(),
        relay_session: held.map(str::to_string),
        session_id: payload
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    };
    let keys = mcp::own_keys(home, &caller);
    let waiting: Vec<mail::Message> = keys.iter().flat_map(|k| mail::waiting(home, k)).collect();
    let newest = waiting.iter().map(|m| m.at).max()?;
    let mut from: Vec<&str> = Vec::new();
    for m in &waiting {
        if !from.contains(&m.from.as_str()) {
            from.push(&m.from);
        }
    }
    let (n, from) = (waiting.len(), from.join(", "));
    if event == PROMPT {
        return Some(serde_json::json!({ "hookSpecificOutput": {
            "hookEventName": PROMPT,
            "additionalContext": format!("You have {n} relay message(s) waiting (from {from}). Call the relay inbox tool to read them."),
        }}));
    }
    let going = payload.get("stop_hook_active").and_then(Value::as_bool) == Some(true);
    let key = keys.first()?;
    if going || mail::told(home, key).is_some_and(|t| t >= newest) {
        return None;
    }
    // What cannot be kept does not block: it would at every stop.
    mail::set_told(home, key, newest).ok()?;
    Some(serde_json::json!({
        "decision": "block",
        "reason": format!("You have {n} relay message(s) from {from}. Call the relay inbox tool to read them, then carry on."),
    }))
}

/// Claude's events that tell of mail.
const PROMPT: &str = "UserPromptSubmit";
const STOP: &str = "Stop";

/// What a hook's payload says of its session: its id and folder (read from
/// the variables the agent's profile names, `env`, where the payload lacks
/// them: Gemini's), the conversation's file, and the event's detail.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    session_id: String,
    cwd: PathBuf,
    detail: Option<String>,
    transcript_path: Option<PathBuf>,
}

impl Word {
    /// None without a session and a whole folder.
    fn of(hooks: &UserHooks, event: &str, payload: &Value, env: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let field = |k: &str| payload.get(k).and_then(Value::as_str).map(str::to_string);
        let named = |var: &Option<String>| var.as_deref().and_then(&env);
        let session_id = field("session_id")
            .or_else(|| named(&hooks.from_env.session_id))
            .filter(|s| !s.is_empty())?;
        let cwd = PathBuf::from(field("cwd").or_else(|| named(&hooks.from_env.cwd))?);
        if !cwd.is_absolute() {
            return None;
        }
        Some(Self {
            session_id,
            cwd,
            detail: hooks.detail(event, payload),
            transcript_path: field("transcript_path").filter(|t| !t.is_empty()).map(PathBuf::from),
        })
    }
}

/// What a word makes of a session.
#[derive(Debug, PartialEq)]
enum Heard {
    Ended,
    Stands(HookState),
}

/// The session as `word` leaves it, or that it ended, as its agent's `hooks`
/// say: the one `known`, changed by the event, or a new one. A start heard of
/// a session already known says nothing new (codex runs its hooks in the
/// background, so a start may come after the prompt that followed it), and a
/// tool done ends a wait or keeps a turn going but never wakes an idle one.
fn hear(
    hooks: &UserHooks,
    event: &str,
    word: Word,
    known: Option<HookState>,
    pid: Option<u32>,
    now: Timestamp,
) -> Heard {
    let told = hooks.told(event, word.detail.as_deref());
    if told == Told::Ended {
        return Heard::Ended;
    }
    let wakes = known.as_ref().is_some_and(|k| k.state == Said::Idle) && hooks.carries_on(event);
    let changes = !(wakes || known.is_some() && hooks.starts(event));
    let mut s = known.unwrap_or_else(|| HookState {
        agent: hooks.agent.clone(),
        session_id: word.session_id.clone(),
        cwd: word.cwd.clone(),
        state: Said::Idle,
        detail: None,
        pid: None,
        transcript_path: None,
        started: now,
        at: now,
    });
    if let Told::Stands { status, waiting_for } = told
        && changes
    {
        s.state = match status {
            Status::Busy => Said::Busy,
            Status::Waiting => Said::Waiting,
            Status::Idle => Said::Idle,
        };
        s.detail = (s.state == Said::Waiting)
            .then(|| waiting_for.map(str::to_string).or(word.detail))
            .flatten();
    }
    s.cwd = word.cwd;
    s.pid = pid.or(s.pid);
    s.transcript_path = word.transcript_path.or(s.transcript_path);
    s.at = now;
    Heard::Stands(s)
}

/// A start in a process that held another session: that one is over.
fn forget_same_process(home: &Home, s: &HookState) {
    let Ok(files) = std::fs::read_dir(home.state_dir().join(&s.agent)) else {
        return;
    };
    for file in files.flatten() {
        let path = file.path();
        if let Ok(Some(other)) = state::read(&path)
            && other.session_id != s.session_id
            && other.pid == s.pid
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The profiles: the built-ins with yours over them; the built-ins alone
/// where yours do not load (said under `RELAY_HOOK_DEBUG`).
fn registry(home: &Home) -> Registry {
    Registry::load(home).unwrap_or_else(|e| {
        if std::env::var_os("RELAY_HOOK_DEBUG").is_some() {
            eprintln!("rl hook: detection overrides ignored: {e:#}");
        }
        Registry::builtin()
    })
}

/// All of stdin, if it is all there by `deadline`. The agent writes the
/// payload and closes the pipe, so this is quick; one that keeps it open is
/// not waited on past the deadline. The reading thread is left behind then,
/// and goes with the process.
fn stdin_until(deadline: Instant) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(std::io::read_to_string(std::io::stdin()).unwrap_or_default());
    });
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok()
}

/// Reads stdin to its end (or `deadline`) and drops it, so an agent writing
/// its payload to a hook that has nothing to do with it is not left blocked.
fn drain(deadline: Instant) {
    let _ = stdin_until(deadline);
}

/// Whether `ASKING` says an ask of Relay's is going on (`user_hooks::is_asking`).
fn asking() -> bool {
    user_hooks::is_asking(
        std::env::var(user_hooks::ASKING).ok().as_deref(),
        user_hooks::now_secs(),
    )
}

/// The agent's process: the nearest of this one's ancestors whose command
/// line names it (`agent_among`), as `ps` lists them. None where it is not
/// found, or `ps` is not quick.
async fn agent_pid(agent: &str) -> Option<u32> {
    let out = tokio::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,args="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    let table = processes(&String::from_utf8_lossy(&out.stdout));
    agent_among(&table, std::process::id(), agent)
}

/// `ps -o pid=,ppid=,args=` as pid → (parent, command line).
pub fn processes(text: &str) -> HashMap<u32, (u32, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (pid, rest) = l.split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            let (ppid, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            Some((pid.parse().ok()?, (ppid.parse().ok()?, args.trim().to_string())))
        })
        .collect()
}

/// The nearest ancestor of `me` whose command line names `agent`.
///
/// A hook is started through a shell (codex: `sh -lc` in a new session;
/// gemini: its shell), so its parent is not the agent; the agent is further
/// up. How each one's process is named is not certain: codex's npm package
/// starts a native binary (`…/codex/codex` or a per-platform name) under a
/// node launcher, and gemini may relaunch itself under node with flags
/// (`node --max-old-space-size=… …/gemini-cli/…`). So a process names the
/// agent where its program, or the first argument after it that is not a
/// flag, has a path part that is the agent's name, starts with it and a
/// `-` or `.` (`gemini-cli`, `codex.js`). A shell is never the agent, though
/// its command names `rl hook <agent>`.
pub fn agent_among(table: &HashMap<u32, (u32, String)>, me: u32, agent: &str) -> Option<u32> {
    let mut at = table.get(&me)?.0;
    for _ in 0..64 {
        let (parent, args) = table.get(&at)?;
        if names(args, agent) {
            return Some(at);
        }
        if *parent == at || *parent == 0 {
            return None;
        }
        at = *parent;
    }
    None
}

const SHELLS: [&str; 6] = ["sh", "bash", "zsh", "dash", "fish", "ksh"];

fn names(args: &str, agent: &str) -> bool {
    let mut words = args.split_whitespace();
    let Some(program) = words.next() else { return false };
    let base = |w: &str| w.rsplit('/').next().unwrap_or(w).trim_start_matches('-').to_string();
    if SHELLS.contains(&base(program).as_str()) {
        return false;
    }
    let part = |p: &str| {
        p == agent
            || p.strip_prefix(agent)
                .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('.'))
    };
    let script = words.find(|w| !w.starts_with('-'));
    [Some(program), script]
        .into_iter()
        .flatten()
        .any(|w| w.split('/').any(part))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    /// An agent's hooks, as its built-in profile has them.
    fn h(agent: &str) -> UserHooks {
        Registry::builtin().user_hooks(agent).unwrap().clone()
    }

    fn word(sid: &str) -> Word {
        Word {
            session_id: sid.into(),
            cwd: "/w".into(),
            detail: None,
            transcript_path: None,
        }
    }

    fn stands(h: Heard) -> HookState {
        match h {
            Heard::Stands(s) => s,
            Heard::Ended => panic!("ended"),
        }
    }

    #[test]
    fn the_first_word_is_kept_as_the_start() {
        let (codex, now) = (h("codex"), Timestamp::now());
        let first = stands(hear(&codex, "UserPromptSubmit", word("s"), None, None, now));
        assert_eq!(first.started, now);
        let later = now + jiff::SignedDuration::from_secs(30);
        let next = stands(hear(&codex, "UserPromptSubmit", word("s"), Some(first), None, later));
        assert_eq!((next.started, next.at), (now, later));
    }

    #[test]
    fn events_move_a_session_between_idle_busy_and_waiting() {
        let (codex, now) = (h("codex"), Timestamp::now());
        let prompt = stands(hear(&codex, "UserPromptSubmit", word("s"), None, Some(7), now));
        assert_eq!((prompt.state, prompt.pid), (Said::Busy, Some(7)));
        let asked = stands(hear(
            &codex,
            "PermissionRequest",
            word("s"),
            Some(prompt.clone()),
            None,
            now,
        ));
        assert_eq!((asked.state, asked.pid), (Said::Waiting, Some(7)), "the pid is kept");
        let done = stands(hear(&codex, "Stop", word("s"), Some(asked), None, now));
        assert_eq!(done.state, Said::Idle);
        assert_eq!(done.detail, None);
    }

    #[test]
    fn a_late_start_or_tool_done_changes_nothing() {
        let (codex, now) = (h("codex"), Timestamp::now());
        let busy = stands(hear(&codex, "UserPromptSubmit", word("s"), None, None, now));
        let late = stands(hear(&codex, "SessionStart", word("s"), Some(busy), None, now));
        assert_eq!(late.state, Said::Busy, "a start of a session known");
        let idle = stands(hear(&codex, "Stop", word("s"), Some(late), None, now));
        let woken = stands(hear(&codex, "PostToolUse", word("s"), Some(idle), None, now));
        assert_eq!(woken.state, Said::Idle, "a tool done never wakes an idle turn");
    }

    #[test]
    fn a_session_end_is_heard_as_one() {
        let claude = h("claude");
        assert_eq!(
            hear(&claude, "SessionEnd", word("s"), None, None, Timestamp::now()),
            Heard::Ended
        );
    }

    #[test]
    fn a_hook_needs_its_session_and_a_whole_folder() {
        let none = |_: &str| None;
        let codex = h("codex");
        let w = Word::of(
            &codex,
            "PermissionRequest",
            &json!({"session_id": "s1", "cwd": "/w/app", "tool_name": "shell", "hook_event_name": "PermissionRequest", "transcript_path": "/t/s1.jsonl"}),
            none,
        )
        .unwrap();
        assert_eq!(
            (w.session_id.as_str(), w.cwd.to_str(), w.detail.as_deref()),
            ("s1", Some("/w/app"), Some("shell"))
        );
        assert_eq!(w.transcript_path.as_deref(), Some(Path::new("/t/s1.jsonl")));
        assert!(Word::of(&codex, "Stop", &json!({"cwd": "/w"}), none).is_none());
        assert!(Word::of(&codex, "Stop", &json!({"session_id": "s", "cwd": "w"}), none).is_none());
        assert!(Word::of(&codex, "Stop", &Value::Null, none).is_none());
        // Gemini says them in its environment too.
        let env = |k: &str| match k {
            "GEMINI_SESSION_ID" => Some("g1".to_string()),
            "GEMINI_CWD" => Some("/w/g".to_string()),
            _ => None,
        };
        let g = Word::of(&h("gemini"), "AfterAgent", &Value::Null, env).unwrap();
        assert_eq!((g.session_id.as_str(), g.cwd.to_str()), ("g1", Some("/w/g")));
        assert!(
            Word::of(&codex, "Stop", &Value::Null, env).is_none(),
            "gemini's, not codex's"
        );
    }

    #[test]
    fn a_word_goes_where_the_copy_and_the_session_say() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path());
        std::fs::create_dir_all(home.session_dir("x")).unwrap();
        let w = |agent: &str, user: bool| Words {
            agent: agent.into(),
            event: "Stop".into(),
            user,
        };
        let held = Some(Target::Held(home.session_dir("x").join(HOOK_FILE)));
        let (claude, codex) = (h("claude"), h("codex"));
        assert_eq!(target(&home, &claude, &w("claude", true), None), Some(Target::Own));
        assert_eq!(target(&home, &claude, &w("claude", false), None), None);
        assert_eq!(target(&home, &claude, &w("claude", false), Some("x")), held);
        assert_eq!(target(&home, &claude, &w("claude", true), Some("x")), None);
        assert_eq!(target(&home, &codex, &w("codex", false), None), Some(Target::Own));
        assert_eq!(target(&home, &codex, &w("codex", false), Some("x")), held);
        assert_eq!(target(&home, &codex, &w("codex", false), Some("nope")), None);
    }

    #[test]
    fn only_the_held_sessions_own_agent_speaks_for_it() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(held_session(s("x"), s("claude"), "claude"), s("x"));
        assert_eq!(
            held_session(s("x"), s("sh"), "claude"),
            None,
            "claude inside a held shell"
        );
        assert_eq!(held_session(s("x"), None, "claude"), None);
        assert_eq!(held_session(s(""), s("claude"), "claude"), None);
        assert_eq!(held_session(None, s("claude"), "claude"), None);
    }

    #[test]
    fn a_hook_is_an_agent_an_event_and_perhaps_user() {
        let said = |args: &[&str]| {
            words(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>()).map(|w| (w.agent, w.event, w.user))
        };
        let t = |a: &str, e: &str, u: bool| Some((a.to_string(), e.to_string(), u));
        assert_eq!(said(&["codex", "Stop"]), t("codex", "Stop", false));
        assert_eq!(said(&["claude", "Stop", "--user"]), t("claude", "Stop", true));
        assert_eq!(
            said(&["codex", "Stop", "--user"]),
            t("codex", "Stop", true),
            "taken, and harmless"
        );
        for bad in [
            &[][..],
            &["Stop"],
            &["a", "b", "c"],
            &["--help"],
            &["-x", "Stop"],
            &["--dialect", "x", "Stop"],
            &["Stop", "--dialect=x"],
            &["codex", "Stop", "--dialect", "codex"],
            &["--user", "Stop"],
            &["claude", "--user"],
            &["claude", "Stop", "--users"],
            &[""],
        ] {
            assert_eq!(said(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn claudes_user_level_copy_says_so_after_its_event() {
        let said = |args: &[&str]| words(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>());
        let w = said(&["claude", "Stop", "--user"]).unwrap();
        assert_eq!((w.agent.as_str(), w.event.as_str(), w.user), ("claude", "Stop", true));
        assert!(!said(&["claude", "Stop"]).unwrap().user, "the session's own");
    }

    #[test]
    fn the_agent_is_found_up_the_chain_past_the_shell() {
        let table = processes(
            "    1     0 /sbin/launchd\n\
               500     1 /bin/zsh -l\n\
               600   500 node /opt/homebrew/lib/node_modules/@openai/codex/bin/codex.js\n\
               601   600 /opt/homebrew/lib/node_modules/@openai/codex/vendor/aarch64-apple-darwin/codex/codex\n\
               700   601 /bin/sh -lc /Users/me/codex/bin/rl hook codex Stop\n\
               701   700 /Users/me/codex/bin/rl hook codex Stop\n\
               800   500 node --max-old-space-size=8192 /usr/local/lib/node_modules/@google/gemini-cli/dist/index.js\n\
               801   800 bash -c /x/rl hook gemini AfterAgent\n\
               802   801 /x/rl hook gemini AfterAgent\n\
               900   500 /usr/bin/vim notes-on-codex.md\n\
               901   900 /x/rl hook codex Stop\n",
        );
        assert_eq!(
            table[&700],
            (601, "/bin/sh -lc /Users/me/codex/bin/rl hook codex Stop".into())
        );
        assert_eq!(
            agent_among(&table, 701, "codex"),
            Some(601),
            "the nearest, not the launcher"
        );
        assert_eq!(agent_among(&table, 802, "gemini"), Some(800));
        assert_eq!(agent_among(&table, 901, "codex"), None, "a file named for it is not it");
        assert_eq!(agent_among(&table, 802, "codex"), None);
        assert_eq!(agent_among(&table, 12345, "codex"), None, "not in the table");
        // A loop in a garbled table ends.
        let looped = processes("5 6 a\n6 5 b\n");
        assert_eq!(agent_among(&looped, 5, "codex"), None);
    }
}
