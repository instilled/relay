//! `rl hook <agent> <Event>`: what an agent's hook calls, with the hook's
//! JSON on stdin.
//!
//! Under a Relay run (`RELAY_RUN` set) it is that run's signal, posted as
//! `run.hook`. Outside any run — or inside a run of another kind, which
//! `rld` answers by leaving the run as it was — an agent's user-level hook
//! tells `rld` of one of the agent's own sessions (`outside.hook`), which
//! the list then shows as theirs.
//!
//! What each agent's events say is in its profile (`sessions.hooks`),
//! read from the built-in profiles and yours in `$RELAY_HOME/detect`, as
//! the panel reads them: a hook of an agent with none there tells of no
//! session.
//!
//! Claude's user-level hook says `--user` after the event (its profile's
//! `marker`): a Claude run of Relay's calls both it and the run's own hook
//! (`rl hook claude <Event>`, from the settings `rld` passes it), and only
//! the run's own is the run's signal. The `--user` copy is never posted as
//! `run.hook`; it goes to `rld` as `outside.hook`, with the run it was
//! called under, and `rld` lets it go when that run is Claude's own
//! (`user_hooks::foreign`) — one call either way. The run's own hook,
//! outside any run, says nothing.
//!
//! A hook called from a `claude` that `rld` runs itself (`claude agents`,
//! `user_hooks::ASKING` in its environment, set under 30s ago) does nothing
//! at all.
//!
//! The agent's process is looked for (`ps`) once a session: the word goes
//! to `rld` without a pid, and only where `rld` answers that it knows none
//! (`wants_pid`), for an event that is worth it (`asks_pid`), is it said
//! once more with the pid.
//!
//! A hook must never slow down or break the agent that called it: all of
//! it — reading stdin, finding the agent's process, the post — is done
//! within `WITHIN`, or given up on; nothing is printed (some agents feed a
//! hook's stdout back to the model, and Gemini parses it); the daemon is
//! never started from here; and the caller always exits 0.

use crate::client::Client;
use rl_core::Home;
use rl_detect::Registry;
use rl_detect::sessions::UserHooks;
use rl_detect::user_hooks;
use rl_proto::methods::{OutsideHook, OutsideHookParams, RunHook, RunHookParams};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::Instant;

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
    /// `--user`: the copy in the user's own settings, not a run's own
    /// (`user_hooks::USER`). Only Claude's says it; any agent's may.
    pub user: bool,
}

/// What `rl hook` was called with, from its words: `<agent> <Event>`, or
/// `<Event>` alone as written before the agent was named, which is
/// Claude's; and `--user`, wherever it stands. `--dialect <agent>` (or
/// `=<agent>`), the older spelling, names the agent as well, wherever it
/// stands; naming two that differ is not a shape this knows. None for any
/// shape it does not know — any other word with a dash — and the hook then
/// does nothing.
pub fn words(words: &[String]) -> Option<Words> {
    let mut dialect = None;
    let mut user = false;
    let mut rest = Vec::new();
    let mut words = words.iter();
    while let Some(w) = words.next() {
        if w == "--dialect" {
            dialect = Some(words.next()?.clone());
        } else if let Some(d) = w.strip_prefix("--dialect=") {
            dialect = Some(d.to_string());
        } else if w == user_hooks::USER {
            user = true;
        } else if w.starts_with('-') || w.is_empty() {
            return None;
        } else {
            rest.push(w.clone());
        }
    }
    let (agent, event) = match (rest.as_slice(), dialect) {
        ([event], dialect) => (dialect.unwrap_or_else(|| user_hooks::CLAUDE.into()), event.clone()),
        ([agent, event], Some(d)) if d != *agent => return None,
        ([agent, event], _) => (agent.clone(), event.clone()),
        _ => return None,
    };
    Some(Words { agent, event, user })
}

/// Does what the hook asks, or nothing, within `WITHIN`. Never fails.
pub async fn hook(home: &Home, words: &Words) {
    // `rld`'s own `claude agents`: nothing to tell of, and telling of it
    // would only have `rld` ask again.
    if asking() {
        return;
    }
    let deadline = Instant::now() + WITHIN;
    let (agent, event) = (words.agent.as_str(), words.event.as_str());
    let run = std::env::var("RELAY_RUN").ok().filter(|s| !s.is_empty());
    let registry = registry(home);
    let hooks = registry.user_hooks(agent);
    // Where the agent's user-level copy is marked, only that copy tells of
    // the agent's own sessions, and it is never a run's signal.
    let marked = hooks.is_some_and(|h| h.marker.is_some());
    let user = words.user && marked;
    let tells = hooks.is_some() && (user || !marked);
    let signals = run.is_some() && !user;
    if !tells && !signals {
        return;
    }
    let Some(raw) = stdin_until(deadline) else { return };
    let payload: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let said = async {
        // The daemon first: with none up, nothing else is worth the time.
        let mut rld = Client::connect(home).await?;
        if signals && let Some(id) = run {
            let params = RunHookParams {
                id,
                dialect: agent.to_string(),
                event: event.to_string(),
                payload: payload.clone(),
            };
            let run = rld.call::<RunHook>(params).await?;
            // The run's own word: said. Another agent's, started inside that
            // run (a codex a shell run started), is one of the agent's own.
            if !tells || !user_hooks::foreign(&run, agent, &registry) {
                return anyhow::Ok(());
            }
        }
        let Some(hooks) = hooks.filter(|_| tells) else {
            return anyhow::Ok(());
        };
        let Some(mut params) = outside(hooks, event, &payload, |k| std::env::var(k).ok()) else {
            return anyhow::Ok(());
        };
        let told = rld.call::<OutsideHook>(params.clone()).await?;
        if finds_pid(hooks, event, told.wants_pid)
            && let Some(pid) = agent_pid(agent).await
        {
            params.pid = Some(pid);
            rld.call::<OutsideHook>(params).await?;
        }
        anyhow::Ok(())
    };
    if let Ok(Err(e)) = tokio::time::timeout_at(deadline, said).await
        && std::env::var_os("RELAY_HOOK_DEBUG").is_some()
    {
        eprintln!("rl hook: {e:#}");
    }
}

/// The profiles, as the panel reads them: the built-ins with yours over
/// them; the built-ins alone where yours do not load (`rld` says what is
/// wrong with them).
fn registry(home: &Home) -> Registry {
    Registry::load(home).unwrap_or_else(|_| Registry::builtin())
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

/// What an outside session's hook tells `rld`, from its payload: its
/// session and folder (read from the variables the agent's profile names,
/// `from_env`, where the payload lacks them: Gemini's), and the run it was
/// called under (`RELAY_RUN`). None without both.
pub fn outside(
    hooks: &UserHooks,
    event: &str,
    payload: &Value,
    env: impl Fn(&str) -> Option<String>,
) -> Option<OutsideHookParams> {
    let field = |k: &str| payload.get(k).and_then(Value::as_str).map(str::to_string);
    let named = |var: &Option<String>| var.as_deref().and_then(&env);
    let session_id = field("session_id")
        .or_else(|| named(&hooks.from_env.session_id))
        .filter(|s| !s.is_empty())?;
    let cwd = PathBuf::from(field("cwd").or_else(|| named(&hooks.from_env.cwd))?);
    if !cwd.is_absolute() {
        return None;
    }
    Some(OutsideHookParams {
        agent: hooks.agent.clone(),
        session_id,
        cwd,
        event: event.to_string(),
        detail: hooks.detail(event, payload),
        pid: None,
        transcript_path: field("transcript_path").filter(|t| !t.is_empty()).map(PathBuf::from),
        run: env("RELAY_RUN").filter(|r| !r.is_empty()),
    })
}

/// Whether `ASKING` says an ask of `rld`'s is going on (`user_hooks::is_asking`).
fn asking() -> bool {
    user_hooks::is_asking(
        std::env::var(user_hooks::ASKING).ok().as_deref(),
        user_hooks::now_secs(),
    )
}

/// Whether to look for the agent's process, after `rld` answered
/// `wants_pid` to a word on `event`: only where it knows none, and the
/// event is worth it (`UserHooks::asks_pid`: a start, or a prompt, the
/// first word `rld` may hear of a session it did not see start). The rest —
/// a tool done, above all, which Gemini waits on — do without `ps`; `rld`
/// keeps the pid it was given.
pub fn finds_pid(hooks: &UserHooks, event: &str, wants_pid: bool) -> bool {
    wants_pid && hooks.asks_pid(event)
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

    /// An agent's hooks, as its built-in profile has them.
    fn h(agent: &str) -> UserHooks {
        Registry::builtin().user_hooks(agent).unwrap().clone()
    }

    #[test]
    fn the_process_is_looked_for_only_where_rld_wants_it_and_the_event_is_worth_it() {
        let codex = h("codex");
        assert!(finds_pid(&codex, "SessionStart", true));
        assert!(finds_pid(&codex, "UserPromptSubmit", true));
        assert!(finds_pid(&h("gemini"), "BeforeAgent", true));
        assert!(!finds_pid(&codex, "SessionStart", false), "known: no ps");
        assert!(!finds_pid(&codex, "PermissionRequest", false));
        assert!(!finds_pid(&codex, "PermissionRequest", true), "not worth a ps");
        assert!(!finds_pid(&codex, "PostToolUse", true));
    }

    #[test]
    fn a_hook_names_its_agent_or_is_claudes() {
        let said =
            |args: &[&str]| words(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>()).map(|w| (w.agent, w.event));
        let pair = |a: &str, e: &str| Some((a.to_string(), e.to_string()));
        assert_eq!(said(&["codex", "Stop"]), pair("codex", "Stop"));
        assert_eq!(said(&["Stop"]), pair("claude", "Stop"));
        // `--dialect`, wherever it stands, names the agent; two that differ
        // are no shape.
        assert_eq!(said(&["--dialect", "x", "Stop"]), pair("x", "Stop"));
        assert_eq!(said(&["Stop", "--dialect=x"]), pair("x", "Stop"));
        assert_eq!(said(&["codex", "Stop", "--dialect", "codex"]), pair("codex", "Stop"));
        assert_eq!(said(&["codex", "Stop", "--dialect", "gemini"]), None);
        for bad in [
            &[][..],
            &["a", "b", "c"],
            &["--help"],
            &["-x", "Stop"],
            &["Stop", "--dialect"],
            &[""],
            &["claude", "Stop", "--users"],
            &["--user"],
        ] {
            assert_eq!(said(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn claudes_user_level_copy_says_so_after_its_event() {
        let said = |args: &[&str]| words(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>());
        let w = said(&["claude", "Stop", "--user"]).unwrap();
        assert_eq!((w.agent.as_str(), w.event.as_str(), w.user), ("claude", "Stop", true));
        assert!(!said(&["claude", "Stop"]).unwrap().user, "a run's own");
        assert!(said(&["--user", "Stop"]).unwrap().user, "wherever it stands");
        assert!(said(&["codex", "Stop", "--user"]).unwrap().user, "taken, and harmless");
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

    #[test]
    fn an_outside_hook_needs_its_session_and_a_whole_folder() {
        let none = |_: &str| None;
        let (codex, gemini) = (h("codex"), h("gemini"));
        let p = outside(
            &codex,
            "PermissionRequest",
            &json!({"session_id": "s1", "cwd": "/w/app", "tool_name": "shell", "hook_event_name": "PermissionRequest", "transcript_path": "/t/s1.jsonl"}),
            none,
        )
        .unwrap();
        assert_eq!(
            (p.session_id.as_str(), p.cwd.to_str(), p.detail.as_deref(), p.pid),
            ("s1", Some("/w/app"), Some("shell"), None)
        );
        assert_eq!(p.transcript_path.as_deref(), Some(std::path::Path::new("/t/s1.jsonl")));
        assert_eq!(p.run, None);
        let under = |k: &str| (k == "RELAY_RUN").then(|| "r-1".to_string());
        let p = outside(&h("claude"), "Stop", &json!({"session_id": "s1", "cwd": "/w"}), under).unwrap();
        assert_eq!(p.run.as_deref(), Some("r-1"), "the run it was called under");
        assert!(outside(&codex, "Stop", &json!({"cwd": "/w"}), none).is_none());
        assert!(outside(&codex, "Stop", &json!({"session_id": "s", "cwd": "w"}), none).is_none());
        assert!(outside(&codex, "Stop", &Value::Null, none).is_none());
        // Gemini says them in its environment too.
        let env = |k: &str| match k {
            "GEMINI_SESSION_ID" => Some("g1".to_string()),
            "GEMINI_CWD" => Some("/w/g".to_string()),
            _ => None,
        };
        let g = outside(&gemini, "AfterAgent", &Value::Null, env).unwrap();
        assert_eq!((g.session_id.as_str(), g.cwd.to_str()), ("g1", Some("/w/g")));
        assert!(
            outside(&codex, "Stop", &Value::Null, env).is_none(),
            "gemini's, not codex's"
        );
    }
}
