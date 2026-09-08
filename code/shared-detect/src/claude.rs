//! Claude Code: exact signals from its hooks.
//!
//! `rld` writes a settings file with one command hook per event, all calling
//! `rl hook <Event>`; the `claude` profile passes it as `--settings`, so every
//! Claude run under Relay reports back, in any directory, without editing the
//! project's `.claude/settings.json`. `rl hook` reads the hook's JSON on stdin
//! and posts it to the daemon with `RELAY_RUN` from its environment.

use crate::Signal;
use anyhow::{Context, Result};
use relay_core::Home;
use relay_proto::types::RunState;
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

/// Seconds Claude waits for a hook. `rl hook` gives up on the daemon well
/// before this.
const HOOK_TIMEOUT_SECS: u32 = 5;

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
                    "command": format!("{rl} hook {ev}"),
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
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(path)
}

/// What a hook event says about the run. `None` for events that carry no
/// state (unknown kinds, informational notifications).
pub fn signal(event: &str, payload: &Value) -> Option<Signal> {
    let s = |k: &str| payload.get(k).and_then(Value::as_str).map(|v| clip(v.trim()));
    Some(match event {
        "SessionStart" => Signal::hook(RunState::Working, Some("session started".into())),
        "UserPromptSubmit" => Signal::hook(RunState::Working, s("prompt").map(|p| format!("prompt: {p}"))),
        "PreToolUse" => Signal::hook(RunState::Working, s("tool_name").map(|t| format!("tool: {t}"))),
        "PostToolUse" => Signal::hook(RunState::Working, s("tool_name").map(|t| format!("tool: {t} done"))),
        "Stop" => Signal::hook(RunState::Idle, Some("finished responding".into())),
        "Notification" => {
            let message = s("message");
            let kind = s("notification_type").unwrap_or_default();
            let lower = message.as_deref().unwrap_or("").to_ascii_lowercase();
            match kind.as_str() {
                "permission_prompt" | "elicitation_dialog" => Signal::hook(RunState::NeedsInput, message),
                "idle_prompt" => Signal::hook(RunState::Idle, message),
                "auth_success" => return None,
                // Older Claude Code versions send no type: read the message.
                "" if lower.contains("permission") || lower.contains("approve") => {
                    Signal::hook(RunState::NeedsInput, message)
                }
                "" if lower.contains("waiting for your input") => Signal::hook(RunState::Idle, message),
                _ => return None,
            }
        }
        _ => return None,
    })
}

fn clip(s: &str) -> String {
    let one_line = s.lines().next().unwrap_or("").trim();
    if one_line.chars().count() <= DETAIL_MAX {
        one_line.to_string()
    } else {
        let cut: String = one_line.chars().take(DETAIL_MAX - 1).collect();
        format!("{cut}…")
    }
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@%".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_proto::types::{Confidence, Source};

    #[test]
    fn settings_have_one_hook_per_event() {
        let v = settings_json(Path::new("/opt/my tools/rl"));
        let hooks = v["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), EVENTS.len());
        assert_eq!(
            hooks["Notification"][0]["hooks"][0]["command"],
            "'/opt/my tools/rl' hook Notification"
        );
        assert_eq!(hooks["Stop"][0]["hooks"][0]["timeout"], HOOK_TIMEOUT_SECS);
        assert_eq!(
            settings_json(Path::new("/usr/local/bin/rl"))["hooks"]["Stop"][0]["hooks"][0]["command"],
            "/usr/local/bin/rl hook Stop"
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

    #[test]
    fn events_map_to_states() {
        let s = signal(
            "Notification",
            &json!({"notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash"}),
        )
        .unwrap();
        assert_eq!(
            (s.state, s.source, s.confidence),
            (RunState::NeedsInput, Source::Hook, Confidence::Exact)
        );
        assert_eq!(s.detail.as_deref(), Some("Claude needs your permission to use Bash"));
        assert_eq!(
            signal("Notification", &json!({"message": "Claude needs your permission"}))
                .unwrap()
                .state,
            RunState::NeedsInput
        );
        assert_eq!(
            signal(
                "Notification",
                &json!({"notification_type": "idle_prompt", "message": "x"})
            )
            .unwrap()
            .state,
            RunState::Idle
        );
        assert!(signal("Notification", &json!({"notification_type": "auth_success"})).is_none());
        assert_eq!(signal("Stop", &json!({})).unwrap().state, RunState::Idle);
        let s = signal("PreToolUse", &json!({"tool_name": "Bash"})).unwrap();
        assert_eq!((s.state, s.detail.as_deref()), (RunState::Working, Some("tool: Bash")));
        let s = signal("UserPromptSubmit", &json!({"prompt": "fix the build\nplease"})).unwrap();
        assert_eq!(s.detail.as_deref(), Some("prompt: fix the build"));
        assert!(signal("PreCompact", &json!({})).is_none());
        assert!(signal("SessionEnd", &json!({})).is_none());
    }
}
