//! A summary of an ended run: what Relay knows about it — the row, its
//! events, the tail of its output — handed to `assistants.summarise` as an
//! ordinary assistant run, the answer kept in `summaries` by the run. On
//! demand (`s` in the panel, `rl summarise <id>`), or from a reaction on
//! `run.finished`. Never of an assistant's own run, so such a reaction cannot
//! feed itself.

use crate::assistant;
use crate::server::Daemon;
use anyhow::{Result, bail};
use jiff::Timestamp;
use rl_core::time::fmt_duration;
use rl_ledger::events::NewEvent;
use rl_ledger::{events, runs, session_io, summaries};
use rl_proto::methods::{AssistantParams, EventFilter, SummaryParams};
use rl_proto::types::{Cause, Event, Run, Summary, event_kind};
use std::fmt::Write as _;

/// The most of a run's output the agent is shown, from the end.
const OUTPUT_TAIL: usize = 64 * 1024;
/// The most of an answer that is kept.
const TEXT_MAX: usize = 8 * 1024;

pub async fn get(d: &Daemon, run: &str) -> Result<Option<Summary>> {
    let id = run.to_string();
    d.ledger.call(move |c| summaries::get(c, &id)).await
}

/// The summary of a run: the one kept, unless `again` or there is none yet.
pub async fn make(d: &Daemon, p: SummaryParams) -> Result<Summary> {
    let id = p.run.clone();
    let run = d
        .ledger
        .call({
            let id = id.clone();
            move |c| runs::get(c, &id)
        })
        .await?;
    let Some(run) = run else { bail!("no run {id}") };
    if !run.state.is_terminal() {
        bail!("`{}` is still going; a summary is of an ended run", run.title);
    }
    if matches!(run.cause, Cause::Assistant(_)) {
        bail!("`{}` is an assistant's own run; those are not summarised", run.title);
    }
    if !p.again
        && let Some(s) = get(d, &id).await?
    {
        return Ok(s);
    }
    let evs = d
        .ledger
        .call({
            let id = id.clone();
            move |c| {
                events::list(
                    c,
                    &EventFilter {
                        run: Some(id),
                        limit: Some(200),
                        ..Default::default()
                    },
                )
            }
        })
        .await?;
    let bytes = d
        .ledger
        .call({
            let id = id.clone();
            move |c| session_io::tail(c, &id, OUTPUT_TAIL)
        })
        .await?;
    let output = String::from_utf8_lossy(&bytes);
    let prompt = prompt(&run, &evs, &output);
    // Where the agent runs: the run's folder, if it is still there.
    let cwd = if run.cwd.is_dir() {
        run.cwd.clone()
    } else if run.workspace.is_dir() {
        run.workspace.clone()
    } else {
        d.home.path().to_path_buf()
    };
    let out = assistant::run(
        d,
        AssistantParams {
            job: "summarise".into(),
            prompt,
            cwd,
            title: format!("summarise: {}", run.title),
            timeout: p.timeout.clone(),
        },
    )
    .await?;
    if out.timed_out {
        bail!("{} did not answer in time; its run is {}", out.agent, out.run);
    }
    let text = clean(&out.output);
    if text.is_empty() {
        bail!(
            "{} said nothing (exit {}); its run is {}",
            out.agent,
            out.exit.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
            out.run
        );
    }
    // Whole milliseconds, as the ledger keeps them: what is returned is
    // what `summary.get` will say.
    let now = Timestamp::now();
    let at = Timestamp::from_millisecond(now.as_millisecond()).unwrap_or(now);
    let s = Summary {
        run: id,
        agent: out.agent,
        by: out.run,
        at,
        text,
    };
    d.ledger
        .call({
            let s = s.clone();
            move |c| {
                summaries::put(c, &s)?;
                let e = NewEvent::for_run(&run, s.at, event_kind::RUN_SUMMARISED, first_line(&s.text));
                events::append(c, &e)?;
                Ok(())
            }
        })
        .await?;
    Ok(s)
}

/// What the agent is asked: the run as Relay knows it, then the ask.
pub fn prompt(run: &Run, events: &[Event], output: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "You are summarising one run for the person who started it, who has not read its output. \
         Answer in plain prose, three sentences at most: what it did, what changed, and what is \
         left or went wrong. No headings, lists, YAML or preamble; do not repeat the command. If \
         the output shows nothing happened, say so in one sentence.\n"
    );
    let _ = writeln!(s, "## The run\n");
    let _ = writeln!(s, "title: {}", run.title);
    let _ = writeln!(s, "command: {}", run.argv.join(" "));
    let _ = writeln!(s, "folder: {}", run.cwd.display());
    if let Some(b) = &run.branch {
        let _ = writeln!(s, "branch: {b}");
    }
    let _ = writeln!(s, "why: {}", run.cause);
    let _ = writeln!(s, "started: {}", run.started_at);
    match run.ended_at {
        Some(e) => {
            let _ = writeln!(
                s,
                "ended: {} · after {} · {}{}",
                e,
                fmt_duration(e.duration_since(run.started_at)),
                run.state,
                run.exit_code.map(|c| format!(" · exit {c}")).unwrap_or_default()
            );
        }
        None => {
            let _ = writeln!(s, "state: {}", run.state);
        }
    }
    if !run.tags.is_empty() {
        let _ = writeln!(s, "tags: {}", run.tags.join(", "));
    }
    let _ = writeln!(s, "\n## What happened\n");
    if events.is_empty() {
        let _ = writeln!(s, "(no events)");
    }
    for e in events {
        let _ = writeln!(s, "{}  {}  {}", e.at, e.kind, e.summary);
    }
    let _ = writeln!(
        s,
        "\n## Its output (the end of it, at most {} KB)\n",
        OUTPUT_TAIL / 1024
    );
    if output.trim().is_empty() {
        let _ = writeln!(s, "(nothing)");
    } else {
        let _ = writeln!(s, "````\n{}\n````", output.trim_end());
    }
    s
}

/// The answer as kept: trimmed, fences around the whole of it dropped,
/// capped.
fn clean(raw: &str) -> String {
    let mut t = raw.trim();
    if t.starts_with("```")
        && t.ends_with("```")
        && let Some(nl) = t.find('\n')
    {
        t = t[nl + 1..t.len() - 3].trim();
    }
    let mut end = t.len().min(TEXT_MAX);
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    t[..end].to_string()
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::types::{RunKind, RunState};

    #[test]
    fn the_prompt_carries_the_run_its_events_and_its_output() {
        let t: Timestamp = "2026-09-10T11:00:00Z".parse().unwrap();
        let mut r = Run::new(
            "r1",
            RunKind::Command,
            "nightly tests",
            vec!["make".into(), "test".into()],
            "/w",
            "/w",
            t,
        );
        r.state = RunState::Failed;
        r.ended_at = Some("2026-09-10T11:02:30Z".parse().unwrap());
        r.exit_code = Some(2);
        r.cause = Cause::Schedule("nightly".into());
        let e = Event {
            id: 1,
            at: t,
            workspace: "/w".into(),
            run: Some("r1".into()),
            group_key: "g".into(),
            kind: "run.failed".into(),
            summary: "exit 2".into(),
            data: None,
        };
        let p = prompt(&r, &[e], "test parser ... FAILED\n");
        assert!(
            p.contains("title: nightly tests\ncommand: make test\nfolder: /w\nwhy: schedule:nightly\n"),
            "{p}"
        );
        assert!(p.contains("· after 2m30s · failed · exit 2"), "{p}");
        assert!(p.contains("run.failed  exit 2"), "{p}");
        assert!(p.contains("````\ntest parser ... FAILED\n````"), "{p}");
        let p = prompt(&r, &[], "");
        assert!(p.contains("(no events)") && p.contains("(nothing)"), "{p}");
    }

    #[test]
    fn an_answer_is_kept_as_prose() {
        assert_eq!(clean("\n  It ran.  \n"), "It ran.");
        assert_eq!(clean("```text\nIt ran.\nAll good.\n```"), "It ran.\nAll good.");
        assert_eq!(clean("```\n```"), "");
        assert_eq!(first_line("First.\nSecond."), "First.");
        let long = "é".repeat(TEXT_MAX);
        assert!(clean(&long).len() <= TEXT_MAX);
    }
}
