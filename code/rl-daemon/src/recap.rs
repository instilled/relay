//! The agent's own latest recap, kept as it is written, with no assistant
//! asked: Claude's `away_summary`, Codex's last turn. Looked for when a
//! Claude hook says a turn is over or it waits on you, and by a sweep every
//! few seconds over the Claude and Codex runs that wait on you or have just
//! ended — Claude writes its recap a while after the turn, often a minute or
//! more, and Codex has no hooks. Each read goes on from where the last one
//! stopped (`transcripts.read_to`), so a transcript of many MB is read once.
//!
//! Kept in `summaries` like any recap, so `/summary`, the pane and the
//! list's row see the same one — but never over a newer one: a recap asked
//! for after the agent wrote its own stays. Said in no event: one a turn would
//! bury the run's own news.

use crate::server::Daemon;
use crate::summary;
use crate::transcript::{self, Agent};
use anyhow::Result;
use jiff::{SignedDuration, Timestamp};
use rl_ledger::{runs, summaries};
use rl_proto::types::{Cause, Run, Summary};
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

/// How often the sweep looks.
const EVERY: Duration = Duration::from_secs(5);
/// How long after a run ends the sweep still looks at it.
const AFTER_END: SignedDuration = SignedDuration::from_mins(10);

/// Reads what a run's session file has gained since the last read, and keeps
/// the agent's latest recap in it. Whether one was kept.
pub async fn pick_up(d: &Daemon, run: &Run) -> Result<bool> {
    let Some(agent) = Agent::of(run) else { return Ok(false) };
    if matches!(run.cause, Cause::Assistant(_)) {
        return Ok(false);
    }
    let id = run.id.clone();
    let known = d
        .ledger
        .call(move |c| Ok((summaries::transcript(c, &id)?, summaries::read_to(c, &id)?)))
        .await?;
    let (file, from) = match known {
        (Some(p), n) => (p, n),
        (None, _) => match summary::locate(d, run).await? {
            Some(p) => (p, 0),
            None => return Ok(false),
        },
    };
    let (word, source) = match agent {
        Agent::Claude => ("away_summary", "claude recap"),
        Agent::Codex => ("task_complete", "codex turn"),
    };
    let read = tokio::task::spawn_blocking({
        let file = file.clone();
        move || transcript::read_on(&file, from, word)
    })
    .await?;
    // Gone or unreadable: nothing to keep, and the next look tries again.
    let Ok((text, to)) = read else { return Ok(false) };
    if to == from {
        return Ok(false);
    }
    // One written before the run started is about another.
    let since = Some(run.started_at - transcript::SLACK);
    let recap = match agent {
        Agent::Claude => transcript::claude_recap(&text, since),
        Agent::Codex => transcript::codex_recap(&text, since),
    };
    let made = recap.map(|r| Summary {
        run: run.id.clone(),
        agent: run.cli.clone().unwrap_or_default(),
        by: String::new(),
        at: r.at,
        text: summary::clean(&r.text),
        since: None,
        source: source.into(),
        reach: format!("t:{to}"),
        stale: false,
        target: String::new(),
        deep: false,
        from: None,
        calls: 0,
    });
    let (id, path) = (run.id.clone(), file.to_string_lossy().into_owned());
    d.ledger
        .call(move |c| {
            let kept = match &made {
                Some(s) if !s.text.is_empty() => summaries::put_newer(c, s)?,
                _ => false,
            };
            summaries::set_read_to(c, &id, &path, to)?;
            Ok(kept)
        })
        .await
}

/// Until shutdown: every few seconds, the runs whose recap may have moved on,
/// each read on where its file has grown.
pub async fn sweep(d: Arc<Daemon>) {
    loop {
        let ended = Timestamp::now() - AFTER_END;
        match d.ledger.call(move |c| runs::recappable(c, ended)).await {
            Ok(found) => {
                for run in found {
                    if let Err(e) = pick_up(&d, &run).await {
                        warn!(run = %run.id, error = %format!("{e:#}"), "recap not picked up");
                    }
                }
            }
            Err(e) => warn!(error = %format!("{e:#}"), "recaps"),
        }
        tokio::select! {
            _ = d.shutdown.cancelled() => return,
            _ = tokio::time::sleep(EVERY) => {}
        }
    }
}
