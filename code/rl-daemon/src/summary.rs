//! A recap of a run, live or ended, over a stretch back from now or the whole
//! of it, kept in `summaries` by the run. Only ever on demand (`/summary
//! [since]` in the panel, `rl summary <id> [since]`, or a reaction on
//! `run.finished`) — Relay never asks an agent on its own.
//!
//! From the cheapest source that has one:
//! - Claude writes its own recap into its transcript; the latest in the
//!   stretch is taken as it stands, no assistant asked;
//! - Codex keeps no recap, but its turns say what it last said and did: with
//!   `assistants.summary` set, an assistant condenses them, else they are
//!   the recap as facts;
//! - anything else — and Claude with no recap in the stretch — is read by
//!   `assistants.summary`: what was said, or the output.
//!
//! The one kept is given back while the run has not moved on. Never of an
//! assistant's own run, so a reaction cannot feed itself. The agent's own
//! recap is also kept as it is written (`recap.rs`), so the one kept is often
//! already there.

use crate::assistant;
use crate::server::Daemon;
use crate::transcript::{self, Agent};
use anyhow::{Result, anyhow, bail};
use jiff::Timestamp;
use rl_core::time::{fmt_duration, parse_duration};
use rl_ledger::events::NewEvent;
use rl_ledger::{events, runs, session_io, summaries};
use rl_proto::methods::{AssistantParams, EventFilter, SummaryParams};
use rl_proto::types::SummaryTarget;
use rl_proto::types::{Cause, Event, Run, Summary, event_kind};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The most of a run's output, or of what was said, the agent is shown.
const OUTPUT_TAIL: usize = 64 * 1024;
/// The most of an answer that is kept.
const TEXT_MAX: usize = 8 * 1024;

/// The one kept for the target, marked stale when a run has moved on since.
pub async fn get(d: &Daemon, p: &SummaryParams) -> Result<Option<Summary>> {
    let SummaryTarget::Run { id: run } = &p.target else {
        bail!("only a run can be summarised so far");
    };
    run_kept(d, run).await
}

/// The recap kept for a run, marked stale when it has moved on since.
async fn run_kept(d: &Daemon, run: &str) -> Result<Option<Summary>> {
    let id = run.to_string();
    let Some(mut s) = d.ledger.call(move |c| summaries::get(c, &id)).await? else {
        return Ok(None);
    };
    let id = run.to_string();
    if let Some(r) = d.ledger.call(move |c| runs::get(c, &id)).await?
        && !s.reach.is_empty()
    {
        let t = locate(d, &r).await?;
        s.stale = reach(d, &r, t.as_deref()).await? != s.reach;
    }
    Ok(Some(s))
}

/// The agent's session file for a run: the one a hook named, else found by
/// its folder and start and remembered.
pub(crate) async fn locate(d: &Daemon, run: &Run) -> Result<Option<PathBuf>> {
    let id = run.id.clone();
    if let Some(p) = d.ledger.call(move |c| summaries::transcript(c, &id)).await? {
        return Ok(Some(p));
    }
    let (Some(agent), Some(home)) = (Agent::of(run), std::env::var_os("HOME")) else {
        return Ok(None);
    };
    let home = PathBuf::from(home);
    let found = match agent {
        Agent::Claude => transcript::find_claude(&home, &run.cwd, run.started_at),
        Agent::Codex => transcript::find_codex(&home, &run.cwd, run.started_at),
    };
    if let Some(p) = &found {
        let (id, path) = (run.id.clone(), p.to_string_lossy().into_owned());
        d.ledger.call(move |c| summaries::set_transcript(c, &id, &path)).await?;
    }
    Ok(found)
}

/// How far a run has got: its session file's length, or its output's last
/// chunk.
async fn reach(d: &Daemon, run: &Run, transcript: Option<&Path>) -> Result<String> {
    if let Some(len) = transcript.and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len()) {
        return Ok(format!("t:{len}"));
    }
    let id = run.id.clone();
    let seq = d.ledger.call(move |c| session_io::last_seq(c, &id)).await?;
    Ok(format!("o:{seq}"))
}

/// The last `max` bytes of `s`, on a character boundary.
fn tail_of(s: &str, max: usize) -> &str {
    let mut at = s.len().saturating_sub(max);
    while !s.is_char_boundary(at) {
        at += 1;
    }
    &s[at..]
}

/// A recap of a run: the one kept while the run has not moved on, unless
/// `again`; else a new one from the cheapest source that has one.
pub async fn make(d: &Daemon, p: SummaryParams) -> Result<Summary> {
    let SummaryTarget::Run { id } = p.target.clone() else {
        bail!("only a run can be summarised so far");
    };
    let run = d
        .ledger
        .call({
            let id = id.clone();
            move |c| runs::get(c, &id)
        })
        .await?;
    let Some(run) = run else { bail!("no run {id}") };
    if matches!(run.cause, Cause::Assistant(_)) {
        bail!("`{}` is an assistant's own run; those are not summarised", run.title);
    }
    let since = p
        .since
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let now = Timestamp::now();
    // Whole milliseconds, as the ledger keeps them: what is returned is
    // what `summary.get` will say.
    let now = Timestamp::from_millisecond(now.as_millisecond()).unwrap_or(now);
    let from = match &since {
        Some(s) => {
            Some(now - parse_duration(s).ok_or_else(|| anyhow!("`{s}` is not a stretch of time, like 30m or 2h"))?)
        }
        None => None,
    };
    let file = locate(d, &run).await?;
    let reach = reach(d, &run, file.as_deref()).await?;
    if !p.again
        && let Some(kept) = run_kept(d, &id).await?
        && kept.since == since
        && kept.reach == reach
    {
        return Ok(kept);
    }
    let body = match &file {
        Some(f) => {
            let f = f.clone();
            tokio::task::spawn_blocking(move || std::fs::read_to_string(f).ok())
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    let mut made = Summary {
        run: id.clone(),
        agent: String::new(),
        by: String::new(),
        at: now,
        text: String::new(),
        since: since.clone(),
        source: String::new(),
        reach,
        stale: false,
        target: String::new(),
        deep: false,
        from: None,
        calls: 0,
    };
    // What an assistant would read, when the agent's own record does not do.
    let (heading, material) = match (Agent::of(&run), body) {
        (Some(Agent::Claude), Some(body)) => {
            if let Some(r) = transcript::claude_recap(&body, from) {
                made.agent = "claude".into();
                made.source = "claude recap".into();
                made.text = r.text;
                return keep(d, &run, made).await;
            }
            ("What was said".to_string(), transcript::claude_messages(&body, from))
        }
        (Some(Agent::Codex), Some(body)) => {
            let turns = transcript::codex_turns(&body, from);
            let asked = d.config.read().unwrap().config.assistants.summary.is_some();
            if !turns.is_empty() && !asked {
                made.agent = "codex".into();
                made.source = "codex turns".into();
                made.text = transcript::codex_line(&turns);
                return keep(d, &run, made).await;
            }
            made.source = "codex turns".into();
            ("Its turns".to_string(), transcript::codex_facts(&turns))
        }
        _ => {
            let bytes = d
                .ledger
                .call({
                    let id = id.clone();
                    move |c| match from {
                        Some(f) => session_io::tail_since(c, &id, f, OUTPUT_TAIL),
                        None => session_io::tail(c, &id, OUTPUT_TAIL),
                    }
                })
                .await?;
            (
                format!("Its output (the end of it, at most {} KB)", OUTPUT_TAIL / 1024),
                String::from_utf8_lossy(&bytes).into_owned(),
            )
        }
    };
    if material.trim().is_empty() {
        made.agent = "relay".into();
        if made.source.is_empty() {
            made.source = "nothing new".into();
        }
        made.text = match &since {
            Some(s) => format!("Nothing in the last {s}."),
            None => "Nothing to go on yet.".into(),
        };
        return keep(d, &run, made).await;
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
                        since: from,
                        limit: Some(200),
                        ..Default::default()
                    },
                )
            }
        })
        .await?;
    let prompt = prompt(&run, since.as_deref(), &evs, &heading, tail_of(&material, OUTPUT_TAIL));
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
            job: "summary".into(),
            prompt,
            cwd,
            title: format!("summary: {}", run.title),
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
    if made.source.is_empty() {
        made.source = "summarised".into();
    }
    made.agent = out.agent;
    made.by = out.run;
    made.text = text;
    keep(d, &run, made).await
}

/// Keeps a recap by its run, and says so in the ledger.
async fn keep(d: &Daemon, run: &Run, s: Summary) -> Result<Summary> {
    let run = run.clone();
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

/// What the agent is asked: the run as Relay knows it, the stretch, then the
/// ask.
pub fn prompt(run: &Run, since: Option<&str>, events: &[Event], heading: &str, material: &str) -> String {
    let mut s = String::new();
    let stretch = match since {
        Some(d) => format!("what happened in the last {d} of one run"),
        None => "one run".to_string(),
    };
    let _ = writeln!(
        s,
        "You are summarising {stretch} for the person who started it, who has not read its output. \
         Answer in plain prose, three sentences at most: what it did, what changed, and what is \
         left or went wrong. No headings, lists, YAML or preamble; do not repeat the command. If \
         nothing happened, say so in one sentence.\n"
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
    if let Some(d) = since {
        let _ = writeln!(s, "stretch: the last {d}");
    }
    let _ = writeln!(s, "\n## What happened\n");
    if events.is_empty() {
        let _ = writeln!(s, "(no events)");
    }
    for e in events {
        let _ = writeln!(s, "{}  {}  {}", e.at, e.kind, e.summary);
    }
    let _ = writeln!(s, "\n## {heading}\n");
    if material.trim().is_empty() {
        let _ = writeln!(s, "(nothing)");
    } else {
        let _ = writeln!(s, "````\n{}\n````", material.trim_end());
    }
    s
}

/// What `claude -p` says when its input is left open. An assistant's input is
/// closed (`assistant::run`), but should it ever be open the warning is the
/// CLI's, not the answer.
const STDIN_WARNING: &str = "Warning: no stdin data received";

/// The answer as kept: the CLI's stdin warning left out, trimmed, fences
/// around the whole of it dropped, capped.
pub(crate) fn clean(raw: &str) -> String {
    let said: String = raw
        .split_inclusive('\n')
        .filter(|l| !l.trim_start().starts_with(STDIN_WARNING))
        .collect();
    let mut t = said.trim();
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
        let p = prompt(
            &r,
            None,
            &[e],
            "Its output (the end of it, at most 64 KB)",
            "test parser ... FAILED\n",
        );
        assert!(
            p.contains("title: nightly tests\ncommand: make test\nfolder: /w\nwhy: schedule:nightly\n"),
            "{p}"
        );
        assert!(p.contains("· after 2m30s · failed · exit 2"), "{p}");
        assert!(p.contains("run.failed  exit 2"), "{p}");
        assert!(p.contains("````\ntest parser ... FAILED\n````"), "{p}");
        assert!(p.contains("## Its output (the end of it, at most 64 KB)\n"), "{p}");
        let p = prompt(&r, Some("2h"), &[], "What was said", "");
        assert!(p.contains("(no events)") && p.contains("(nothing)"), "{p}");
        assert!(p.contains("what happened in the last 2h of one run"), "{p}");
        assert!(
            p.contains("stretch: the last 2h\n") && p.contains("## What was said\n"),
            "{p}"
        );
        assert_eq!(tail_of("aébc", 3), "bc", "on a character boundary");
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

    #[test]
    fn the_clis_stdin_warning_is_not_the_answer() {
        let warning = "Warning: no stdin data received in 3s, proceeding without it. If piping from a slow \
                       command, redirect stdin explicitly: < /dev/null to skip, or wait longer.";
        assert_eq!(clean(&format!("{warning}\nIt ran.\nAll good.\n")), "It ran.\nAll good.");
        assert_eq!(
            clean(&format!("It ran.\r\n{warning}\r\nAll good.")),
            "It ran.\r\nAll good."
        );
        assert_eq!(clean(&format!("{warning}\n```\nIt ran.\n```\n")), "It ran.");
        assert_eq!(clean(warning), "", "the warning alone is nothing said");
        assert_eq!(
            clean("It warned: \"Warning: no stdin data received\"."),
            "It warned: \"Warning: no stdin data received\".",
            "only a line of its own"
        );
    }
}
