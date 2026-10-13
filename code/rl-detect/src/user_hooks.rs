//! Codex's and Gemini's hooks, set where the user's own are: one command
//! hook per event, all calling `rl hook <agent> <Event>`.
//!
//! Unlike Claude's (a settings file `rld` passes to each run it starts),
//! these live in the agent's user-level file, so every session of that
//! agent calls them, Relay's runs and the rest alike. `rl hook` tells them
//! apart by `RELAY_RUN`: a run's hook posts the run's signal (`signal`);
//! any other tells `rld` of the session (`told`), which keeps it on the
//! list as one of the agent's own.
//!
//! Nothing is written but on a yes in `rl setup`. The file stays the
//! user's: Relay's entries are appended, never anything removed or moved
//! but Relay's own (`is_relays`), and a file that does not read is never
//! rewritten.
//!
//! What the agents' docs and source said on 2026-10-13
//! (`docs/plans/2026-10-13-agent-hooks-notes.md`):
//! - Codex: `$CODEX_HOME/hooks.json` (default `~/.codex`), `{"hooks":
//!   {"<Event>": [{"hooks": [{"type": "command", "command", "timeout"
//!   (seconds), "async"}]}]}}`. A user-level hook runs only once trusted in
//!   codex's `/hooks` screen. Uncertain, per the notes: whether the codex
//!   installed (0.162) already has hooks on by default (`[features] hooks`),
//!   and whether `SessionStart` fires before the first prompt.
//! - Gemini: `~/.gemini/settings.json` (`$GEMINI_CLI_HOME` stands for the
//!   home), the same shape with `timeout` in milliseconds. Hooks run
//!   synchronously, the agent waiting on them. Uncertain: whether
//!   `SessionEnd` fires when gemini is killed (hence the pid check in
//!   `rld`), and that `$GEMINI_CLI_HOME/.gemini/settings.json` is the path
//!   (inferred from its source). Gemini reads its settings with comments
//!   allowed; one with comments does not read here and is left alone.

use crate::Signal;
use crate::claude::clip;
use anyhow::{Context, Result, bail};
use rl_proto::types::RunState;
use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const CODEX: &str = "codex";
pub const GEMINI: &str = "gemini";

/// The agents whose own sessions are learned from their hooks.
pub const AGENTS: [&str; 2] = [CODEX, GEMINI];

/// The events Relay hooks, by agent: a session's start, a turn's start, a
/// wait for your approval, a tool done (so a wait you answered is over),
/// an interrupted turn (codex's), a turn's end, the session's end.
pub fn events(agent: &str) -> &'static [&'static str] {
    match agent {
        CODEX => &[
            "SessionStart",
            "UserPromptSubmit",
            "PermissionRequest",
            "PostToolUse",
            "Interrupt",
            "Stop",
            "SessionEnd",
        ],
        GEMINI => &[
            "SessionStart",
            "BeforeAgent",
            "Notification",
            "AfterTool",
            "AfterAgent",
            "SessionEnd",
        ],
        _ => &[],
    }
}

/// The event that carries the prompt you typed (`prompt` in its payload).
pub fn prompt_event(agent: &str) -> Option<&'static str> {
    match agent {
        CODEX => Some("UserPromptSubmit"),
        GEMINI => Some("BeforeAgent"),
        _ => None,
    }
}

/// Codex waits this many seconds for a hook; `rl hook` is done well before.
const CODEX_TIMEOUT_SECS: u32 = 3;
/// Gemini waits this many milliseconds, and waits it in the agent's loop.
const GEMINI_TIMEOUT_MS: u32 = 2000;
/// Gemini's only notification type: a tool waits for your approval.
const TOOL_PERMISSION: &str = "ToolPermission";

/// What an event says about a Relay run of `agent`'s. None for events that
/// carry no state: `SessionStart`, which says nothing of whether a prompt
/// came with it (and codex may tell it after the prompt's, its hooks being
/// run in the background), and `SessionEnd`, as the process's exit right
/// after it is the exact signal.
pub fn signal(agent: &str, event: &str, payload: &Value) -> Option<Signal> {
    let s = |k: &str| payload.get(k).and_then(Value::as_str).map(|v| clip(v.trim()));
    Some(match (agent, event) {
        (CODEX, "UserPromptSubmit") | (GEMINI, "BeforeAgent") => {
            Signal::hook(RunState::Working, s("prompt").map(|p| format!("prompt: {p}")))
        }
        (CODEX, "PermissionRequest") => Signal::hook(
            RunState::NeedsInput,
            Some(match s("tool_name") {
                Some(t) => format!("approve: {t}"),
                None => "waiting for approval".into(),
            }),
        ),
        (GEMINI, "Notification") if s("notification_type").as_deref() == Some(TOOL_PERMISSION) => Signal::hook(
            RunState::NeedsInput,
            s("message").or_else(|| Some("waiting for approval".into())),
        ),
        // A tool ran: any wait for your approval is over, and the turn goes on.
        (CODEX, "PostToolUse") | (GEMINI, "AfterTool") => {
            Signal::hook(RunState::Working, s("tool_name").map(|t| format!("tool: {t} done")))
        }
        (CODEX, "Interrupt") => Signal::hook(RunState::Idle, Some("interrupted".into())),
        (CODEX, "Stop") | (GEMINI, "AfterAgent") => Signal::hook(RunState::Idle, Some("finished responding".into())),
        _ => return None,
    })
}

/// Whether the event only carries a turn on: a tool done. Codex runs its
/// hooks in the background, so one can arrive after the turn's end; it may
/// end a wait for you (needing you → working) or keep a turn working, and
/// never wakes an idle one.
pub fn carries_on(agent: &str, event: &str) -> bool {
    matches!((agent, event), (CODEX, "PostToolUse") | (GEMINI, "AfterTool"))
}

/// The agents a run can be, by its `cli`, whose hooks Relay knows.
const KNOWN: [&str; 3] = [crate::claude::DIALECT, CODEX, GEMINI];

/// Whether `agent`'s hook, carrying `run`'s `RELAY_RUN`, is not that run's:
/// the agent was started inside it. So when the run is a shell or a
/// command, or another agent Relay knows (a Claude run calling `codex
/// exec`). An agent run of a CLI Relay does not know, or none named, may be
/// this agent by another name (`npx @openai/codex`): its hooks are its own.
pub fn foreign(run: &rl_proto::types::Run, agent: &str) -> bool {
    use rl_proto::types::RunKind;
    match run.kind {
        RunKind::Shell | RunKind::Command | RunKind::Plugin => true,
        _ => run
            .cli
            .as_deref()
            .is_some_and(|cli| cli != agent && KNOWN.contains(&cli)),
    }
}

/// The word from a payload that narrows its event, for `rld`: a
/// notification's type, a permission's tool, a start's source, an end's
/// reason.
pub fn detail(event: &str, payload: &Value) -> Option<String> {
    let key = match event {
        "Notification" => "notification_type",
        "PermissionRequest" => "tool_name",
        "SessionStart" => "source",
        "SessionEnd" => "reason",
        _ => return None,
    };
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(|v| clip(v.trim()))
        .filter(|v| !v.is_empty())
}

/// What an event says about one of an agent's own sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Told {
    /// It stands so now: a `status` word (`Outside::BUSY` and the rest),
    /// and what it waits on, if anything.
    Stands {
        status: &'static str,
        waiting_for: Option<&'static str>,
    },
    /// It is over.
    Ended,
    /// Nothing about how it stands; only that it is still there.
    Nothing,
}

/// What `event` (narrowed by `detail`, as `detail` gives it) says about one
/// of `agent`'s own sessions.
pub fn told(agent: &str, event: &str, detail: Option<&str>) -> Told {
    use rl_proto::methods::Outside;
    let stands = |status| Told::Stands {
        status,
        waiting_for: None,
    };
    match (agent, event) {
        (CODEX | GEMINI, "SessionStart") => stands(Outside::IDLE),
        (CODEX, "UserPromptSubmit") | (GEMINI, "BeforeAgent") => stands(Outside::BUSY),
        (CODEX, "PermissionRequest") => Told::Stands {
            status: Outside::WAITING,
            waiting_for: Some("permission prompt"),
        },
        (GEMINI, "Notification") if detail == Some(TOOL_PERMISSION) => Told::Stands {
            status: Outside::WAITING,
            waiting_for: Some("permission prompt"),
        },
        (CODEX, "PostToolUse") | (GEMINI, "AfterTool") => stands(Outside::BUSY),
        (CODEX, "Stop" | "Interrupt") | (GEMINI, "AfterAgent") => stands(Outside::IDLE),
        (CODEX | GEMINI, "SessionEnd") => Told::Ended,
        _ => Told::Nothing,
    }
}

/// The agent's user-level hooks file, as the agent finds it from this
/// environment (`env` reads a variable): `$CODEX_HOME/hooks.json` or
/// `~/.codex/hooks.json`; `$GEMINI_CLI_HOME/.gemini/settings.json` or
/// `~/.gemini/settings.json`. None for another agent, or with no home.
pub fn file(agent: &str, env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    match agent {
        CODEX => Some(
            set("CODEX_HOME")
                .or_else(|| Some(set("HOME")?.join(".codex")))?
                .join("hooks.json"),
        ),
        GEMINI => Some(
            set("GEMINI_CLI_HOME")
                .or_else(|| set("HOME"))?
                .join(".gemini")
                .join("settings.json"),
        ),
        _ => None,
    }
}

/// The command an event's hook runs.
///
/// `|| true`: whatever `rl` does — gone (127), an older one that does not
/// know these words (clap's 2) — the hook succeeds. Gemini takes exit 2 as
/// "block", and codex warns on any failure; Relay's hook never does either.
pub fn command(rl_bin: &Path, agent: &str, event: &str) -> String {
    let rl = rl_bin.to_string_lossy().replace('\'', "'\\''");
    format!("'{rl}' hook {agent} {event}{NEVER_FAILS}")
}

/// What every hook command ends with, so it cannot fail.
const NEVER_FAILS: &str = " || true";

/// One event's entry: a matcher-less group with Relay's one command.
fn entry(agent: &str, event: &str, command: String) -> Value {
    let hook = match agent {
        // `async`: codex runs it in the background, so it can never hold a
        // turn up — but not the end, which codex might not wait for as it
        // exits (uncertain); `rl hook` is done in milliseconds anyway.
        CODEX if event == "SessionEnd" => {
            json!({"type": "command", "command": command, "timeout": CODEX_TIMEOUT_SECS})
        }
        CODEX => json!({"type": "command", "command": command, "timeout": CODEX_TIMEOUT_SECS, "async": true}),
        _ => json!({"type": "command", "command": command, "timeout": GEMINI_TIMEOUT_MS}),
    };
    json!({ "hooks": [hook] })
}

/// Whether codex's `config.toml` beside its hooks file has hooks of its
/// own, inline (`[hooks]`, `[[hooks.<Event>]]`): codex then warns on every
/// start that one layer has both. Only a look at the table headers.
pub fn inline_hooks(hooks_file: &Path) -> bool {
    let Some(dir) = hooks_file.parent() else { return false };
    std::fs::read_to_string(dir.join("config.toml")).is_ok_and(|t| {
        t.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with("[hooks]") || l.starts_with("[hooks.") || l.starts_with("[[hooks.")
        })
    })
}

/// Whether `command` is one of Relay's hooks for `agent`: `rl hook <agent>
/// <Event>`, by whatever path `rl` was installed at.
/// With or without the `|| true` an older Relay did not write: an install
/// brings such a one up to date.
pub fn is_relays(command: &str, agent: &str) -> bool {
    let mut words: Vec<&str> = command.split_whitespace().collect();
    if words.ends_with(&["||", "true"]) {
        words.truncate(words.len() - 2);
    }
    let n = words.len();
    if n < 4 || words[n - 3] != "hook" || words[n - 2] != agent {
        return false;
    }
    let program = words[..n - 3].join(" ");
    let program = program.trim_matches(|c| c == '\'' || c == '"');
    program.rsplit('/').next() == Some("rl")
}

/// The file as it reads: none when it is not there, an error when it is
/// there and is not a JSON object.
fn read(path: &Path) -> Result<Option<Map<String, Value>>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(Some(Map::new()));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => Ok(Some(m)),
        Ok(_) => bail!("{} is not a JSON object; left as it was", path.display()),
        Err(e) => bail!("{} does not read as JSON ({e}); left as it was", path.display()),
    }
}

/// The `hooks` object, which must be one where it is there at all.
fn hooks_of<'a>(doc: &'a mut Map<String, Value>, path: &Path) -> Result<&'a mut Map<String, Value>> {
    match doc.entry("hooks").or_insert_with(|| Value::Object(Map::new())) {
        Value::Object(h) => Ok(h),
        _ => bail!("{}: `hooks` is not an object; left as it was", path.display()),
    }
}

/// Each command in an event's list of groups, mutably.
fn commands(groups: &mut [Value]) -> impl Iterator<Item = &mut Value> {
    groups
        .iter_mut()
        .filter_map(|g| g.get_mut("hooks").and_then(Value::as_array_mut))
        .flatten()
        .filter_map(|h| h.get_mut("command"))
}

/// Whether Relay's hooks are in the file: any of them. A file that is not
/// there has none; one that does not read is an error.
pub fn installed(path: &Path, agent: &str) -> Result<bool> {
    let Some(doc) = read(path)? else { return Ok(false) };
    let Some(Value::Object(hooks)) = doc.get("hooks") else {
        return Ok(false);
    };
    Ok(hooks.values().filter_map(Value::as_array).any(|groups| {
        groups
            .iter()
            .filter_map(|g| g.get("hooks").and_then(Value::as_array))
            .flatten()
            .filter_map(|h| h.get("command").and_then(Value::as_str))
            .any(|c| is_relays(c, agent))
    }))
}

/// Adds Relay's hook to each of `agent`'s events in `path`, creating the
/// file where it is not there. An event that has Relay's already keeps it,
/// pointed at this `rl`; nothing else is touched. True when the file
/// changed.
pub fn install(path: &Path, agent: &str, rl_bin: &Path) -> Result<bool> {
    let mut doc = read(path)?.unwrap_or_default();
    let before = doc.clone();
    let hooks = hooks_of(&mut doc, path)?;
    for event in events(agent) {
        let cmd = command(rl_bin, agent, event);
        let groups = match hooks.entry(*event).or_insert_with(|| Value::Array(vec![])) {
            Value::Array(g) => g,
            _ => bail!("{}: `hooks.{event}` is not a list; left as it was", path.display()),
        };
        let mut found = false;
        for c in commands(groups) {
            if c.as_str().is_some_and(|c| is_relays(c, agent)) {
                *c = Value::String(cmd.clone());
                found = true;
            }
        }
        if !found {
            groups.push(entry(agent, event, cmd));
        }
    }
    if doc == before {
        return Ok(false);
    }
    write(path, &doc)?;
    Ok(true)
}

/// Takes Relay's hooks for `agent` out of `path`: only commands that are
/// Relay's, and a group or an event's list left empty by that. True when
/// the file changed; one not there is left not there.
pub fn uninstall(path: &Path, agent: &str) -> Result<bool> {
    let Some(mut doc) = read(path)? else { return Ok(false) };
    let Some(Value::Object(hooks)) = doc.get_mut("hooks") else {
        return Ok(false);
    };
    let mut changed = false;
    let relays = |h: &Value| {
        h.get("command")
            .and_then(Value::as_str)
            .is_some_and(|c| is_relays(c, agent))
    };
    hooks.retain(|_, groups| {
        let Value::Array(list) = groups else { return true };
        let had = list.len();
        list.retain_mut(|g| {
            let Some(inner) = g.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let n = inner.len();
            inner.retain(|h| !relays(h));
            if inner.len() == n {
                return true;
            }
            changed = true;
            !inner.is_empty()
        });
        // Emptied by this, it goes; one that was empty already stays.
        !(list.is_empty() && had > 0)
    });
    if !changed {
        return Ok(false);
    }
    if hooks.is_empty() {
        // `shift_remove`: the keys after it keep their order.
        doc.shift_remove("hooks");
    }
    write(path, &doc)?;
    Ok(true)
}

/// Where a write lands: the file a symlink points at — followed whether or
/// not that file is there yet, so a link is never replaced by a file.
fn target(path: &Path) -> PathBuf {
    let mut at = path.to_path_buf();
    for _ in 0..16 {
        match std::fs::read_link(&at) {
            Ok(to) => at = at.parent().map_or(to.clone(), |dir| dir.join(&to)),
            Err(_) => break,
        }
    }
    std::fs::canonicalize(&at).unwrap_or(at)
}

/// Writes the document over the file it came from, whole or not at all: a
/// sibling written and synced first, then moved over it. A symlink is
/// followed, so a file kept elsewhere (a dotfiles checkout) is the one
/// changed. The sibling is made with the file's own permissions from the
/// start (a new one, the user's alone): settings can hold tokens.
fn write(path: &Path, doc: &Map<String, Value>) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = target(path);
    let dir = path.parent().context("a file with no folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let text = serde_json::to_string_pretty(doc)? + "\n";
    let name = path.file_name().context("a file with no name")?.to_string_lossy();
    let tmp = dir.join(format!(".{name}.relay-{}", std::process::id()));
    let mode = std::fs::metadata(&path).map_or(0o600, |m| m.permissions().mode() & 0o7777);
    let written = (|| -> std::io::Result<()> {
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        // The mode asked for is narrowed by the umask; the file's own is kept.
        f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &path)
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing {}", path.display()));
    }
    // The rename itself, as far as the folder can be synced.
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::types::{Confidence, Source};

    const RL: &str = "/opt/relay/bin/rl";

    fn doc(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn the_files_are_where_each_agent_looks() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
        };
        let home = env(&[("HOME", "/h")]);
        assert_eq!(file(CODEX, home).unwrap(), Path::new("/h/.codex/hooks.json"));
        assert_eq!(file(GEMINI, home).unwrap(), Path::new("/h/.gemini/settings.json"));
        let moved = env(&[("HOME", "/h"), ("CODEX_HOME", "/c"), ("GEMINI_CLI_HOME", "/g")]);
        assert_eq!(file(CODEX, moved).unwrap(), Path::new("/c/hooks.json"));
        assert_eq!(file(GEMINI, moved).unwrap(), Path::new("/g/.gemini/settings.json"));
        assert_eq!(file(CODEX, env(&[("CODEX_HOME", "")])), None, "empty is unset");
        assert_eq!(file("claude", home), None);
    }

    #[test]
    fn codex_installs_into_an_empty_file_one_hook_per_event() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("codex-home/hooks.json");
        assert!(!installed(&path, CODEX).unwrap());
        assert!(install(&path, CODEX, Path::new(RL)).unwrap());
        let v = doc(&path);
        let hooks = v["hooks"].as_object().unwrap();
        assert_eq!(hooks.keys().map(String::as_str).collect::<Vec<_>>(), events(CODEX));
        assert_eq!(
            hooks["PermissionRequest"],
            json!([{"hooks": [{"type": "command", "command": "'/opt/relay/bin/rl' hook codex PermissionRequest || true",
                               "timeout": 3, "async": true}]}])
        );
        assert!(installed(&path, CODEX).unwrap());
        assert!(!installed(&path, GEMINI).unwrap(), "another agent's are not these");
    }

    #[test]
    fn gemini_keeps_every_other_key_and_hook_in_order_and_never_twice() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        let mine = r#"{
  "theme": "GitHub",
  "hooks": {
    "BeforeTool": [{"matcher": "write_file", "hooks": [{"type": "command", "command": "check.sh", "timeout": 5000}]}],
    "AfterAgent": [{"hooks": [{"type": "command", "command": "notify-me"}]}]
  },
  "general": {"vimMode": true}
}"#;
        std::fs::write(&path, mine).unwrap();
        assert!(install(&path, GEMINI, Path::new("/opt/my tools/rl")).unwrap());
        let v = doc(&path);
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["theme", "hooks", "general"], "nothing moved");
        assert_eq!(v["general"], json!({"vimMode": true}));
        assert_eq!(v["hooks"]["BeforeTool"][0]["hooks"][0]["command"], "check.sh");
        let after = v["hooks"]["AfterAgent"].as_array().unwrap();
        assert_eq!(after.len(), 2, "the user's first, Relay's after");
        assert_eq!(after[0]["hooks"][0]["command"], "notify-me");
        assert_eq!(
            after[1],
            json!({"hooks": [{"type": "command", "command": "'/opt/my tools/rl' hook gemini AfterAgent || true", "timeout": 2000}]})
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!install(&path, GEMINI, Path::new("/opt/my tools/rl")).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "twice: no duplicate");
        // `rl` moved: its hooks follow, still once each.
        assert!(install(&path, GEMINI, Path::new(RL)).unwrap());
        let v = doc(&path);
        let after = v["hooks"]["AfterAgent"].as_array().unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(
            after[1]["hooks"][0]["command"],
            "'/opt/relay/bin/rl' hook gemini AfterAgent || true"
        );
        // One an older Relay wrote, with no `|| true`: brought up to date, not doubled.
        let mut v = doc(&path);
        v["hooks"]["AfterAgent"][1]["hooks"][0]["command"] = json!("/opt/relay/bin/rl hook gemini AfterAgent");
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        assert!(installed(&path, GEMINI).unwrap());
        assert!(install(&path, GEMINI, Path::new(RL)).unwrap());
        let after = doc(&path)["hooks"]["AfterAgent"].as_array().unwrap().clone();
        assert_eq!(after.len(), 2);
        assert_eq!(
            after[1]["hooks"][0]["command"],
            "'/opt/relay/bin/rl' hook gemini AfterAgent || true"
        );
    }

    #[test]
    fn a_hook_is_foreign_to_a_shell_a_command_or_another_known_agent_only() {
        use rl_proto::types::{Run, RunKind};
        let run = |kind, cli: Option<&str>| {
            let mut r = Run::new(
                "r1",
                kind,
                "t",
                vec!["x".into()],
                "/w",
                "/w",
                "2026-10-13T10:00:00Z".parse().unwrap(),
            );
            r.cli = cli.map(Into::into);
            r
        };
        assert!(foreign(&run(RunKind::Shell, None), CODEX));
        assert!(foreign(&run(RunKind::Command, None), CODEX));
        assert!(foreign(&run(RunKind::Agent, Some("claude")), CODEX));
        assert!(foreign(&run(RunKind::Agent, Some("gemini")), CODEX));
        assert!(!foreign(&run(RunKind::Agent, Some("codex")), CODEX));
        // A wrapper Relay does not know (`npx @openai/codex`), or none named:
        // the hooks are the run's own.
        assert!(!foreign(&run(RunKind::Agent, Some("npx")), CODEX));
        assert!(!foreign(&run(RunKind::Agent, None), GEMINI));
        assert!(carries_on(CODEX, "PostToolUse") && carries_on(GEMINI, "AfterTool"));
        assert!(!carries_on(CODEX, "AfterTool") && !carries_on(CODEX, "UserPromptSubmit"));
    }

    #[test]
    fn a_hook_command_never_fails_whatever_rl_does() {
        let tmp = tempfile::tempdir().unwrap();
        let run = |cmd: &str| {
            std::process::Command::new("/bin/sh")
                .args(["-c", cmd])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .code()
        };
        // Gone: the shell's 127, made 0.
        let gone = command(&tmp.path().join("no such/rl"), GEMINI, "AfterTool");
        assert_eq!(run(&gone), Some(0), "{gone}");
        // An older `rl` that refuses these words with clap's 2: still 0.
        std::fs::create_dir_all(tmp.path().join("it's")).unwrap();
        let old = tmp.path().join("it's/rl");
        std::fs::write(&old, "#!/bin/sh\nexit 2\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cmd = command(&old, GEMINI, "AfterTool");
        assert_eq!(run(&cmd), Some(0), "{cmd}");
        assert!(is_relays(&cmd, GEMINI), "a quote in the path is still Relay's: {cmd}");
    }

    #[test]
    fn uninstall_takes_only_relays_out() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hooks.json");
        std::fs::write(
            &path,
            r#"{"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}], "PreCompact": []}, "x": 1}"#,
        )
        .unwrap();
        install(&path, CODEX, Path::new(RL)).unwrap();
        // One of Relay's shares a group with the user's own.
        let mut v = doc(&path);
        v["hooks"]["Stop"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type": "command", "command": "rl hook codex Stop"}));
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        assert!(uninstall(&path, CODEX).unwrap());
        assert_eq!(
            doc(&path),
            json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}], "PreCompact": []}, "x": 1})
        );
        assert!(!uninstall(&path, CODEX).unwrap(), "nothing of Relay's left");
        let gone = tmp.path().join("nothing.json");
        assert!(!uninstall(&gone, CODEX).unwrap());
        assert!(!gone.exists(), "a file not there stays not there");
        // Relay's alone: the file is left with no hooks key at all.
        let only = tmp.path().join("only.json");
        install(&only, CODEX, Path::new(RL)).unwrap();
        uninstall(&only, CODEX).unwrap();
        assert_eq!(doc(&only), json!({}));
    }

    #[test]
    fn a_file_that_does_not_read_is_never_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        for text in [
            "{ \"hooks\": { // mine\n } }",
            "[1, 2]",
            r#"{"hooks": []}"#,
            r#"{"hooks": {"AfterAgent": {"not": "a list"}}}"#,
        ] {
            let path = tmp.path().join("settings.json");
            std::fs::write(&path, text).unwrap();
            let e = install(&path, GEMINI, Path::new(RL)).unwrap_err();
            assert!(format!("{e:#}").contains("left as it was"), "{text}: {e:#}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, "{ nope").unwrap();
        assert!(installed(&path, GEMINI).is_err());
        assert!(uninstall(&path, GEMINI).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ nope");
    }

    #[test]
    fn a_symlinked_file_is_changed_where_it_lives() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("dotfiles/hooks.json");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "{}").unwrap();
        let link = tmp.path().join("hooks.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        install(&link, CODEX, Path::new(RL)).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(installed(&real, CODEX).unwrap());
    }

    #[test]
    fn relays_commands_are_known_by_their_words() {
        assert!(is_relays("/usr/local/bin/rl hook codex Stop", CODEX));
        assert!(is_relays("'/opt/my tools/rl' hook gemini AfterAgent", GEMINI));
        assert!(is_relays("'/opt/my tools/rl' hook gemini AfterAgent || true", GEMINI));
        assert!(is_relays("'/usr/local/bin/rl' hook codex Stop ||   true  ", CODEX));
        assert!(!is_relays("'/usr/local/bin/rl' hook codex || true", CODEX));
        assert!(is_relays("rl hook codex Whatever", CODEX));
        assert!(!is_relays("/usr/local/bin/rl hook gemini Stop", CODEX));
        assert!(!is_relays("/bin/curl hook codex Stop", CODEX));
        assert!(!is_relays("rl hook codex", CODEX));
        assert!(!is_relays("say done", CODEX));
    }

    #[test]
    fn events_say_how_a_session_stands() {
        let stands = |status, waiting_for| Told::Stands { status, waiting_for };
        assert_eq!(
            told(CODEX, "PermissionRequest", Some("Bash")),
            stands("waiting", Some("permission prompt"))
        );
        assert_eq!(
            told(GEMINI, "Notification", Some("ToolPermission")),
            stands("waiting", Some("permission prompt"))
        );
        assert_eq!(told(GEMINI, "Notification", Some("Other")), Told::Nothing);
        assert_eq!(told(CODEX, "UserPromptSubmit", None), stands("busy", None));
        assert_eq!(told(GEMINI, "BeforeAgent", None), stands("busy", None));
        assert_eq!(told(CODEX, "Stop", None), stands("idle", None));
        assert_eq!(told(GEMINI, "AfterAgent", None), stands("idle", None));
        assert_eq!(told(GEMINI, "SessionStart", Some("startup")), stands("idle", None));
        assert_eq!(told(CODEX, "SessionEnd", None), Told::Ended);
        assert_eq!(
            told(CODEX, "BeforeAgent", None),
            Told::Nothing,
            "gemini's word, not codex's"
        );
        assert_eq!(
            detail("Notification", &json!({"notification_type": "ToolPermission"})).as_deref(),
            Some("ToolPermission")
        );
        assert_eq!(detail("Stop", &json!({"last_assistant_message": "long"})), None);
    }

    #[test]
    fn events_say_how_a_relay_run_stands() {
        let s = signal(CODEX, "PermissionRequest", &json!({"tool_name": "shell"})).unwrap();
        assert_eq!(
            (s.state, s.source, s.confidence, s.detail.as_deref()),
            (
                RunState::NeedsInput,
                Source::Hook,
                Confidence::Exact,
                Some("approve: shell")
            )
        );
        let s = signal(GEMINI, "BeforeAgent", &json!({"prompt": "fix it\nnow"})).unwrap();
        assert_eq!(
            (s.state, s.detail.as_deref()),
            (RunState::Working, Some("prompt: fix it"))
        );
        assert_eq!(
            signal(
                GEMINI,
                "Notification",
                &json!({"notification_type": "ToolPermission", "message": "Allow?"})
            )
            .unwrap()
            .state,
            RunState::NeedsInput
        );
        assert!(signal(GEMINI, "Notification", &json!({})).is_none());
        assert_eq!(signal(CODEX, "Stop", &json!({})).unwrap().state, RunState::Idle);
        assert_eq!(signal(GEMINI, "AfterAgent", &json!({})).unwrap().state, RunState::Idle);
        assert!(signal(CODEX, "SessionEnd", &json!({})).is_none());
        assert!(
            signal("claude", "Stop", &json!({})).is_none(),
            "Claude's are claude::signal's"
        );
    }

    #[test]
    fn a_tool_that_ran_ends_the_wait_and_an_interrupt_ends_the_turn() {
        // A run: approve, then the tool runs — working again, exactly.
        for (agent, ran) in [(CODEX, "PostToolUse"), (GEMINI, "AfterTool")] {
            assert!(events(agent).contains(&ran), "{agent} hooks {ran}");
            let s = signal(agent, ran, &json!({"tool_name": "shell"})).unwrap();
            assert_eq!(
                (s.state, s.confidence, s.detail.as_deref()),
                (RunState::Working, Confidence::Exact, Some("tool: shell done"))
            );
            assert_eq!(
                told(agent, ran, None),
                Told::Stands {
                    status: "busy",
                    waiting_for: None
                }
            );
        }
        assert!(events(CODEX).contains(&"Interrupt"));
        assert_eq!(signal(CODEX, "Interrupt", &json!({})).unwrap().state, RunState::Idle);
        assert_eq!(
            told(CODEX, "Interrupt", None),
            Told::Stands {
                status: "idle",
                waiting_for: None
            }
        );
    }

    #[test]
    fn taking_the_hooks_key_out_keeps_the_order_of_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, r#"{"theme": "x", "general": {}, "ui": 1}"#).unwrap();
        install(&path, GEMINI, Path::new(RL)).unwrap();
        // `hooks` went in last; move it between others, as a user might.
        std::fs::write(
            &path,
            std::fs::read_to_string(&path).unwrap().replacen(
                "\"theme\": \"x\",",
                "\"theme\": \"x\", \"keep\": true,",
                1,
            ),
        )
        .unwrap();
        let v = doc(&path);
        let mut m = v.as_object().unwrap().clone();
        let hooks = m.shift_remove("hooks").unwrap();
        let mut reordered = Map::new();
        for (k, val) in m {
            reordered.insert(k.clone(), val);
            if k == "theme" {
                reordered.insert("hooks".into(), hooks.clone());
            }
        }
        std::fs::write(&path, serde_json::to_string(&reordered).unwrap()).unwrap();
        uninstall(&path, GEMINI).unwrap();
        let keys: Vec<String> = doc(&path).as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["theme", "keep", "general", "ui"]);
    }

    #[test]
    fn a_write_keeps_the_files_permissions_and_a_new_one_is_the_users_alone() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let fresh = tmp.path().join("new/hooks.json");
        install(&fresh, CODEX, Path::new(RL)).unwrap();
        assert_eq!(mode(&fresh), 0o600);
        let shared = tmp.path().join("settings.json");
        std::fs::write(&shared, "{}").unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o640)).unwrap();
        install(&shared, GEMINI, Path::new(RL)).unwrap();
        assert_eq!(mode(&shared), 0o640);
        let left: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".relay-"))
            .collect();
        assert!(left.is_empty(), "no temp file left behind");
    }

    #[test]
    fn a_link_to_a_file_not_there_yet_stays_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("dotfiles")).unwrap();
        let link = tmp.path().join("hooks.json");
        std::os::unix::fs::symlink("dotfiles/hooks.json", &link).unwrap();
        install(&link, CODEX, Path::new(RL)).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(installed(&tmp.path().join("dotfiles/hooks.json"), CODEX).unwrap());
    }

    #[test]
    fn codex_hooks_inline_in_its_config_are_noticed_and_its_end_is_waited_for() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join("hooks.json");
        assert!(!inline_hooks(&hooks), "no config at all");
        std::fs::write(
            tmp.path().join("config.toml"),
            "model = \"x\"\n[features]\nhooks = true\n",
        )
        .unwrap();
        assert!(!inline_hooks(&hooks), "a feature flag is not a hook");
        std::fs::write(
            tmp.path().join("config.toml"),
            "model = \"x\"\n\n[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\n",
        )
        .unwrap();
        assert!(inline_hooks(&hooks));
        install(&hooks, CODEX, Path::new(RL)).unwrap();
        let v = doc(&hooks);
        assert_eq!(v["hooks"]["SessionEnd"][0]["hooks"][0].get("async"), None);
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["async"], true);
    }
}
