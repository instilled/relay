//! A run's frozen local facts: what it did, read from git, its transcript and
//! Relay's own events over its window. No assistant is called.

use crate::{git, summary, transcript};
use anyhow::Result;
use jiff::Timestamp;
use rl_ledger::events::{self, NewEvent};
use rl_ledger::{Ledger, facts as store};
use rl_proto::RunFacts;
use rl_proto::methods::EventFilter;
use rl_proto::types::{Event, Run, event_kind};
use std::time::Duration;

/// The longest silence in a transcript still counted as working.
pub const GAP: Duration = Duration::from_secs(120);

/// The facts of `run` over `[started_at, ended_at]`. Each source that is
/// missing (no repo, no transcript) leaves its facts empty.
pub async fn collect(ledger: &Ledger, run: &Run, start_head: Option<&str>, end_head: Option<&str>) -> RunFacts {
    let mut facts = RunFacts {
        run: run.id.clone(),
        branch: run.branch.clone(),
        at: run.ended_at.unwrap_or(run.started_at),
        ..RunFacts::default()
    };
    if let (Some(from), Some(to)) = (start_head, end_head) {
        let stat = git::delta(&run.cwd, from, to).await;
        facts.commits = stat.commits;
        facts.added = stat.added;
        facts.removed = stat.removed;
        facts.files = stat.files;
        facts.top_paths = stat.top_paths;
    }
    if let Some(branch) = &run.branch {
        facts.pushed = git::pushed(&run.cwd, branch).await;
    }
    if let Some(body) = transcript_body(ledger, run).await {
        let since = Some(run.started_at);
        let usage = transcript::usage(&body, since);
        facts.tokens_in = Some(usage.tokens_in);
        facts.tokens_cached = Some(usage.tokens_cached);
        facts.tokens_out = Some(usage.tokens_out);
        facts.active_secs = transcript::active_span(&body, since, GAP);
    }
    facts.errors = count_errors(&shell_events(ledger, run).await);
    facts
}

/// The one-line subjects of the commits a run landed, newest first.
pub async fn commits_in(_ledger: &Ledger, run: &Run, start_head: Option<&str>, end_head: Option<&str>) -> Vec<String> {
    match (start_head, end_head) {
        (Some(from), Some(to)) => git::log(&run.cwd, from, to).await,
        _ => Vec::new(),
    }
}

/// Reads the facts of a run that has just ended and stores them, with one
/// `commit.landed` event per commit it made. All git and transcript reading
/// happens before the ledger is written to.
pub async fn record(ledger: &Ledger, run: &Run, start_head: Option<&str>) -> Result<()> {
    let end_head = git::head(&run.cwd).await;
    let facts = collect(ledger, run, start_head, end_head.as_deref()).await;
    let commits = commits_in(ledger, run, start_head, end_head.as_deref()).await;
    let at = run.ended_at.unwrap_or(run.started_at);
    let landed: Vec<NewEvent> = commits
        .iter()
        .map(|line| {
            let sha = line.split_whitespace().next().unwrap_or_default();
            NewEvent::for_run(run, at, event_kind::COMMIT_LANDED, line.clone())
                .with_data(serde_json::json!({ "sha": sha }))
        })
        .collect();
    ledger
        .call(move |c| {
            store::set(c, &facts)?;
            for ev in &landed {
                events::append(c, ev)?;
            }
            Ok(())
        })
        .await
}

/// How many shell commands failed: those whose `exit` is present and non-zero.
/// A command with no recorded `exit` is not counted.
pub fn count_errors(events: &[Event]) -> u32 {
    events
        .iter()
        .filter(|e| e.kind == event_kind::SHELL_COMMAND)
        .filter(|e| {
            e.data
                .as_ref()
                .and_then(|v| v.get("exit"))
                .and_then(|v| v.as_i64())
                .is_some_and(|n| n != 0)
        })
        .count() as u32
}

async fn transcript_body(ledger: &Ledger, run: &Run) -> Option<String> {
    let path = summary::locate_in(ledger, run).await.ok().flatten()?;
    tokio::task::spawn_blocking(move || std::fs::read_to_string(path).ok())
        .await
        .ok()
        .flatten()
}

async fn shell_events(ledger: &Ledger, run: &Run) -> Vec<Event> {
    let (id, from) = (run.id.clone(), run.started_at);
    let to = run.ended_at;
    let found = ledger
        .call_read(move |c| {
            events::list(
                c,
                &EventFilter {
                    run: Some(id),
                    since: Some(from),
                    kinds: vec![event_kind::SHELL_COMMAND.to_string()],
                    limit: Some(u32::MAX),
                    ..Default::default()
                },
            )
        })
        .await
        .unwrap_or_default();
    found.into_iter().filter(|e| within(e.at, to)).collect()
}

fn within(at: Timestamp, end: Option<Timestamp>) -> bool {
    end.is_none_or(|e| at <= e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::path::PathBuf;

    fn ev(kind: &str, data: Option<Value>) -> Event {
        Event {
            id: 1,
            at: Timestamp::UNIX_EPOCH,
            workspace: PathBuf::from("/w"),
            run: Some("r".into()),
            group_key: String::new(),
            kind: kind.into(),
            summary: String::new(),
            data,
        }
    }

    #[test]
    fn errors_counts_nonzero_shell_commands() {
        let events = vec![
            ev("shell.command", Some(json!({"command": "ok", "exit": 0}))),
            ev("shell.command", Some(json!({"command": "bad", "exit": 2}))),
            ev("shell.command", Some(json!({"command": "unknown"}))),
            ev("shell.command", None),
            ev("run.state", Some(json!({"exit": 9}))),
        ];
        assert_eq!(count_errors(&events), 1);
    }

    #[test]
    fn window_end_is_inclusive_and_open_when_running() {
        let t = Timestamp::UNIX_EPOCH;
        assert!(within(t, Some(t)));
        assert!(!within(t + Duration::from_secs(1), Some(t)));
        assert!(within(t + Duration::from_secs(1), None));
    }
}

#[cfg(test)]
mod record_tests {
    use super::*;
    use rl_ledger::runs;
    use rl_proto::types::RunKind;
    use std::path::Path;

    fn git(dir: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .status()
            .unwrap();
        assert!(st.success());
    }

    #[tokio::test]
    async fn a_run_that_committed_has_facts_and_commit_events() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let start = git::head(&repo).await;
        assert!(start.is_some());
        std::fs::write(repo.join("a.txt"), "x\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "work"]);

        let ledger = Ledger::open(&tmp.path().join("ledger.db")).unwrap();
        let now = Timestamp::now();
        let mut run = Run::new(
            "r1",
            RunKind::Shell,
            "t",
            vec!["sh".into()],
            repo.clone(),
            repo.clone(),
            now,
        );
        run.ended_at = Some(now);
        let r = run.clone();
        ledger.call(move |c| runs::insert(c, &r)).await.unwrap();

        record(&ledger, &run, start.as_deref()).await.unwrap();

        let got = ledger.call_read(|c| store::get(c, "r1")).await.unwrap().unwrap();
        assert_eq!(got.commits, 1);
        assert_eq!(got.files, 1);
        let evs = ledger
            .call_read(|c| {
                events::list(
                    c,
                    &EventFilter {
                        run: Some("r1".into()),
                        kinds: vec![event_kind::COMMIT_LANDED.to_string()],
                        ..Default::default()
                    },
                )
            })
            .await
            .unwrap();
        assert_eq!(evs.len(), 1);
        assert!(evs[0].summary.ends_with("work"));
    }

    #[tokio::test]
    async fn without_a_start_head_nothing_is_claimed() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&tmp.path().join("ledger.db")).unwrap();
        let now = Timestamp::now();
        let run = Run::new(
            "r2",
            RunKind::Shell,
            "t",
            vec!["sh".into()],
            tmp.path(),
            tmp.path(),
            now,
        );
        let r = run.clone();
        ledger.call(move |c| runs::insert(c, &r)).await.unwrap();
        record(&ledger, &run, None).await.unwrap();
        let got = ledger.call_read(|c| store::get(c, "r2")).await.unwrap().unwrap();
        assert_eq!(got.commits, 0);
    }
}
