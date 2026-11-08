//! Claude Code: the hooks of a session Relay holds.
//!
//! `rl new` writes a settings file with one command hook per event, all
//! calling `rl hook claude <Event>` (rewritten on every start); the `claude` profile passes it as `--settings`, so
//! every Claude session Relay holds reports back, in any directory, without
//! editing the project's `.claude/settings.json`. `rl hook` reads the hook's
//! JSON on stdin and writes the session's state file.

use anyhow::{Context, Result};
use rl_core::Home;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const DIALECT: &str = "claude";

/// Hook events Relay subscribes to. `SessionEnd` is not needed: the process
/// exit right after it is the exact signal.
pub const EVENTS: [&str; 6] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
];

/// Seconds Claude waits for a hook. `rl hook` is done well before this.
pub(crate) const HOOK_TIMEOUT_SECS: u32 = 5;

const DETAIL_MAX: usize = 160;

pub fn settings_path(home: &Home) -> PathBuf {
    home.hooks_dir().join("claude.json")
}

/// The settings document: one `command` hook per event.
pub fn settings_json(rl_bin: &Path) -> Value {
    let rl = shell_quote(&rl_bin.to_string_lossy());
    let hooks: serde_json::Map<String, Value> = EVENTS
        .iter()
        .map(|ev| {
            let entry = json!([{
                "hooks": [{
                    "type": "command",
                    "command": format!("{rl} hook {DIALECT} {ev}"),
                    "timeout": HOOK_TIMEOUT_SECS,
                }]
            }]);
            (ev.to_string(), entry)
        })
        .collect();
    json!({ "hooks": hooks })
}

/// Writes (or refreshes) the settings file. Returns its path.
pub fn write_settings(home: &Home, rl_bin: &Path) -> Result<PathBuf> {
    let path = settings_path(home);
    let text = serde_json::to_string_pretty(&settings_json(rl_bin))? + "\n";
    if std::fs::read_to_string(&path).ok().as_deref() != Some(&text) {
        std::fs::create_dir_all(path.parent().unwrap())?;
        rl_core::atomic::write(&path, text.as_bytes()).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(path)
}

pub(crate) fn clip(s: &str) -> String {
    let one_line = s.lines().next().unwrap_or("").trim();
    if one_line.chars().count() <= DETAIL_MAX {
        one_line.to_string()
    } else {
        let cut: String = one_line.chars().take(DETAIL_MAX - 1).collect();
        format!("{cut}…")
    }
}

pub(crate) fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@%".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_have_one_hook_per_event() {
        let v = settings_json(Path::new("/opt/my tools/rl"));
        let hooks = v["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), EVENTS.len());
        assert_eq!(
            hooks["Notification"][0]["hooks"][0]["command"],
            "'/opt/my tools/rl' hook claude Notification"
        );
        assert_eq!(hooks["Stop"][0]["hooks"][0]["timeout"], HOOK_TIMEOUT_SECS);
        assert_eq!(
            settings_json(Path::new("/usr/local/bin/rl"))["hooks"]["Stop"][0]["hooks"][0]["command"],
            "/usr/local/bin/rl hook claude Stop"
        );
    }

    #[test]
    fn write_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path());
        let p = write_settings(&home, Path::new("/x/rl")).unwrap();
        let m1 = std::fs::metadata(&p).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_settings(&home, Path::new("/x/rl")).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), m1);
        write_settings(&home, Path::new("/y/rl")).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("/y/rl hook"));
    }
}
