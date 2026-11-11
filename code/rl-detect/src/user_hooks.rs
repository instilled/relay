//! The format of the agents' own hooks, set where the user's are: one
//! command hook per event, all calling `rl hook <agent> <Event>`, with the
//! marker after it where the agent's profile has one (Claude's `--user`).
//!
//! Each agent's facts — where its file is, which events it hooks and what
//! each says, how long it waits for one — are in its profile
//! (`sessions::UserHooks`). This is what is the same for all of them: the
//! file's shape, `{"hooks": {"<Event>": [{"hooks": [{"type": "command",
//! "command", "timeout"}]}]}}`, which Claude, Codex and Gemini share. An
//! agent whose hooks are written another way would need that way here.
//!
//! These live in the agent's user-level file, so every session of that
//! agent calls them, Relay's held sessions and the rest alike. `rl hook`
//! tells them apart by `RL_SESSION`: under a session Relay holds of that
//! agent's, the word is that session's; any other is one of the agent's
//! own (`UserHooks::told`), which keeps it on the list. Where a held
//! session of the agent's has hooks of its own beside these (Claude's,
//! `claude::settings_json`, passed as `--settings`), the marker tells this
//! copy apart: under a held session of that agent's it is let go; under
//! any other (a claude started in a held shell), or none, it tells of one
//! of the agent's own sessions.
//!
//! Beside the hooks, a profile may name settings of the agent's own that
//! make it draw well under Relay (`UserHooks::settings`, Gemini's
//! alternate buffer). They are set with the hooks where the file has no
//! value there, never over one, and stay when the hooks are taken out:
//! they are the agent's, not Relay's.
//!
//! Nothing is written but on a yes in `rl setup`. The file stays the
//! user's: Relay's entries are appended, never anything removed or moved
//! but Relay's own (`is_relays`), and a file that does not read is never
//! rewritten.

use crate::sessions::UserHooks;
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// Claude.
pub const CLAUDE: &str = crate::claude::DIALECT;
/// Codex, whose own `config.toml` may hold hooks too (`inline_hooks`).
pub const CODEX: &str = "codex";

/// The word after the event in a marked hook: that copy, not a run's own
/// (`claude::settings_json`).
pub const USER: &str = crate::sessions::MARKER;

/// Set in the environment of the `claude` that Relay itself runs
/// (`claude agents`, `claude stop`): should that fire a user-level hook,
/// `rl hook` says nothing of it. Otherwise each ask could tell of a new
/// session, which calls for another ask, and so on. Its value is the Unix
/// time, in seconds, it was set (`asking_since`): a long-lived process the
/// `claude` leaves behind (a background supervisor) inherits it, and its
/// later sessions' hooks must not be silenced for good.
pub const ASKING: &str = "RELAY_ASKING";

/// How long after `ASKING` was set a hook still takes it for an ask's.
pub const ASKING_FOR: u64 = 30;

/// How far ahead of `now` a time may be and still be an ask's: a clock a
/// little behind. Any further is no ask's.
pub const ASKING_SKEW: u64 = 5;

/// The time now, in Unix seconds.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What `ASKING` is set to: the time now, in Unix seconds.
pub fn asking_since() -> String {
    now_secs().to_string()
}

/// Whether `ASKING`'s `value` (None where it is not set) is that of an ask
/// going on at `now`, in Unix seconds: a time under `ASKING_FOR` old, and
/// not more than `ASKING_SKEW` ahead. A value that is no time is not.
pub fn is_asking(value: Option<&str>, now: u64) -> bool {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .is_some_and(|set| set <= now + ASKING_SKEW && now.saturating_sub(set) < ASKING_FOR)
}

/// The command an event's hook runs: with the agent's marker after the
/// event where it has one (`USER`).
///
/// `|| true`: whatever `rl` does — gone (127), one that refuses these
/// words (clap's 2) — the hook succeeds. Gemini takes exit 2 as
/// "block", Claude too on some events, and codex warns on any failure;
/// Relay's hook never does any of it.
pub fn command(rl_bin: &Path, hooks: &UserHooks, event: &str) -> String {
    let rl = rl_bin.to_string_lossy().replace('\'', "'\\''");
    let marker = hooks.marker.as_deref().map(|m| format!(" {m}")).unwrap_or_default();
    format!("'{rl}' hook {} {event}{marker}{NEVER_FAILS}", hooks.agent)
}

/// What every hook command ends with, so it cannot fail.
const NEVER_FAILS: &str = " || true";

/// One event's entry: a matcher-less group with Relay's one command, its
/// timeout in the agent's unit, and `async` where the agent runs its hooks
/// in its background — but not an end's, which the agent might not wait
/// for as it exits; `rl hook` is done in milliseconds anyway.
fn entry(hooks: &UserHooks, event: &str, command: String) -> Value {
    let mut hook = Map::new();
    hook.insert("type".into(), json!("command"));
    set(&mut hook, hooks, event, command);
    json!({ "hooks": [hook] })
}

/// Sets what Relay writes of its hook on `hook`: the command, the timeout,
/// and `async` where it runs in the agent's background (`entry`), taken
/// out where it does not. Anything else on it is left.
fn set(hook: &mut Map<String, Value>, hooks: &UserHooks, event: &str, command: String) {
    hook.insert("command".into(), json!(command));
    hook.insert("timeout".into(), json!(hooks.timeout));
    if hooks.background && !hooks.ends(event) {
        hook.insert("async".into(), json!(true));
    } else {
        hook.shift_remove("async");
    }
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

/// Whether `command` is one of Relay's hooks for the agent: `rl hook
/// <agent> <Event>`, by whatever path `rl` was installed at — with the
/// agent's marker after it where it has one, as a run's own
/// (`claude::settings_json`) has no place in the user's file, and the
/// `|| true` every one of Relay's ends with.
pub fn is_relays(command: &str, hooks: &UserHooks) -> bool {
    program(command, hooks).is_some()
}

/// The `rl` a command of Relay's for the agent runs (`is_relays`), as the
/// shell reads it: `/opt/relay/bin/rl`, or `rl` alone. None for any other
/// command.
fn program(command: &str, hooks: &UserHooks) -> Option<String> {
    let mut words: Vec<&str> = command.split_whitespace().collect();
    if !words.ends_with(&["||", "true"]) {
        return None;
    }
    words.truncate(words.len() - 2);
    if let Some(marker) = &hooks.marker {
        if words.last() != Some(&marker.as_str()) {
            return None;
        }
        words.pop();
    }
    let n = words.len();
    if n < 4 || words[n - 3] != "hook" || words[n - 2] != hooks.agent {
        return None;
    }
    let program = words[..n - 3].join(" ");
    let program = program.trim_matches(|c| c == '\'' || c == '"').replace("'\\''", "'");
    (program.rsplit('/').next() == Some("rl")).then_some(program)
}

/// Whether a command's `rl` would run: `rl` alone, found on the agent's
/// `PATH` as far as Relay can tell; or a whole path to a file that may be
/// run.
fn runs(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    program == "rl"
        || (Path::new(program).is_absolute()
            && std::fs::metadata(program).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// The file as it reads: none when it is not there, an error when it is
/// there and is not a JSON object.
pub fn read(path: &Path) -> Result<Option<Map<String, Value>>> {
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

/// Each hook in an event's list of groups, mutably.
fn hook_objects(groups: &mut [Value]) -> impl Iterator<Item = &mut Map<String, Value>> {
    groups
        .iter_mut()
        .filter_map(|g| g.get_mut("hooks").and_then(Value::as_array_mut))
        .flatten()
        .filter_map(Value::as_object_mut)
}

/// Whether Relay's hooks are in the file: any of them. A file that is not
/// there has none; one that does not read is an error.
pub fn installed(path: &Path, hooks: &UserHooks) -> Result<bool> {
    Ok(relays_programs(path, hooks)?.next().is_some())
}

/// Whether Relay's hooks are in the file and would run: any of them whose
/// `rl` is there to run (`runs`). One whose `rl` was moved or taken away
/// fails without a word (`|| true`), and tells of nothing.
pub fn installed_and_runs(path: &Path, hooks: &UserHooks) -> Result<bool> {
    Ok(relays_programs(path, hooks)?.any(|p| runs(&p)))
}

/// The `rl` of each of Relay's hooks in the file.
fn relays_programs(path: &Path, hooks: &UserHooks) -> Result<impl Iterator<Item = String>> {
    let doc = read(path)?.unwrap_or_default();
    let commands: Vec<String> = match doc.get("hooks") {
        Some(Value::Object(events)) => events
            .values()
            .filter_map(Value::as_array)
            .flatten()
            .filter_map(|g| g.get("hooks").and_then(Value::as_array))
            .flatten()
            .filter_map(|h| h.get("command").and_then(Value::as_str))
            .map(String::from)
            .collect(),
        _ => vec![],
    };
    let hooks = hooks.clone();
    Ok(commands.into_iter().filter_map(move |c| program(&c, &hooks)))
}

/// Adds Relay's hook to each of the agent's events in `path`, creating the
/// file where it is not there. An event that has Relay's already keeps it,
/// brought up to date: pointed at this `rl`, with the timeout and `async`
/// the profile says now. Nothing else is touched. True when the file
/// changed.
pub fn install(path: &Path, hooks: &UserHooks, rl_bin: &Path) -> Result<bool> {
    let Some(doc) = installed_doc(path, hooks, rl_bin)? else {
        return Ok(false);
    };
    write(path, &doc)?;
    Ok(true)
}

/// Whether `install` would change the file, without writing it: what
/// `rl setup --check` asks. An error where `install` would refuse.
pub fn install_needed(path: &Path, hooks: &UserHooks, rl_bin: &Path) -> Result<bool> {
    Ok(installed_doc(path, hooks, rl_bin)?.is_some())
}

/// The `rl` of each of Relay's hooks in the file, as the shell reads it
/// (`/opt/relay/bin/rl`, or `rl` alone).
pub fn programs(path: &Path, hooks: &UserHooks) -> Result<Vec<String>> {
    Ok(relays_programs(path, hooks)?.collect())
}

/// The file as `install` leaves it, where that is not as it is: None when
/// nothing would change.
fn installed_doc(path: &Path, hooks: &UserHooks, rl_bin: &Path) -> Result<Option<Map<String, Value>>> {
    let mut doc = read(path)?.unwrap_or_default();
    let before = doc.clone();
    let events = hooks_of(&mut doc, path)?;
    for event in hooks.event_names() {
        let cmd = command(rl_bin, hooks, event);
        let groups = match events.entry(event).or_insert_with(|| Value::Array(vec![])) {
            Value::Array(g) => g,
            _ => bail!("{}: `hooks.{event}` is not a list; left as it was", path.display()),
        };
        let mut found = false;
        for h in hook_objects(groups) {
            if h.get("command")
                .and_then(Value::as_str)
                .is_some_and(|c| is_relays(c, hooks))
            {
                set(h, hooks, event, cmd.clone());
                found = true;
            }
        }
        if !found {
            groups.push(entry(hooks, event, cmd));
        }
    }
    for (key, value) in &hooks.settings {
        settle(&mut doc, key, value);
    }
    Ok((doc != before).then_some(doc))
}

/// The profile's settings the file has no value for yet, by their dotted
/// paths: what an install would set. All of them for a file that is not
/// there yet; an error for one that does not read.
pub fn unset<'a>(path: &Path, hooks: &'a UserHooks) -> Result<Vec<&'a str>> {
    let doc = read(path)?.unwrap_or_default();
    Ok(hooks
        .settings
        .iter()
        .filter(|(key, _)| at(&doc, key).is_none())
        .map(|(key, _)| key.as_str())
        .collect())
}

/// The value at a dotted path, if every step on the way is an object.
fn at<'a>(doc: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    let mut steps = key.split('.');
    let mut here = doc.get(steps.next()?)?;
    for step in steps {
        here = here.as_object()?.get(step)?;
    }
    Some(here)
}

/// Sets `value` at the dotted path `key` where nothing is there, making
/// the objects on the way. A value already there is the user's and is left,
/// as is a path that runs into something not an object.
fn settle(doc: &mut Map<String, Value>, key: &str, value: &Value) {
    let steps: Vec<&str> = key.split('.').collect();
    let (last, way) = steps.split_last().expect("a path has a step");
    let mut here = doc;
    for step in way {
        match here.entry(*step).or_insert_with(|| Value::Object(Map::new())) {
            Value::Object(m) => here = m,
            _ => return,
        }
    }
    here.entry(*last).or_insert_with(|| value.clone());
}

/// Takes Relay's hooks for the agent out of `path`: only commands that are
/// Relay's, and a group or an event's list left empty by that. True when
/// the file changed; one not there is left not there.
pub fn uninstall(path: &Path, hooks: &UserHooks) -> Result<bool> {
    let Some(mut doc) = read(path)? else { return Ok(false) };
    let Some(Value::Object(events)) = doc.get_mut("hooks") else {
        return Ok(false);
    };
    let mut changed = false;
    let relays = |h: &Value| {
        h.get("command")
            .and_then(Value::as_str)
            .is_some_and(|c| is_relays(c, hooks))
    };
    events.retain(|_, groups| {
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
    if events.is_empty() {
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
pub fn write(path: &Path, doc: &Map<String, Value>) -> Result<()> {
    write_text(path, &(serde_json::to_string_pretty(doc)? + "\n"))
}

/// `write` for text that is not JSON (Codex's `config.toml`).
pub fn write_text(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = target(path);
    let dir = path.parent().context("a file with no folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
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
    use crate::Registry;

    const RL: &str = "/opt/relay/bin/rl";

    fn doc(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// An agent's hooks, as its built-in profile has them.
    fn h(agent: &str) -> UserHooks {
        Registry::builtin().user_hooks(agent).unwrap().clone()
    }

    fn names(agent: &str) -> Vec<String> {
        h(agent).event_names().map(String::from).collect()
    }

    #[test]
    fn only_a_fresh_time_is_an_ask_going_on() {
        assert!(is_asking(Some("1000"), 1000));
        assert!(is_asking(Some("1000"), 1029));
        assert!(!is_asking(Some("1000"), 1030));
        assert!(!is_asking(Some("1000"), 5000));
        assert!(is_asking(Some("1000"), 999), "a clock a little behind");
        assert!(is_asking(Some("1005"), 1000));
        assert!(!is_asking(Some("1006"), 1000), "ahead by more than a skew");
        assert!(!is_asking(Some("99999999999"), 1000), "a time far ahead never silences");
        assert!(!is_asking(Some("1"), 1_800_000_000), "the old value is a time long ago");
        assert!(!is_asking(Some("yes"), 1000));
        assert!(!is_asking(Some(""), 1000));
        assert!(!is_asking(None, 1000));
        assert!(is_asking(Some(&asking_since()), asking_since().parse().unwrap()));
    }

    #[test]
    fn codex_installs_into_an_empty_file_one_hook_per_event() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("codex-home/hooks.json");
        assert!(!installed(&path, &h(CODEX)).unwrap());
        assert!(install(&path, &h(CODEX), Path::new(RL)).unwrap());
        let v = doc(&path);
        let hooks = v["hooks"].as_object().unwrap();
        assert_eq!(hooks.keys().cloned().collect::<Vec<_>>(), names(CODEX));
        assert_eq!(
            hooks["PermissionRequest"],
            json!([{"hooks": [{"type": "command", "command": "'/opt/relay/bin/rl' hook codex PermissionRequest || true",
                               "timeout": 3, "async": true}]}])
        );
        assert!(installed(&path, &h(CODEX)).unwrap());
        assert!(
            !installed(&path, &h("gemini")).unwrap(),
            "another agent's are not these"
        );
    }

    #[test]
    fn install_needed_says_what_install_would_do_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("codex-home/hooks.json");
        let codex = h(CODEX);
        assert!(install_needed(&path, &codex, Path::new(RL)).unwrap());
        assert!(!path.exists() && !path.parent().unwrap().exists(), "nothing made");
        install(&path, &codex, Path::new(RL)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!install_needed(&path, &codex, Path::new(RL)).unwrap());
        assert_eq!(programs(&path, &codex).unwrap().first().map(String::as_str), Some(RL));
        // `rl` moved: an install would point them at the new one.
        assert!(install_needed(&path, &codex, Path::new("/new/rl")).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "still not written");
        // Where install refuses, so does the question.
        std::fs::write(&path, "{ nope").unwrap();
        assert!(install_needed(&path, &codex, Path::new(RL)).is_err());
    }

    #[test]
    fn gemini_keeps_every_other_key_and_hook_in_order_and_never_twice() {
        let gemini = h("gemini");
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
        assert!(install(&path, &gemini, Path::new("/opt/my tools/rl")).unwrap());
        let v = doc(&path);
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["theme", "hooks", "general", "ui"],
            "nothing moved; its setting last"
        );
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
        assert!(!install(&path, &gemini, Path::new("/opt/my tools/rl")).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "twice: no duplicate");
        // `rl` moved: its hooks follow, still once each.
        assert!(install(&path, &gemini, Path::new(RL)).unwrap());
        let v = doc(&path);
        let after = v["hooks"]["AfterAgent"].as_array().unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(
            after[1]["hooks"][0]["command"],
            "'/opt/relay/bin/rl' hook gemini AfterAgent || true"
        );
        // One with no `|| true` is not Relay's: left alone, ours added.
        let mut v = doc(&path);
        v["hooks"]["AfterAgent"][1]["hooks"][0]["command"] = json!("/opt/relay/bin/rl hook gemini AfterAgent");
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        assert!(!is_relays("/opt/relay/bin/rl hook gemini AfterAgent", &gemini));
        assert!(install(&path, &gemini, Path::new(RL)).unwrap());
        let after = doc(&path)["hooks"]["AfterAgent"].as_array().unwrap().clone();
        assert_eq!(after.len(), 3);
        assert_eq!(
            after[1]["hooks"][0]["command"],
            "/opt/relay/bin/rl hook gemini AfterAgent"
        );
        assert_eq!(
            after[2]["hooks"][0]["command"],
            "'/opt/relay/bin/rl' hook gemini AfterAgent || true"
        );
    }

    #[test]
    fn a_hook_command_never_fails_whatever_rl_does() {
        let gemini = h("gemini");
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
        let gone = command(&tmp.path().join("no such/rl"), &gemini, "AfterTool");
        assert_eq!(run(&gone), Some(0), "{gone}");
        // An older `rl` that refuses these words with clap's 2: still 0.
        std::fs::create_dir_all(tmp.path().join("it's")).unwrap();
        let old = tmp.path().join("it's/rl");
        std::fs::write(&old, "#!/bin/sh\nexit 2\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cmd = command(&old, &gemini, "AfterTool");
        assert_eq!(run(&cmd), Some(0), "{cmd}");
        assert!(is_relays(&cmd, &gemini), "a quote in the path is still Relay's: {cmd}");
    }

    #[test]
    fn uninstall_takes_only_relays_out() {
        let codex = h(CODEX);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hooks.json");
        std::fs::write(
            &path,
            r#"{"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}], "PreCompact": []}, "x": 1}"#,
        )
        .unwrap();
        install(&path, &codex, Path::new(RL)).unwrap();
        // One of Relay's shares a group with the user's own.
        let mut v = doc(&path);
        v["hooks"]["Stop"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type": "command", "command": "rl hook codex Stop || true"}));
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        assert!(uninstall(&path, &codex).unwrap());
        assert_eq!(
            doc(&path),
            json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}], "PreCompact": []}, "x": 1})
        );
        assert!(!uninstall(&path, &codex).unwrap(), "nothing of Relay's left");
        let gone = tmp.path().join("nothing.json");
        assert!(!uninstall(&gone, &codex).unwrap());
        assert!(!gone.exists(), "a file not there stays not there");
        // Relay's alone: the file is left with no hooks key at all.
        let only = tmp.path().join("only.json");
        install(&only, &codex, Path::new(RL)).unwrap();
        uninstall(&only, &codex).unwrap();
        assert_eq!(doc(&only), json!({}));
    }

    #[test]
    fn a_file_that_does_not_read_is_never_rewritten() {
        let gemini = h("gemini");
        let tmp = tempfile::tempdir().unwrap();
        for text in [
            "{ \"hooks\": { // mine\n } }",
            "[1, 2]",
            r#"{"hooks": []}"#,
            r#"{"hooks": {"AfterAgent": {"not": "a list"}}}"#,
        ] {
            let path = tmp.path().join("settings.json");
            std::fs::write(&path, text).unwrap();
            let e = install(&path, &gemini, Path::new(RL)).unwrap_err();
            assert!(format!("{e:#}").contains("left as it was"), "{text}: {e:#}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, "{ nope").unwrap();
        assert!(installed(&path, &gemini).is_err());
        assert!(uninstall(&path, &gemini).is_err());
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
        install(&link, &h(CODEX), Path::new(RL)).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(installed(&real, &h(CODEX)).unwrap());
    }

    #[test]
    fn an_install_brings_the_timeout_and_async_up_to_date_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hooks.json");
        install(&path, &h(CODEX), Path::new(RL)).unwrap();
        // Written by a Relay whose profile said otherwise, and touched by
        // the user since.
        let mut v = doc(&path);
        v["hooks"]["Stop"][0]["hooks"][0]["timeout"] = json!(10);
        v["hooks"]["Stop"][0]["hooks"][0]["statusMessage"] = json!("mine");
        v["hooks"]["SessionEnd"][0]["hooks"][0]["async"] = json!(true);
        v["hooks"]["Interrupt"][0]["hooks"][0]
            .as_object_mut()
            .unwrap()
            .remove("async");
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        assert!(install(&path, &h(CODEX), Path::new(RL)).unwrap());
        let v = doc(&path);
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0],
            json!({"type": "command", "command": "'/opt/relay/bin/rl' hook codex Stop || true",
                   "timeout": 3, "async": true, "statusMessage": "mine"})
        );
        assert_eq!(v["hooks"]["SessionEnd"][0]["hooks"][0].get("async"), None);
        assert_eq!(v["hooks"]["Interrupt"][0]["hooks"][0]["async"], true);
        assert!(!install(&path, &h(CODEX), Path::new(RL)).unwrap(), "up to date now");
    }

    #[test]
    fn a_hook_whose_rl_is_gone_is_there_but_does_not_run() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        let rl = tmp.path().join("it's here/rl");
        std::fs::create_dir_all(rl.parent().unwrap()).unwrap();
        std::fs::write(&rl, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&rl, std::fs::Permissions::from_mode(0o755)).unwrap();
        let claude = h(CLAUDE);
        install(&path, &claude, &rl).unwrap();
        assert!(installed(&path, &claude).unwrap() && installed_and_runs(&path, &claude).unwrap());
        // Not to be run: there, but not running.
        std::fs::set_permissions(&rl, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!installed_and_runs(&path, &claude).unwrap());
        // Gone: the same.
        std::fs::remove_file(&rl).unwrap();
        assert!(installed(&path, &claude).unwrap(), "still in the file");
        assert!(!installed_and_runs(&path, &claude).unwrap());
        // `rl` alone, found on the agent's PATH as far as Relay knows.
        install(&path, &claude, Path::new("rl")).unwrap();
        assert!(installed_and_runs(&path, &claude).unwrap());
        assert!(!installed_and_runs(&tmp.path().join("none.json"), &claude).unwrap());
    }

    #[test]
    fn relays_commands_are_known_by_their_words() {
        let (codex, gemini) = (h(CODEX), h("gemini"));
        assert!(!is_relays("/usr/local/bin/rl hook codex Stop", &codex), "no `|| true`");
        assert!(is_relays("/usr/local/bin/rl hook codex Stop || true", &codex));
        assert!(is_relays("'/opt/my tools/rl' hook gemini AfterAgent || true", &gemini));
        assert!(is_relays("'/usr/local/bin/rl' hook codex Stop ||   true  ", &codex));
        assert!(!is_relays("'/usr/local/bin/rl' hook codex || true", &codex));
        assert!(is_relays("rl hook codex Whatever || true", &codex));
        assert!(!is_relays("/usr/local/bin/rl hook gemini Stop", &codex));
        assert!(!is_relays("/bin/curl hook codex Stop", &codex));
        assert!(!is_relays("rl hook codex", &codex));
        assert!(!is_relays("say done", &codex));
    }

    #[test]
    fn taking_the_hooks_key_out_keeps_the_order_of_the_rest() {
        let gemini = h("gemini");
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, r#"{"theme": "x", "general": {}, "ui": 1}"#).unwrap();
        install(&path, &gemini, Path::new(RL)).unwrap();
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
        uninstall(&path, &gemini).unwrap();
        let keys: Vec<String> = doc(&path).as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["theme", "keep", "general", "ui"]);
    }

    #[test]
    fn a_write_keeps_the_files_permissions_and_a_new_one_is_the_users_alone() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let fresh = tmp.path().join("new/hooks.json");
        install(&fresh, &h(CODEX), Path::new(RL)).unwrap();
        assert_eq!(mode(&fresh), 0o600);
        let shared = tmp.path().join("settings.json");
        std::fs::write(&shared, "{}").unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o640)).unwrap();
        install(&shared, &h("gemini"), Path::new(RL)).unwrap();
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
        install(&link, &h(CODEX), Path::new(RL)).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(installed(&tmp.path().join("dotfiles/hooks.json"), &h(CODEX)).unwrap());
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
        install(&hooks, &h(CODEX), Path::new(RL)).unwrap();
        let v = doc(&hooks);
        assert_eq!(v["hooks"]["SessionEnd"][0]["hooks"][0].get("async"), None);
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["async"], true);
    }

    #[test]
    fn claude_installs_into_an_empty_file_as_its_user_level_copy() {
        let claude = h(CLAUDE);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("claude-config/settings.json");
        assert!(install(&path, &claude, Path::new(RL)).unwrap());
        let v = doc(&path);
        let hooks = v["hooks"].as_object().unwrap();
        assert_eq!(hooks.keys().cloned().collect::<Vec<_>>(), names(CLAUDE));
        assert_eq!(
            hooks["Notification"],
            json!([{"hooks": [{"type": "command",
                               "command": "'/opt/relay/bin/rl' hook claude Notification --user || true",
                               "timeout": 5}]}])
        );
        assert!(installed(&path, &claude).unwrap());
        assert!(!installed(&path, &h(CODEX)).unwrap());
    }

    #[test]
    fn claude_keeps_every_other_key_and_hook_in_order_and_never_twice() {
        let claude = h(CLAUDE);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        let mine = r#"{
  "model": "opus",
  "hooks": {
    "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]}],
    "Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]
  },
  "permissions": {"allow": ["Bash(ls)"]}
}"#;
        std::fs::write(&path, mine).unwrap();
        assert!(install(&path, &claude, Path::new(RL)).unwrap());
        let v = doc(&path);
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["model", "hooks", "permissions"], "nothing moved");
        let events: Vec<&str> = v["hooks"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            events,
            [
                "PreToolUse",
                "Stop",
                "SessionStart",
                "UserPromptSubmit",
                "Notification",
                "PostToolUse",
                "SessionEnd"
            ],
            "the user's first, in their order"
        );
        assert_eq!(v["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "guard.sh");
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "say done");
        assert_eq!(
            stop[1]["hooks"][0]["command"],
            "'/opt/relay/bin/rl' hook claude Stop --user || true"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!install(&path, &claude, Path::new(RL)).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "twice: no duplicate");

        // A run's own hook, set there by hand, is not Relay's user-level one:
        // left, and not taken for it.
        let mut v = doc(&path);
        v["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .push(json!({"hooks": [{"type": "command", "command": "/opt/relay/bin/rl hook claude Stop"}]}));
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        assert!(uninstall(&path, &claude).unwrap());
        let v = doc(&path);
        assert_eq!(
            v["hooks"],
            json!({
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]}],
                "Stop": [{"hooks": [{"type": "command", "command": "say done"}]},
                         {"hooks": [{"type": "command", "command": "/opt/relay/bin/rl hook claude Stop"}]}]
            })
        );
        assert_eq!(v["permissions"], json!({"allow": ["Bash(ls)"]}));

        // One that does not read is refused, and left as it was.
        std::fs::write(&path, "{ \"hooks\": ").unwrap();
        assert!(install(&path, &claude, Path::new(RL)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ \"hooks\": ");
    }

    #[test]
    fn claudes_user_level_hook_is_known_by_its_user_word() {
        let (claude, codex) = (h(CLAUDE), h(CODEX));
        assert!(is_relays("'/opt/my tools/rl' hook claude Stop --user || true", &claude));
        assert!(is_relays(
            "/usr/local/bin/rl hook claude Notification --user || true",
            &claude
        ));
        assert!(!is_relays("/usr/local/bin/rl hook claude Stop", &claude), "a run's own");
        assert!(!is_relays("/usr/local/bin/rl hook claude --user || true", &claude));
        assert!(!is_relays("/usr/local/bin/rl hook codex Stop --user || true", &claude));
        assert!(!is_relays("/usr/local/bin/rl hook claude Stop --user || true", &codex));
        assert_eq!(
            command(Path::new("/x/it's/rl"), &claude, "Stop"),
            "'/x/it'\\''s/rl' hook claude Stop --user || true"
        );
        assert_eq!(
            command(Path::new("/x/rl"), &codex, "Stop"),
            "'/x/rl' hook codex Stop || true"
        );
    }

    #[test]
    fn an_install_sets_the_agents_own_settings_where_the_user_has_none() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(".gemini/settings.json");
        let gemini = h("gemini");
        assert_eq!(unset(&file, &gemini).unwrap(), ["ui.useAlternateBuffer"]);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"ui": {"theme": "Default"}}"#).unwrap();
        assert!(install(&file, &gemini, Path::new(RL)).unwrap());
        assert_eq!(
            doc(&file)["ui"],
            json!({"theme": "Default", "useAlternateBuffer": true})
        );
        assert!(unset(&file, &gemini).unwrap().is_empty());
        assert!(!install(&file, &gemini, Path::new(RL)).unwrap(), "nothing more to do");
        // Taking the hooks out leaves the setting: it is gemini's.
        assert!(uninstall(&file, &gemini).unwrap());
        assert_eq!(doc(&file)["ui"]["useAlternateBuffer"], json!(true));
        assert!(doc(&file).get("hooks").is_none());
    }

    #[test]
    fn a_setting_the_user_has_is_left_as_it_is() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("settings.json");
        let gemini = h("gemini");
        std::fs::write(&file, r#"{"ui": {"useAlternateBuffer": false}}"#).unwrap();
        assert!(unset(&file, &gemini).unwrap().is_empty());
        install(&file, &gemini, Path::new(RL)).unwrap();
        assert_eq!(doc(&file)["ui"]["useAlternateBuffer"], json!(false));
        // A path that runs into something else is not forced through.
        std::fs::write(&file, r#"{"ui": "plain"}"#).unwrap();
        assert_eq!(unset(&file, &gemini).unwrap(), ["ui.useAlternateBuffer"]);
        install(&file, &gemini, Path::new(RL)).unwrap();
        assert_eq!(doc(&file)["ui"], json!("plain"));
        assert!(installed(&file, &gemini).unwrap(), "the hooks went in all the same");
    }
}
