//! A run comes to rest by its agent's hook: its turn ends, or it waits on
//! your approval. Each such hook queues it for review, as asking or finished
//! (`rest::classify`), by what the agent last said.

use crate::server::Daemon;
use crate::transcript::{self, Agent};
use crate::{rest, summary};
use anyhow::Result;
use rl_core::time;
use rl_ledger::review_queue;
use rl_proto::queue::{Hint, How, RestKind};
use rl_proto::types::Run;
use serde_json::Value;

/// How a hook says a run came to rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookRest {
    /// The turn is over; the agent's final message, when the hook carries it.
    TurnEnd(Option<String>),
    /// It waits on your approval, or on an answer in a dialog.
    Permission,
}

/// Whether `event`, from an agent of `dialect`, brings its run to rest.
/// Claude's `idle_prompt` (a minute after `Stop`, with nothing new) and a
/// turn you interrupted do not.
pub fn hook_rest(dialect: &str, event: &str, payload: &Value) -> Option<HookRest> {
    let field = |k: &str| payload.get(k).and_then(Value::as_str);
    let said = |k: &str| field(k).filter(|m| !m.trim().is_empty()).map(str::to_string);
    match (dialect, event) {
        ("claude" | "codex", "Stop") => Some(HookRest::TurnEnd(said("last_assistant_message"))),
        ("gemini", "AfterAgent") => Some(HookRest::TurnEnd(said("prompt_response"))),
        ("claude", "Notification")
            if matches!(
                field("notification_type"),
                Some("permission_prompt" | "elicitation_dialog")
            ) =>
        {
            Some(HookRest::Permission)
        }
        ("codex", "PermissionRequest") => Some(HookRest::Permission),
        ("gemini", "Notification") if field("notification_type") == Some("ToolPermission") => {
            Some(HookRest::Permission)
        }
        _ => None,
    }
}

/// Queues `run` at its rest. A turn's end whose hook carries no message is
/// told by the last one in the run's log.
pub async fn record(d: &Daemon, run: &Run, rest: HookRest) -> Result<()> {
    let (kind, how, hints) = match rest {
        HookRest::Permission => (RestKind::Asks, How::Permission, vec![Hint::Permission]),
        HookRest::TurnEnd(said) => {
            let said = match said {
                Some(m) => Some(m),
                None => last_in_log(d, run).await?,
            };
            let (kind, hints) = rest::classify(said.as_deref());
            (kind, How::Reply, hints)
        }
    };
    let (id, start, head) = (run.id.clone(), run.started_at, run.start_head.clone());
    d.ledger
        .call(move |c| review_queue::rest(c, "run", &id, kind, how, &hints, time::now(), start, head.as_deref()))
        .await
}

/// The agent's last message in the run's log; `None` with no log to read.
async fn last_in_log(d: &Daemon, run: &Run) -> Result<Option<String>> {
    let Some(agent) = Agent::of(run) else { return Ok(None) };
    let Some(file) = summary::locate(d, run).await? else {
        return Ok(None);
    };
    let text = tokio::task::spawn_blocking(move || std::fs::read_to_string(file)).await?;
    Ok(text.ok().and_then(|t| transcript::last_message(agent, &t)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hook_rest_maps_each_agent() {
        use HookRest::{Permission, TurnEnd};
        let said = |m: &str| Some(TurnEnd(Some(m.to_string())));
        let cases = [
            (
                "claude",
                "Stop",
                json!({ "last_assistant_message": "Done." }),
                said("Done."),
            ),
            ("claude", "Stop", json!({}), Some(TurnEnd(None))),
            (
                "claude",
                "Stop",
                json!({ "last_assistant_message": " " }),
                Some(TurnEnd(None)),
            ),
            ("codex", "Stop", json!({ "last_assistant_message": "Ok?" }), said("Ok?")),
            (
                "gemini",
                "AfterAgent",
                json!({ "prompt_response": "Fixed." }),
                said("Fixed."),
            ),
            ("gemini", "AfterAgent", json!({}), Some(TurnEnd(None))),
            (
                "claude",
                "Notification",
                json!({ "notification_type": "permission_prompt" }),
                Some(Permission),
            ),
            (
                "claude",
                "Notification",
                json!({ "notification_type": "elicitation_dialog" }),
                Some(Permission),
            ),
            (
                "codex",
                "PermissionRequest",
                json!({ "tool_name": "shell" }),
                Some(Permission),
            ),
            (
                "gemini",
                "Notification",
                json!({ "notification_type": "ToolPermission" }),
                Some(Permission),
            ),
            (
                "claude",
                "Notification",
                json!({ "notification_type": "idle_prompt" }),
                None,
            ),
            ("codex", "Interrupt", json!({}), None),
            ("claude", "PostToolUse", json!({ "tool_name": "Bash" }), None),
            ("codex", "PostToolUse", json!({ "tool_name": "shell" }), None),
            ("gemini", "AfterTool", json!({}), None),
            ("gemini", "Stop", json!({}), None),
        ];
        for (dialect, event, payload, want) in cases {
            assert_eq!(hook_rest(dialect, event, &payload), want, "{dialect} {event} {payload}");
        }
    }
}
