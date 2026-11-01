//! Claude's own sessions, as `claude agents --json --all` lists them (the
//! done ones too, as `claude agents` shows them): the `claude-agents`
//! reader a profile's `sessions.list` names (`lister`), asked of the claude
//! profile's binary. Claude's is the one listing there
//! is today; `/import` asks it too, to move Claude's background sessions
//! over.
//!
//! `rld` spawns no `claude` of its own beyond `claude agents` and `claude
//! stop`, which are not sessions: every other is a run (see `outside`).

use anyhow::{Context, Result};
use jiff::Timestamp;
use rl_detect::Registry;
use rl_proto::methods::Outside;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::debug;

/// The profile whose sessions these are: Claude's, which the parts of
/// Relay that are Claude's alone (its background, `/import`) name.
pub const AGENT: &str = rl_detect::user_hooks::CLAUDE;

/// A line of `claude agents --json`, as Claude writes it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listed {
    session_id: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    cwd: PathBuf,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
    /// Unix milliseconds.
    #[serde(default)]
    started_at: Option<i64>,
    #[serde(default)]
    waiting_for: Option<String>,
}

/// `claude agents --json` as sessions. An entry that does not read — no
/// `sessionId`, no `cwd` — is skipped, not the whole list; values are kept
/// as Claude writes them, ones Relay does not know yet too.
pub(crate) fn parse(text: &str) -> Result<Vec<Outside>> {
    let listed: Vec<serde_json::Value> = serde_json::from_str(text).context("reading `claude agents --json`")?;
    Ok(listed
        .into_iter()
        .filter_map(|v| match serde_json::from_value::<Listed>(v) {
            Ok(l) => Some(l),
            Err(e) => {
                debug!(error = %e, "claude agents: an entry skipped");
                None
            }
        })
        // A folder it is in is a whole path: an empty or relative one
        // would make a group that is no folder.
        .filter(|l| !l.session_id.is_empty() && l.cwd.is_absolute())
        .map(|l| {
            let name = l
                .name
                .filter(|n| !n.trim().is_empty() && !n.starts_with("<<"))
                .unwrap_or_else(|| {
                    l.cwd
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_else(|| l.session_id.clone())
                });
            Outside {
                agent: AGENT.into(),
                session_id: l.session_id,
                id: l.id,
                name,
                cwd: l.cwd,
                kind: l.kind.unwrap_or_default(),
                status: l.status.unwrap_or_default(),
                state: l.state,
                pid: l.pid,
                started_at: l.started_at.and_then(|ms| Timestamp::from_millisecond(ms).ok()),
                waiting_for: l.waiting_for,
                run: None,
                new: false,
                workspace: None,
                state_at: None,
                was: None,
            }
        })
        .collect())
}

/// The claude profile's binary, where it is on this machine.
pub(crate) fn claude_bin(registry: &Registry) -> Result<PathBuf> {
    super::lister::agent_bin(registry, AGENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_agents_json_reads_as_sessions() {
        let text = r#"[
          {"pid":1,"id":"8af077cd","cwd":"/w/app","kind":"background","startedAt":1789789820657,
           "sessionId":"8af077cd-5fe3","name":"asset list","status":"idle","state":"done"},
          {"pid":2,"cwd":"/w/relay","kind":"interactive","startedAt":1789821153884,
           "sessionId":"42fdfb2d","name":"ground work","status":"busy"},
          {"pid":3,"id":"a8b1e728","cwd":"/w/market","kind":"background",
           "sessionId":"0d06b838","name":"<<placeholder>>","status":"busy","state":"working"}
        ]"#;
        let s = parse(text).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(
            (s[0].id.as_deref(), s[0].name.as_str()),
            (Some("8af077cd"), "asset list")
        );
        assert_eq!(
            (s[0].agent.as_str(), s[0].key()),
            ("claude", "o:claude:8af077cd-5fe3".into())
        );
        assert!(!s[0].in_a_terminal() && !s[0].working());
        assert!(s[1].in_a_terminal() && s[1].working());
        assert_eq!(s[2].name, "market", "a placeholder name gives way to the folder's");
        assert!(s[2].working());
    }

    #[test]
    fn the_new_fields_read_and_what_does_not_is_skipped() {
        let text = r#"[
          {"pid":41,"id":"b1","cwd":"/w/app","kind":"background","startedAt":1789789820657,
           "sessionId":"s-one","status":"waiting","waitingFor":"permission prompt","state":"pondering",
           "somethingNew":{"x":1}},
          {"id":"b2","cwd":"/w/app","kind":"background","state":"done"},
          {"cwd":"/w/app","kind":"interactive","sessionId":"","status":"idle"},
          {"sessionId":"s-nocwd","kind":"interactive"},
          {"cwd":"","kind":"interactive","sessionId":"s-emptycwd","status":"idle"},
          {"cwd":"w/rel","kind":"interactive","sessionId":"s-relcwd","status":"idle"},
          "not even an object",
          {"cwd":"/w/gone","kind":"background","sessionId":"s-two","id":"b3","state":"stopped"}
        ]"#;
        let s = parse(text).unwrap();
        assert_eq!(
            s.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(),
            ["s-one", "s-two"],
            "no sessionId, an empty one, no cwd, an empty or relative one, not an object: skipped"
        );
        let one = &s[0];
        assert_eq!(one.pid, Some(41));
        assert_eq!(
            one.started_at,
            Some(Timestamp::from_millisecond(1789789820657).unwrap())
        );
        assert_eq!(one.status, "waiting");
        assert_eq!(one.waiting_for.as_deref(), Some("permission prompt"));
        assert_eq!(
            one.state.as_deref(),
            Some("pondering"),
            "a state not known yet, kept as said"
        );
        let two = &s[1];
        assert_eq!((two.pid, two.started_at, two.status.as_str()), (None, None, ""));
        assert!(parse("{}").is_err(), "not a list at all");
        let k = parse(r#"[{"cwd":"/w","sessionId":"s"}]"#).unwrap().remove(0);
        assert_eq!(k.kind, "", "no kind given, none assumed");
        assert!(k.in_a_terminal(), "no id: nothing to attach to");
    }
}
