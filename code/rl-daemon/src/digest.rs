//! One line per run: where it stands, and whether that is news. Shared by
//! the fleet's LAST column, the since-you-left screen and `rl since`.

use jiff::Timestamp;
use rl_core::time::fmt_duration;
use rl_proto::methods::DigestItem;
use rl_proto::types::{Event, Run, RunState, Signal};

pub fn item(run: Run, last: Option<Event>, joined_by: Option<String>, emitted: Vec<Signal>) -> DigestItem {
    let at = last.as_ref().map(|e| e.at).unwrap_or(run.state_at);
    let summary = summarise(&run, last.as_ref(), &emitted);
    let unseen = unseen(&run, at);
    DigestItem {
        summary,
        at,
        unseen,
        joined_by,
        emitted,
        run,
    }
}

/// The newest thing a run raised, in its own words.
pub fn said(emitted: &[Signal]) -> Option<&str> {
    emitted
        .iter()
        .max_by_key(|s| s.at)
        .map(|s| s.said.as_str())
        .filter(|s| !s.is_empty())
}

/// The question, the exit, the last thing said — in that order of interest.
/// A run that ended well and raised something says what it raised, in its own
/// words: for a probe that is the whole point, and `exit 0` says nothing. One
/// that failed keeps its exit, which is the one worth reading.
pub fn summarise(run: &Run, last: Option<&Event>, emitted: &[Signal]) -> String {
    let detail = last.and_then(|e| {
        e.data
            .as_ref()
            .and_then(|d| d.get("detail"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .or_else(|| (e.kind != "run.started").then(|| e.summary.clone()))
    });
    match run.state {
        RunState::Finished | RunState::Failed | RunState::Stopped => {
            let how = last
                .and_then(|e| e.data.as_ref())
                .and_then(|d| d.get("signal"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| run.exit_code.map(|c| format!("exit {c}")))
                .unwrap_or_else(|| run.state.to_string());
            match said(emitted).filter(|_| run.state == RunState::Finished) {
                Some(s) => s.to_string(),
                None => match run.ended_at {
                    Some(e) => format!("{how} · {}", fmt_duration(e.duration_since(run.started_at))),
                    None => how,
                },
            }
        }
        RunState::NeedsInput | RunState::Blocked | RunState::Stale => detail.unwrap_or_else(|| run.state.to_string()),
        _ => run
            .detail
            .clone()
            .or(detail)
            .or_else(|| run.last_line.clone())
            .unwrap_or_else(|| run.state.to_string()),
    }
}

/// News: an attention state or a finish the user has not looked at since.
pub fn unseen(run: &Run, at: Timestamp) -> bool {
    matches!(
        run.state,
        RunState::NeedsInput | RunState::Blocked | RunState::Failed | RunState::Stale | RunState::Finished
    ) && run.seen_at.is_none_or(|s| s < at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::types::RunKind;
    use serde_json::json;

    fn run(state: RunState) -> Run {
        let t: Timestamp = "2026-09-08T10:00:00Z".parse().unwrap();
        let mut r = Run::new("k7x2m9qa", RunKind::Agent, "fix it", vec![], "/w", "/w", t);
        r.state = state;
        r
    }
    fn ev(kind: &str, summary: &str, detail: Option<&str>) -> Event {
        Event {
            id: 1,
            at: "2026-09-08T10:05:00Z".parse().unwrap(),
            workspace: "/w".into(),
            run: Some("k7x2m9qa".into()),
            group_key: "/w".into(),
            kind: kind.into(),
            summary: summary.into(),
            data: detail.map(|d| json!({ "detail": d })),
        }
    }

    #[test]
    fn summaries_read_the_way_the_wireframe_does() {
        let q = ev(
            "run.needs_input",
            "needs input: Claude needs your permission to use Bash",
            Some("Claude needs your permission to use Bash"),
        );
        assert_eq!(
            summarise(&run(RunState::NeedsInput), Some(&q), &[]),
            "Claude needs your permission to use Bash"
        );
        let mut done = run(RunState::Finished);
        done.exit_code = Some(0);
        done.ended_at = Some("2026-09-08T10:41:03Z".parse().unwrap());
        assert_eq!(summarise(&done, None, &[]), "exit 0 · 41m03s");
        let mut w = run(RunState::Working);
        w.last_line = Some("Compiling…".into());
        assert_eq!(
            summarise(&w, Some(&ev("run.started", "started `x`", None)), &[]),
            "Compiling…"
        );
        assert_eq!(
            summarise(
                &w,
                Some(&ev("run.state", "working: tool: Edit", Some("tool: Edit"))),
                &[]
            ),
            "tool: Edit"
        );
        assert_eq!(summarise(&run(RunState::Idle), None, &[]), "idle");
        let mut busy = run(RunState::Working);
        busy.detail = Some("tool: Bash".into());
        busy.last_line = Some("$ cargo test".into());
        assert_eq!(
            summarise(
                &busy,
                Some(&ev("run.state", "working: session started", Some("session started"))),
                &[]
            ),
            "tool: Bash",
            "the run's own word beats the last event's"
        );
    }

    #[test]
    fn an_ended_run_says_what_it_raised() {
        let mut done = run(RunState::Finished);
        done.ended_at = Some("2026-09-08T10:01:00Z".parse().unwrap());
        done.exit_code = Some(0);
        let at = |s: &str| -> Timestamp { s.parse().unwrap() };
        let sig = |kind: &str, count, said: &str, when: &str| Signal {
            kind: kind.into(),
            count,
            said: said.into(),
            at: at(when),
        };
        let raised = [
            sig("log.issue", 1, "a note", "2026-09-08T10:00:30Z"),
            sig(
                "flint.product.new-feedback",
                5,
                "5 feedback rows to process",
                "2026-09-08T10:00:40Z",
            ),
        ];
        // The newest words, not the exit — that is what the probe went for.
        assert_eq!(summarise(&done, None, &raised), "5 feedback rows to process");
        assert_eq!(
            summarise(&done, None, &[]),
            "exit 0 · 1m00s",
            "nothing raised, nothing said"
        );
        // A firing that failed keeps its exit, which is the one worth reading.
        let mut bad = done.clone();
        bad.state = RunState::Failed;
        bad.exit_code = Some(1);
        assert_eq!(summarise(&bad, None, &raised), "exit 1 · 1m00s");
        // A run still going says what it is doing, as before.
        assert_eq!(summarise(&run(RunState::Idle), None, &raised), "idle");
    }

    #[test]
    fn unseen_is_attention_or_finish_not_yet_looked_at() {
        let at: Timestamp = "2026-09-08T10:05:00Z".parse().unwrap();
        assert!(unseen(&run(RunState::NeedsInput), at));
        assert!(unseen(&run(RunState::Finished), at));
        assert!(!unseen(&run(RunState::Working), at));
        let mut seen = run(RunState::NeedsInput);
        seen.seen_at = Some("2026-09-08T10:06:00Z".parse().unwrap());
        assert!(!unseen(&seen, at));
        seen.seen_at = Some("2026-09-08T10:04:00Z".parse().unwrap());
        assert!(unseen(&seen, at), "older look, newer news");
    }
}
