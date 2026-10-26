//! What happened, said for the person who was not looking: of a run, of a
//! flow's runs, of one firing's, of a project's, or of everything, over a stretch. Only ever
//! asked for: `/summary` in the panel, `rl summary`, a flow on
//! `run.finished`, or the catch-up's top when
//! `catch_up.summary` is `auto` — the panel and `rl since` ask for it as you
//! come back.
//!
//! Brief, a run is always written by `assistants.summary`, in three parts
//! (`Worked on:`, `Open questions:`, `Next:`), from the bounded tail of what
//! was said (Claude, Codex, Gemini) or of its output, the run's frozen facts
//! once it has ended, and the agent's own recap — kept apart as a seed — when
//! it wrote one in the stretch.
//!
//! Deep, a run is always read by the assistant, all of the stretch: Claude's
//! messages and tool calls, Codex's turns, or the whole output, with Relay's
//! events. Past `CHUNK` it is read in parts, each summarised, then those
//! joined — in parts again while what is joined is past `CHUNK` — so a
//! weekend of a busy agent is still one answer.
//!
//! Many runs (a flow, a project, everything) are told from their runs' lines
//! in one call when brief (a flow's runs folded, at most `LINES_MAX`, the
//! newest of each folder);
//! deep, from each run's deep read (kept ones as they are) and a join. `dry`
//! only counts the calls, each run's as its own deep read would.
//!
//! The stretch, unless given, is since you last looked: a run's `seen_at`
//! when it has written or had an event since, else all of it; a flow's, from
//! the start of its oldest run not seen, else the last day; one firing's, all of it. A project or everything needs one given — the
//! client knows when you were last here. A stretch with nothing in it says so
//! without asking anyone.
//!
//! The one kept is given back while what it covers has not moved on: a run's
//! brief recap of the whole of it in `summaries` (where the agent's own recap
//! is also kept as it is written, `recap.rs`), said on the run as a
//! `run.summarised` event; everything else in `summary_kept` by (what, how
//! deep, from when), said on no run. Never of an assistant's own run, so a
//! flow cannot feed itself.

use crate::assistant;
use crate::server::Daemon;
use crate::transcript::{self, Agent};
use anyhow::{Context, Result, bail};
use jiff::{SignedDuration, Timestamp};
use rl_core::display_path;
use rl_core::time::{fmt_duration, fmt_when};
use rl_ledger::events::NewEvent;
use rl_ledger::{Ledger, events, facts, kept, runs, seeds, session_io, summaries};
use rl_proto::brief::Brief;
use rl_proto::facts::RunFacts;
use rl_proto::methods::{AssistantParams, EventFilter, SummaryParams};
use rl_proto::types::{Cause, Event, Run, RunState, Summary, SummaryTarget, event_kind};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The most of a run's output, or of what was said, a brief read is shown.
const OUTPUT_TAIL: usize = 64 * 1024;
/// The most of an answer that is kept.
const TEXT_MAX: usize = 8 * 1024;
/// The most of what was said or written one assistant call reads; more is
/// read in parts.
const CHUNK: usize = 48 * 1024;
/// The most of Relay's events a brief read, and a deep one, is given.
const EVENTS_BRIEF: u32 = 200;
const EVENTS_DEEP: u32 = 500;
/// The most of those events one call is given, the newest kept.
const EVENTS_BYTES: usize = 8 * 1024;
/// How far back a flow is read when every run of it has been seen.
const FLOW_STRETCH: SignedDuration = SignedDuration::from_hours(24);

/// The one kept for the target, depth and stretch, marked stale when what it
/// covers has moved on since. A run's default stretch, brief, with nothing
/// kept for it gives back the newest said of the run — its recap, or one
/// since you looked — so looking at it again does not hide what was said.
pub async fn get(d: &Daemon, p: &SummaryParams) -> Result<Option<Summary>> {
    let from = p.from.map(whole);
    match &p.target {
        SummaryTarget::Run { id } => {
            let run = load(d, id).await?;
            let from = match &run {
                Some(r) => run_stretch(d, r, from).await?,
                None => from,
            };
            let found = match (p.deep, from) {
                (false, None) => None,
                _ => kept_of(d, &p.target, p.deep, from).await?,
            };
            match found {
                Some(mut s) => {
                    if let Some(r) = &run {
                        s.stale = run_reach(d, r).await? != s.reach;
                    }
                    Ok(Some(s))
                }
                None if !p.deep && p.from.is_none() => match run_latest(d, id, run.as_ref()).await? {
                    Some(s) => Ok(Some(s)),
                    // Nothing written: the agent's own recap, for display.
                    None => run_seed(d, id, run.as_ref()).await,
                },
                None => Ok(None),
            }
        }
        t => {
            // A project or everything with no stretch has nothing kept for it.
            let Ok(from) = over_from(d, t, from, now()).await else {
                return Ok(None);
            };
            let Some(mut s) = kept_of(d, t, p.deep, Some(from)).await? else {
                return Ok(None);
            };
            s.stale = many_reach(d, &in_stretch(d, t, from).await?).await? != s.reach;
            Ok(Some(s))
        }
    }
}

/// A summary of the target: the one kept while what it covers has not moved
/// on, unless `again`; else a new one. `dry` only says how many assistant
/// calls a new one takes, and asks no one.
pub async fn make(d: &Daemon, p: SummaryParams) -> Result<Summary> {
    let from = p.from.map(whole);
    match &p.target {
        SummaryTarget::Run { id } => {
            let Some(run) = load(d, id).await? else {
                bail!("no run {id}")
            };
            if matches!(run.cause, Cause::Assistant(_)) {
                bail!("`{}` is an assistant's own run; those are not summarised", run.title);
            }
            let from = run_stretch(d, &run, from).await?;
            if p.deep {
                run_deep(d, &run, from, &p).await
            } else {
                run_brief(d, &run, from, &p).await
            }
        }
        t => over(d, t, from, &p).await,
    }
}

/// Now, in whole milliseconds as the ledger keeps them: what is returned is
/// what `summary.get` will say.
fn now() -> Timestamp {
    whole(Timestamp::now())
}

fn whole(t: Timestamp) -> Timestamp {
    Timestamp::from_millisecond(t.as_millisecond()).unwrap_or(t)
}

async fn load(d: &Daemon, id: &str) -> Result<Option<Run>> {
    let id = id.to_string();
    d.ledger.call_read(move |c| runs::get(c, &id)).await
}

/// Where a run's stretch begins: as asked; else when you last looked at it,
/// if it has `moved` since; else all of it — live or ended alike.
fn run_from(run: &Run, asked: Option<Timestamp>, moved: bool) -> Option<Timestamp> {
    asked.or_else(|| run.seen_at.filter(|_| moved))
}

/// Whether a run has done anything since you last looked: written output, or
/// had an event other than being summarised. Looking (selecting the row)
/// sets `seen_at`, so a run that waits as it was is not "moved on".
async fn moved_since_seen(d: &Daemon, run: &Run) -> Result<bool> {
    let Some(seen) = run.seen_at else { return Ok(false) };
    let after = seen + SignedDuration::from_millis(1);
    let id = run.id.clone();
    d.ledger
        .call_read(move |c| {
            if !session_io::tail_since(c, &id, after, 1)?.is_empty() {
                return Ok(true);
            }
            let evs = events::list(
                c,
                &EventFilter {
                    run: Some(id),
                    since: Some(after),
                    limit: Some(EVENTS_BRIEF),
                    ..Default::default()
                },
            )?;
            Ok(evs.iter().any(|e| e.kind != event_kind::RUN_SUMMARISED))
        })
        .await
}

/// A run's stretch, as `run_from` says, looking at what it did since.
async fn run_stretch(d: &Daemon, run: &Run, asked: Option<Timestamp>) -> Result<Option<Timestamp>> {
    if asked.is_some() || run.seen_at.is_none() {
        return Ok(asked);
    }
    Ok(run_from(run, asked, moved_since_seen(d, run).await?))
}

/// The recap kept for a run, marked stale when it has moved on since.
async fn run_kept(d: &Daemon, run: &str) -> Result<Option<Summary>> {
    let id = run.to_string();
    let Some(mut s) = d.ledger.call_read(move |c| summaries::get(c, &id)).await? else {
        return Ok(None);
    };
    s.target = SummaryTarget::Run { id: run.to_string() }.key();
    s.calls = u32::from(!s.by.is_empty());
    if let Some(r) = load(d, run).await?
        && !s.reach.is_empty()
    {
        s.stale = run_reach(d, &r).await? != s.reach;
    }
    Ok(Some(s))
}

/// The agent's own recap of a run (`recap_seeds`), marked stale when the run
/// has moved on since. Shown when nothing was written; never kept as a brief.
async fn run_seed(d: &Daemon, run: &str, found: Option<&Run>) -> Result<Option<Summary>> {
    let id = run.to_string();
    let Some(mut s) = d.ledger.call_read(move |c| seeds::get(c, &id)).await? else {
        return Ok(None);
    };
    s.target = SummaryTarget::Run { id: run.to_string() }.key();
    if let Some(r) = found
        && !s.reach.is_empty()
    {
        s.stale = run_reach(d, r).await? != s.reach;
    }
    Ok(Some(s))
}

/// The newest brief one said of a run over any stretch: its recap of the
/// whole of it (`summaries`), or one since you looked (`summary_kept`) —
/// marked stale when it has moved on since.
async fn run_latest(d: &Daemon, id: &str, run: Option<&Run>) -> Result<Option<Summary>> {
    let whole = run_kept(d, id).await?;
    let key = SummaryTarget::Run { id: id.to_string() }.key();
    let since = d.ledger.call_read(move |c| kept::latest(c, &key, false)).await?;
    let Some(mut s) = since.filter(|s| whole.as_ref().is_none_or(|w| s.at > w.at)) else {
        return Ok(whole);
    };
    s.run = id.to_string();
    if let Some(r) = run {
        s.stale = run_reach(d, r).await? != s.reach;
    }
    Ok(Some(s))
}

/// The one kept in `summary_kept`, with the run it is of, if any.
async fn kept_of(d: &Daemon, t: &SummaryTarget, deep: bool, from: Option<Timestamp>) -> Result<Option<Summary>> {
    let key = t.key();
    let found = d.ledger.call_read(move |c| kept::get(c, &key, deep, from)).await?;
    Ok(found.map(|mut s| {
        if let SummaryTarget::Run { id } = t {
            s.run = id.clone();
        }
        s
    }))
}

/// The agent's session file for a run: the one a hook named, else found by
/// its folder and start and remembered.
pub(crate) async fn locate(d: &Daemon, run: &Run) -> Result<Option<PathBuf>> {
    locate_in(&d.ledger, run).await
}

/// [`locate`], for callers that hold the ledger but not the daemon.
pub(crate) async fn locate_in(ledger: &Ledger, run: &Run) -> Result<Option<PathBuf>> {
    let id = run.id.clone();
    if let Some(p) = ledger.call_read(move |c| summaries::transcript(c, &id)).await? {
        return Ok(Some(p));
    }
    let (Some(agent), Some(home)) = (Agent::of(run), std::env::var_os("HOME")) else {
        return Ok(None);
    };
    let home = PathBuf::from(home);
    let found = match agent {
        Agent::Claude => transcript::find_claude(&home, &run.cwd, run.started_at),
        Agent::Codex => transcript::find_codex(&home, &run.cwd, run.started_at),
        Agent::Gemini => transcript::find_gemini(&home, &run.cwd, run.started_at),
    };
    if let Some(p) = &found {
        let (id, path) = (run.id.clone(), p.to_string_lossy().into_owned());
        ledger.call(move |c| summaries::set_transcript(c, &id, &path)).await?;
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
    let seq = d.ledger.call_read(move |c| session_io::last_seq(c, &id)).await?;
    Ok(format!("o:{seq}"))
}

/// How far a run has got, its session file found first.
async fn run_reach(d: &Daemon, run: &Run) -> Result<String> {
    let file = locate(d, run).await?;
    reach(d, run, file.as_deref()).await
}

/// How far many runs have got: how many, the latest change of state, and
/// how much they have written — so one that writes on, in the same state,
/// has moved on too.
async fn many_reach(d: &Daemon, runs: &[Run]) -> Result<String> {
    let last = runs.iter().map(|r| r.state_at.as_millisecond()).max().unwrap_or(0);
    let ids: Vec<String> = runs.iter().map(|r| r.id.clone()).collect();
    let out = d
        .ledger
        .call_read(move |c| ids.iter().map(|id| session_io::last_seq(c, id)).sum::<Result<i64>>())
        .await?;
    Ok(format!("n:{}:{last}:{out}", runs.len()))
}

/// The last `max` bytes of `s`, on a character boundary.
fn tail_of(s: &str, max: usize) -> &str {
    let mut at = s.len().saturating_sub(max);
    while !s.is_char_boundary(at) {
        at += 1;
    }
    &s[at..]
}

/// `s` in about equal parts of at most `CHUNK` — as many as it takes — each
/// ending at a line's end where a line ends within reach.
fn parts(s: &str) -> Vec<&str> {
    if s.len() <= CHUNK {
        return vec![s];
    }
    let size = s.len().div_ceil(s.len().div_ceil(CHUNK));
    let b = s.as_bytes();
    let (mut out, mut start) = (Vec::new(), 0);
    while start < s.len() {
        let mut end = (start + size).min(s.len());
        if end < s.len() {
            let limit = (start + CHUNK).min(s.len());
            end = match b[end..limit].iter().position(|&c| c == b'\n') {
                Some(i) => end + i + 1,
                None => match b[start..end].iter().rposition(|&c| c == b'\n') {
                    Some(i) if i > 0 => start + i + 1,
                    _ => {
                        let mut e = limit;
                        while !s.is_char_boundary(e) {
                            e -= 1;
                        }
                        e
                    }
                },
            };
        }
        out.push(&s[start..end]);
        start = end;
    }
    out
}

/// A summary yet to be said.
fn blank(target: &SummaryTarget, run: &str, deep: bool, from: Option<Timestamp>, reach: String) -> Summary {
    Summary {
        run: run.to_string(),
        agent: String::new(),
        by: String::new(),
        at: now(),
        text: String::new(),
        since: None,
        source: String::new(),
        reach,
        stale: false,
        target: target.key(),
        deep,
        from,
        calls: 0,
        runs: u32::from(!run.is_empty()),
    }
}

/// A stretch with nothing in it, said without asking anyone.
fn nothing(s: &mut Summary) {
    s.agent = "relay".into();
    if s.source.is_empty() {
        s.source = "nothing new".into();
    }
    s.text = match s.from {
        Some(f) => format!("nothing happened since {}", fmt_when(f, s.at)),
        None => "Nothing to go on yet.".into(),
    };
}

/// What a dry run says of one kept or free: no calls, no text.
fn dry_of(mut s: Summary) -> Summary {
    s.text.clear();
    s.calls = 0;
    s
}

async fn read_body(file: Option<&Path>) -> Option<String> {
    let f = file?.to_path_buf();
    tokio::task::spawn_blocking(move || std::fs::read_to_string(f).ok())
        .await
        .ok()
        .flatten()
}

/// A run's recorded output over the stretch, at most `max` bytes of its end.
async fn output(d: &Daemon, run: &Run, from: Option<Timestamp>, max: usize) -> Result<String> {
    let id = run.id.clone();
    let bytes = d
        .ledger
        .call_read(move |c| match from {
            Some(f) => session_io::tail_since(c, &id, f, max),
            None => session_io::tail(c, &id, max),
        })
        .await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// What Relay knows of a run besides what it said: its events over the stretch
/// (at most `limit`), its frozen facts once it has ended, and the agent's own
/// recap when it was written within the stretch — all in one ledger read.
async fn run_context(
    d: &Daemon,
    run: &Run,
    from: Option<Timestamp>,
    limit: u32,
) -> Result<(Vec<Event>, Option<RunFacts>, Option<Summary>)> {
    let id = run.id.clone();
    let ended = run.ended_at.is_some();
    d.ledger
        .call_read(move |c| {
            let evs = events::list(
                c,
                &EventFilter {
                    run: Some(id.clone()),
                    since: from,
                    limit: Some(limit),
                    ..Default::default()
                },
            )?;
            let facts = if ended { facts::get(c, &id)? } else { None };
            let seed = seeds::get(c, &id)?.filter(|s| from.is_none_or(|f| s.at >= f));
            Ok((evs, facts, seed))
        })
        .await
}

/// Where the assistant runs for a run: its folder, if it is still there.
pub(crate) fn run_cwd(d: &Daemon, run: &Run) -> PathBuf {
    if run.cwd.is_dir() {
        run.cwd.clone()
    } else if run.workspace.is_dir() {
        run.workspace.clone()
    } else {
        d.home.path().to_path_buf()
    }
}

/// A run, brief: the one kept while it has not moved on, unless `again`;
/// else a new one from the cheapest source that has one.
async fn run_brief(d: &Daemon, run: &Run, from: Option<Timestamp>, p: &SummaryParams) -> Result<Summary> {
    let file = locate(d, run).await?;
    let reach = reach(d, run, file.as_deref()).await?;
    if !p.again {
        let kept = match from {
            None => run_kept(d, &run.id).await?.filter(|k| k.since.is_none()),
            Some(_) => kept_of(d, &p.target, false, from).await?,
        };
        if let Some(k) = kept.filter(|k| k.reach == reach) {
            return Ok(if p.dry { dry_of(k) } else { k });
        }
    }
    let body = read_body(file.as_deref()).await;
    let mut made = blank(&p.target, &run.id, false, from, reach);
    // What the assistant reads: the agent's own record of the stretch, else
    // the end of the run's output.
    let (source, heading, material) = match (Agent::of(run), body) {
        (Some(Agent::Claude), Some(body)) => (
            "claude",
            "What was said".to_string(),
            transcript::claude_messages(&body, from),
        ),
        (Some(Agent::Gemini), Some(body)) => (
            "gemini",
            "What was said".to_string(),
            transcript::gemini_messages(&body, from),
        ),
        (Some(Agent::Codex), Some(body)) => (
            "codex",
            "Its turns".to_string(),
            transcript::codex_facts(&transcript::codex_turns(&body, from)),
        ),
        _ => (
            "output",
            format!("Its output (the end of it, at most {} KB)", OUTPUT_TAIL / 1024),
            output(d, run, from, OUTPUT_TAIL).await?,
        ),
    };
    if material.trim().is_empty() {
        nothing(&mut made);
        return done(d, Some(run), made, p.dry).await;
    }
    made.source = source.into();
    made.calls = 1;
    if p.dry {
        return Ok(made);
    }
    let (evs, facts, seed) = run_context(d, run, from, EVENTS_BRIEF).await?;
    let seed = seed.as_ref().map(|s| s.text.as_str());
    let prompt = prompt(
        run,
        from,
        Some(&evs),
        facts.as_ref(),
        seed,
        &heading,
        tail_of(&material, OUTPUT_TAIL),
        false,
    );
    let a = ask(d, run_cwd(d, run), &run.title, prompt, p).await?;
    (made.agent, made.by, made.text) = (a.agent, a.by, a.text);
    keep(d, Some(run), made).await
}

/// A run's deep read kept for this stretch, while the run has not moved on.
async fn deep_kept(d: &Daemon, run: &Run, from: Option<Timestamp>) -> Result<Option<Summary>> {
    let reach = run_reach(d, run).await?;
    let t = SummaryTarget::Run { id: run.id.clone() };
    Ok(kept_of(d, &t, true, from).await?.filter(|k| k.reach == reach))
}

/// A run, deep: all of the stretch, read by the assistant — in parts past
/// `CHUNK`, then joined.
async fn run_deep(d: &Daemon, run: &Run, from: Option<Timestamp>, p: &SummaryParams) -> Result<Summary> {
    if !p.again
        && let Some(k) = deep_kept(d, run, from).await?
    {
        return Ok(if p.dry { dry_of(k) } else { k });
    }
    let file = locate(d, run).await?;
    let reach = reach(d, run, file.as_deref()).await?;
    let target = SummaryTarget::Run { id: run.id.clone() };
    let mut made = blank(&target, &run.id, true, from, reach);
    let body = read_body(file.as_deref()).await;
    let (heading, material) = match (Agent::of(run), body) {
        (Some(Agent::Claude), Some(body)) => (
            "What was said and done".to_string(),
            transcript::claude_story(&body, from),
        ),
        (Some(Agent::Codex), Some(body)) => (
            "Its turns".to_string(),
            transcript::codex_facts(&transcript::codex_turns(&body, from)),
        ),
        (Some(Agent::Gemini), Some(body)) => (
            "What was said and done".to_string(),
            transcript::gemini_story(&body, from),
        ),
        _ => ("Its output".to_string(), output(d, run, from, usize::MAX).await?),
    };
    if material.trim().is_empty() {
        nothing(&mut made);
        return done(d, Some(run), made, p.dry).await;
    }
    let parts = parts(&material);
    let n = parts.len();
    made.calls = if n > 1 { n as u32 + 1 } else { 1 };
    if p.dry {
        return Ok(made);
    }
    let (evs, facts, seed) = run_context(d, run, from, EVENTS_DEEP).await?;
    let seed = seed.as_ref().map(|s| s.text.as_str());
    let cwd = run_cwd(d, run);
    let a = if n == 1 {
        ask(
            d,
            cwd,
            &run.title,
            prompt(run, from, Some(&evs), facts.as_ref(), seed, &heading, parts[0], true),
            p,
        )
        .await?
    } else {
        let mut said = Vec::with_capacity(n);
        for (i, part) in parts.iter().enumerate() {
            let at = format!("Part {} of {n}", i + 1);
            let first = i == 0;
            let q = prompt(
                run,
                from,
                first.then_some(evs.as_slice()),
                facts.as_ref().filter(|_| first),
                seed.filter(|_| first),
                &format!("{at}: {heading}"),
                part,
                true,
            );
            let a = ask(d, cwd.clone(), &run.title, q, p)
                .await
                .with_context(|| format!("part {} of {n}", i + 1))?;
            said.push((at, a.text));
        }
        let what = format!("the run `{}`", run.title);
        let (a, calls) = join(&what, from, &said, |q| ask(d, cwd.clone(), &run.title, q, p)).await?;
        made.calls = n as u32 + calls;
        a
    };
    made.source = if n == 1 {
        "summarised".into()
    } else {
        format!("read in {n} parts")
    };
    (made.agent, made.by, made.text) = (a.agent, a.by, a.text);
    keep(d, Some(run), made).await
}

/// Where many runs' stretch begins: as asked; for a flow, the start of its
/// oldest run not seen, else the last day; a project and everything need one.
async fn over_from(d: &Daemon, t: &SummaryTarget, asked: Option<Timestamp>, now: Timestamp) -> Result<Timestamp> {
    if let Some(f) = asked {
        return Ok(f);
    }
    match t {
        SummaryTarget::Flow { name } => {
            let cause = format!("flow:{name}/");
            let oldest = d.ledger.call_read(move |c| runs::oldest_unseen(c, &cause)).await?;
            Ok(oldest.unwrap_or(now - FLOW_STRETCH))
        }
        SummaryTarget::Firing { id } => {
            let key = id.clone();
            match d.ledger.call_read(move |c| rl_ledger::firings::get(c, &key)).await? {
                Some(f) => Ok(f.started_at),
                None => bail!("no firing {id}"),
            }
        }
        SummaryTarget::Project { .. } => bail!("a project's summary needs where to start (from)"),
        SummaryTarget::All => bail!("a summary of everything needs where to start (from)"),
        SummaryTarget::Run { .. } => bail!("a run's stretch is its own"),
    }
}

/// The runs a flow, a project or everything has in the stretch: those that
/// moved in it or are still going, never an assistant's own; by folder, then
/// as they started.
async fn in_stretch(d: &Daemon, t: &SummaryTarget, from: Timestamp) -> Result<Vec<Run>> {
    let found = d.ledger.call_read(move |c| runs::list_for_digest(c, from)).await?;
    let mut runs: Vec<Run> = found
        .into_iter()
        .filter(|r| !matches!(r.cause, Cause::Assistant(_)) && r.ended_at.is_none_or(|e| e >= from))
        .filter(|r| match t {
            SummaryTarget::Flow { name } => matches!(&r.cause, Cause::Flow { flow, .. } if flow == name),
            SummaryTarget::Firing { id } => r.chain == *id && matches!(r.cause, Cause::Flow { .. }),
            SummaryTarget::Project { workspace } => r.workspace == *workspace,
            SummaryTarget::All => true,
            SummaryTarget::Run { id } => r.id == *id,
        })
        .collect();
    runs.sort_by(|a, b| (&a.workspace, a.started_at, &a.id).cmp(&(&b.workspace, b.started_at, &b.id)));
    Ok(runs)
}

/// What many runs are, for the assistant; one firing's by its flow, read off its runs.
fn what(t: &SummaryTarget, runs: &[Run]) -> String {
    match t {
        SummaryTarget::Flow { name } => format!("the flow `{name}`"),
        SummaryTarget::Firing { .. } => match runs.iter().find_map(|r| match &r.cause {
            Cause::Flow { flow, .. } => Some(flow),
            _ => None,
        }) {
            Some(flow) => format!("one firing of the flow `{flow}`"),
            None => "one firing of a flow".into(),
        },
        SummaryTarget::Project { workspace } => format!("the project {}", display_path(workspace)),
        SummaryTarget::All => "everything Relay ran".into(),
        SummaryTarget::Run { id } => format!("the run {id}"),
    }
}

/// A run's heading among many: its title and when it started, and its folder
/// when they are from all over.
fn heading(t: &SummaryTarget, r: &Run, now: Timestamp) -> String {
    match t {
        SummaryTarget::All => format!(
            "{} · {} · {}",
            r.title,
            display_path(&r.workspace),
            fmt_when(r.started_at, now)
        ),
        _ => format!("{} · {}", r.title, fmt_when(r.started_at, now)),
    }
}

/// A run's line among many, brief: its written brief's first part (never a
/// raw label), else where it stands.
async fn brief_line(d: &Daemon, r: &Run, brief: &Summary) -> Result<String> {
    let line = if brief.source == "nothing new" {
        None
    } else {
        part_line(&brief.text)
    };
    let line = match line {
        Some(l) => l,
        None => {
            let id = r.id.clone();
            let (last, emitted) = d
                .ledger
                .call_read(move |c| Ok((events::latest_for_run(c, &id)?, events::emitted_for_run(c, &id)?)))
                .await?;
            crate::digest::summarise(r, last.as_ref(), &emitted)
        }
    };
    Ok(format!("{} · {line}", r.state))
}

/// The first of a brief's parts that has anything, without its label; else
/// the first line with anything on it. None when every part is `none`.
fn part_line(text: &str) -> Option<String> {
    let b = Brief::parse(text);
    let part = b.worked_on.or(b.next).or(b.questions);
    let text = match part.as_deref() {
        Some(p) => p,
        // Labelled, but every part `none`: no line, not a raw label.
        None if Brief::is_labelled(text) => return None,
        None => text,
    };
    text.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

/// A flow's, a project's or everything's runs: each run's brief first (a kept
/// one as it is), or its deep read; then one call to join them. Each is read
/// on the run's own stretch, so what is kept matches the next look.
async fn over(d: &Daemon, t: &SummaryTarget, from: Option<Timestamp>, p: &SummaryParams) -> Result<Summary> {
    let now = now();
    let from = over_from(d, t, from, now).await?;
    let runs = in_stretch(d, t, from).await?;
    let reach = many_reach(d, &runs).await?;
    if !p.again
        && let Some(k) = kept_of(d, t, p.deep, Some(from)).await?.filter(|k| k.reach == reach)
    {
        return Ok(if p.dry { dry_of(k) } else { k });
    }
    let mut made = blank(t, "", p.deep, Some(from), reach);
    made.runs = runs.len() as u32;
    if runs.is_empty() {
        nothing(&mut made);
        return done(d, None, made, p.dry).await;
    }
    // Each run is read on its own stretch (since it was last seen, else all
    // of it), not the over's, so what is kept matches the next look at it.
    let lines = if !p.deep {
        // Each run's brief, a kept one costing nothing; and one to join them.
        let mut told = Vec::with_capacity(runs.len());
        let mut briefed = 0;
        for r in &runs {
            let own = run_stretch(d, r, None).await?;
            let mut ask = SummaryParams {
                target: SummaryTarget::Run { id: r.id.clone() },
                deep: false,
                from: own,
                again: false,
                dry: true,
                timeout: p.timeout.clone(),
            };
            // What it would cost: nothing for one kept (a kept one carries
            // the call that wrote it, so it is asked of the dry run).
            briefed += run_brief(d, r, own, &ask).await?.calls;
            if !p.dry {
                ask.dry = false;
                let s = run_brief(d, r, own, &ask)
                    .await
                    .with_context(|| format!("briefing `{}`", r.title))?;
                told.push(brief_line(d, r, &s).await?);
            }
        }
        made.calls = briefed;
        if p.dry {
            made.calls += 1;
            return Ok(made);
        }
        many_lines(t, &runs, told, now)
    } else if p.dry {
        // Each run as its own deep read would cost — none for one kept or
        // with nothing in it, its parts and their join for a long one — and
        // one to join them.
        let mut calls = 1;
        for r in &runs {
            let own = run_stretch(d, r, None).await?;
            let dry = SummaryParams {
                target: SummaryTarget::Run { id: r.id.clone() },
                deep: true,
                from: own,
                again: false,
                dry: true,
                timeout: None,
            };
            calls += run_deep(d, r, own, &dry).await?.calls;
        }
        made.calls = calls;
        return Ok(made);
    } else {
        let mut lines = Vec::with_capacity(runs.len());
        for r in &runs {
            let own = run_stretch(d, r, None).await?;
            let s = match deep_kept(d, r, own).await? {
                Some(k) => k,
                None => {
                    let fresh = SummaryParams {
                        target: SummaryTarget::Run { id: r.id.clone() },
                        deep: true,
                        from: own,
                        again: true,
                        dry: false,
                        timeout: p.timeout.clone(),
                    };
                    let s = run_deep(d, r, own, &fresh)
                        .await
                        .with_context(|| format!("reading `{}`", r.title))?;
                    made.calls += s.calls;
                    s
                }
            };
            lines.push((heading(t, r, now), s.text));
        }
        lines
    };
    let what = what(t, &runs);
    let cwd = match t {
        SummaryTarget::Project { workspace } if workspace.is_dir() => workspace.clone(),
        _ => d.home.path().to_path_buf(),
    };
    let (a, calls) = join(&what, Some(from), &lines, |q| ask(d, cwd.clone(), &what, q, p)).await?;
    made.calls += calls;
    made.source = format!("over {} run{}", runs.len(), if runs.len() == 1 { "" } else { "s" });
    (made.agent, made.by, made.text) = (a.agent, a.by, a.text);
    keep(d, None, made).await
}

/// The most lines many runs are told by, brief; past it the rest are
/// counted.
const LINES_MAX: usize = 200;
/// The most of a run's heading, and of its line, told among many.
const HEADING_CUT: usize = 80;
const LINE_CUT: usize = 140;

/// The lines many runs are told by, brief: each run's under its heading, but
/// a flow's runs folded into one (`nightly · 14 runs · 2 failed · last: …`)
/// unless the flow is what is told. Past `LINES_MAX` the newest are told
/// and the rest counted (`… and 12 earlier`); for everything, the room is
/// shared between folders so a busy one does not crowd out the others. Each
/// is cut short, so the lot fits in one call.
fn many_lines(t: &SummaryTarget, runs: &[Run], lines: Vec<String>, now: Timestamp) -> Vec<(String, String)> {
    struct Fold {
        at: usize,
        name: String,
        runs: usize,
        failed: usize,
        last: String,
    }
    let mut out: Vec<(String, String)> = Vec::new();
    // Each line's share of the room — its folder for everything, one share
    // for the rest — and when the newest run it tells started.
    let mut when: Vec<(PathBuf, Timestamp)> = Vec::new();
    let share = |r: &Run| match t {
        SummaryTarget::All => r.workspace.clone(),
        _ => PathBuf::new(),
    };
    let mut folds: Vec<((String, PathBuf), Fold)> = Vec::new();
    for (r, line) in runs.iter().zip(lines) {
        let flow = match (&r.cause, t) {
            (_, SummaryTarget::Flow { .. } | SummaryTarget::Firing { .. }) => None,
            (Cause::Flow { flow, .. }, _) => Some(flow),
            _ => None,
        };
        let Some(name) = flow else {
            out.push((heading(t, r, now), line));
            when.push((share(r), r.started_at));
            continue;
        };
        let key = (name.clone(), r.workspace.clone());
        let failed = usize::from(r.state == RunState::Failed);
        match folds.iter_mut().find(|(k, _)| *k == key) {
            Some((_, f)) => {
                f.runs += 1;
                f.failed += failed;
                f.last = line;
                let mut said = format!("{} runs", f.runs);
                if f.failed > 0 {
                    said.push_str(&format!(" · {} failed", f.failed));
                }
                said.push_str(&format!(" · last: {}", f.last));
                let head = match t {
                    SummaryTarget::All => format!("{} · {}", f.name, display_path(&r.workspace)),
                    _ => f.name.clone(),
                };
                out[f.at] = (head, said);
                when[f.at].1 = when[f.at].1.max(r.started_at);
            }
            None => {
                folds.push((
                    key,
                    Fold {
                        at: out.len(),
                        name: name.clone(),
                        runs: 1,
                        failed,
                        last: line.clone(),
                    },
                ));
                out.push((heading(t, r, now), line));
                when.push((share(r), r.started_at));
            }
        }
    }
    let keep = newest(&when, LINES_MAX);
    let mut told = Vec::with_capacity(out.len().min(LINES_MAX) + 1);
    let mut counted: Vec<&Path> = Vec::new();
    for (i, (head, line)) in out.into_iter().enumerate() {
        if !keep[i] {
            continue;
        }
        // What a share left out is counted above the first of it told.
        let folder = when[i].0.as_path();
        if !counted.contains(&folder) {
            counted.push(folder);
            let left = when.iter().zip(&keep).filter(|((w, _), k)| w == folder && !**k).count();
            if left > 0 {
                let whose = match t {
                    SummaryTarget::All => format!(" · {}", display_path(folder)),
                    _ => String::new(),
                };
                told.push((format!("… and {left} earlier{whose}"), String::new()));
            }
        }
        told.push((cut(&head, HEADING_CUT), cut(&line, LINE_CUT)));
    }
    told
}

/// Which of many lines to tell, at most `max`, given each one's share (its
/// folder, for everything) and when it last moved: the room is shared out
/// between them — one with fewer lines than its part tells them all and
/// leaves the rest to the others — and each tells its newest.
fn newest(when: &[(PathBuf, Timestamp)], max: usize) -> Vec<bool> {
    let mut folders: Vec<(&Path, usize)> = Vec::new();
    for (w, _) in when {
        match folders.iter_mut().find(|(f, _)| *f == w.as_path()) {
            Some((_, n)) => *n += 1,
            None => folders.push((w, 1)),
        }
    }
    folders.sort_by_key(|(_, n)| *n);
    let (mut room, mut left) = (max, folders.len());
    let mut keep = vec![false; when.len()];
    for (folder, n) in folders {
        let share = (room / left).min(n);
        room -= share;
        left -= 1;
        let mut mine: Vec<usize> = (0..when.len()).filter(|&i| when[i].0 == folder).collect();
        mine.sort_by_key(|&i| std::cmp::Reverse((when[i].1, i)));
        for i in mine.into_iter().take(share) {
            keep[i] = true;
        }
    }
    keep
}

/// `s`, at most `max` characters, `…` where it was cut.
fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max - 1).collect();
    format!("{}…", kept.trim_end())
}

/// Many answers or lines joined into one by `ask`: one call while what they
/// say fits in `CHUNK`; past it, told in parts, then those joined the same
/// way, until it fits. With the calls it took.
async fn join<F: Future<Output = Result<Answer>>>(
    what: &str,
    from: Option<Timestamp>,
    lines: &[(String, String)],
    mut ask: impl FnMut(String) -> F,
) -> Result<(Answer, u32)> {
    let mut body = over_body(lines);
    let mut calls = 0;
    while body.len() > CHUNK {
        let parts = parts(&body);
        let n = parts.len();
        let mut said = Vec::with_capacity(n);
        for (i, part) in parts.iter().enumerate() {
            let a = ask(prompt_over_text(what, from, part))
                .await
                .with_context(|| format!("joining, part {} of {n}", i + 1))?;
            calls += 1;
            said.push((format!("Part {} of {n}", i + 1), a.text));
        }
        let next = over_body(&said);
        if next.len() >= body.len() {
            bail!("joining what was said of {what} did not make it shorter");
        }
        body = next;
    }
    Ok((ask(prompt_over_text(what, from, &body)).await?, calls + 1))
}

/// What an assistant answered, and its run.
struct Answer {
    agent: String,
    by: String,
    text: String,
}

/// One assistant call, as a run titled `summary: <of>`; an answer that is
/// late or empty is an error that names its run.
async fn ask(d: &Daemon, cwd: PathBuf, of: &str, prompt: String, p: &SummaryParams) -> Result<Answer> {
    let out = assistant::run(
        d,
        AssistantParams {
            job: "summary".into(),
            prompt,
            cwd,
            title: format!("summary: {of}"),
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
    Ok(Answer {
        agent: out.agent,
        by: out.run,
        text,
    })
}

/// One made without asking anyone: kept, unless it was only a dry run.
async fn done(d: &Daemon, run: Option<&Run>, s: Summary, dry: bool) -> Result<Summary> {
    if dry {
        return Ok(dry_of(s));
    }
    keep(d, run, s).await
}

/// Keeps a summary — a run's brief recap of the whole of it by the run, and
/// said on the run as a `run.summarised` event; anything else by what, how
/// deep and from when, said on no run, so reading runs for a catch-up is not
/// news on them.
async fn keep(d: &Daemon, run: Option<&Run>, s: Summary) -> Result<Summary> {
    let run = run.filter(|_| !s.deep && s.from.is_none()).cloned();
    d.ledger
        .call({
            let s = s.clone();
            move |c| {
                match &run {
                    Some(run) => {
                        summaries::put(c, &s)?;
                        let e = NewEvent::for_run(run, s.at, event_kind::RUN_SUMMARISED, first_line(&s.text));
                        events::append(c, &e)?;
                    }
                    None => kept::put(c, &s)?,
                }
                Ok(())
            }
        })
        .await?;
    Ok(s)
}

/// What the agent is asked of a run: the run as Relay knows it, the stretch,
/// its events (when given), what it said or wrote, then the ask — brief or
/// deep.
#[allow(clippy::too_many_arguments)]
pub fn prompt(
    run: &Run,
    from: Option<Timestamp>,
    events: Option<&[Event]>,
    facts: Option<&RunFacts>,
    seed: Option<&str>,
    heading: &str,
    material: &str,
    deep: bool,
) -> String {
    let mut s = String::new();
    let stretch = match from {
        Some(f) => format!("what happened since {} in one run", fmt_when(f, Timestamp::now())),
        None => "one run".to_string(),
    };
    if deep {
        let _ = writeln!(
            s,
            "You are reading {stretch} closely for the person who started it, who has not read its \
             output. Say what happened, in order, and what changed where — files, branches, commands \
             and how they ended — then what is left or went wrong. Short plain lines, fifteen at \
             most; no preamble; do not repeat the command. If nothing happened, say so in one \
             sentence.\n"
        );
    } else {
        let _ = writeln!(
            s,
            "You are briefing the person who started {stretch} and is coming back to it; they have not \
             read its output. Answer in exactly three labelled parts, each on its own line, in this \
             order, each one or two sentences, and `none` when there is nothing to say:\n\
             Worked on: what it did and what changed\n\
             Open questions: what needs the person's decision or is unresolved\n\
             Next: what to do next\n\
             {}No other headings, lists, YAML or preamble; do not repeat the command.\n",
            if facts.is_some() {
                "Ground it in the facts below; where the agent's own recap disagrees with them, trust \
                 the facts. "
            } else {
                ""
            }
        );
    }
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
    if let Some(f) = from {
        let _ = writeln!(s, "stretch: since {f}");
    }
    if let Some(events) = events {
        let _ = writeln!(s, "\n## What happened\n");
        if events.is_empty() {
            let _ = writeln!(s, "(no events)");
        }
        // The newest that fit in `EVENTS_BYTES`.
        let said: Vec<String> = events
            .iter()
            .map(|e| format!("{}  {}  {}", e.at, e.kind, e.summary))
            .collect();
        let (mut size, mut from) = (0, said.len());
        while from > 0 && size + said[from - 1].len() < EVENTS_BYTES {
            from -= 1;
            size += said[from].len() + 1;
        }
        if from > 0 {
            let _ = writeln!(s, "({from} earlier left out)");
        }
        for l in &said[from..] {
            let _ = writeln!(s, "{l}");
        }
    }
    if let Some(f) = facts {
        let _ = writeln!(s, "\n## The facts\n");
        let commits: Vec<&Event> = events
            .unwrap_or_default()
            .iter()
            .filter(|e| e.kind == event_kind::COMMIT_LANDED)
            .collect();
        if commits.is_empty() {
            let _ = writeln!(s, "commits: {}", f.commits);
        } else {
            let _ = writeln!(s, "commits: {}", f.commits.max(commits.len() as u32));
            for e in &commits {
                let _ = writeln!(s, "- {}", e.summary);
            }
        }
        let _ = writeln!(
            s,
            "changes: +{}/\u{2212}{} across {} files",
            f.added, f.removed, f.files
        );
        if !f.top_paths.is_empty() {
            let _ = writeln!(s, "top paths: {}", f.top_paths.join(", "));
        }
        let _ = writeln!(s, "errors: {}", f.errors);
        match (&f.branch, f.pushed) {
            (Some(b), true) => {
                let _ = writeln!(s, "branch: {b} (pushed)");
            }
            (Some(b), false) => {
                let _ = writeln!(s, "branch: {b} (not pushed)");
            }
            (None, _) => {}
        }
        if let Some(t) = f.active_secs {
            let _ = writeln!(s, "active time: {}", fmt_duration(SignedDuration::from_secs(t as i64)));
        }
        if let (Some(i), Some(c), Some(o)) = (f.tokens_in, f.tokens_cached, f.tokens_out) {
            let _ = writeln!(s, "tokens: {i} fresh, {c} cached, {o} out");
        }
    }
    if let Some(seed) = seed {
        let _ = writeln!(
            s,
            "\n## Its own recap\n\nThe agent's own words about what it did; check them against the rest.\n\n````\n{}\n````",
            seed.trim_end()
        );
    }
    let _ = writeln!(s, "\n## {heading}\n");
    if material.trim().is_empty() {
        let _ = writeln!(s, "(nothing)");
    } else {
        let _ = writeln!(s, "````\n{}\n````", material.trim_end());
    }
    s
}

/// What the agent is asked of many: what they are, since when, and a heading
/// and a line (or a few) for each.
pub fn prompt_over(what: &str, from: Option<Timestamp>, lines: &[(String, String)]) -> String {
    prompt_over_text(what, from, &over_body(lines))
}

/// The ask of many, over what is said of them (`over_body`, or a part of
/// it).
fn prompt_over_text(what: &str, from: Option<Timestamp>, body: &str) -> String {
    let clock = match from {
        Some(f) => fmt_when(f, Timestamp::now()),
        None => "it began".into(),
    };
    format!(
        "You are telling the person what happened in {what} since {clock}, who has not looked. \
         Answer with 3–6 bullets, most important first: what needs them, then what broke, then \
         what moved or shipped. One line each, no preamble.\n{body}"
    )
}

/// A heading and a line (or a few) for each.
fn over_body(lines: &[(String, String)]) -> String {
    let mut s = String::new();
    for (title, line) in lines {
        let _ = write!(s, "\n## {title}\n{}\n", line.trim_end());
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
    fn a_firing_is_told_as_its_flows() {
        let at: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let mut r = Run::new("r1", RunKind::Command, "brief", vec![], "/w", "/w", at);
        r.cause = Cause::Flow {
            flow: "morning-brief".into(),
            step: "brief".into(),
        };
        let t = SummaryTarget::Firing { id: "fi1".into() };
        assert_eq!(what(&t, &[r]), "one firing of the flow `morning-brief`");
        assert_eq!(what(&t, &[]), "one firing of a flow");
    }

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
        r.cause = Cause::Flow {
            flow: "nightly".into(),
            step: "test".into(),
        };
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
            Some(&[e]),
            None,
            None,
            "Its output (the end of it, at most 64 KB)",
            "test parser ... FAILED\n",
            false,
        );
        assert!(
            p.contains("title: nightly tests\ncommand: make test\nfolder: /w\nwhy: flow:nightly/test\n"),
            "{p}"
        );
        assert!(p.contains("· after 2m30s · failed · exit 2"), "{p}");
        assert!(p.contains("run.failed  exit 2"), "{p}");
        assert!(p.contains("````\ntest parser ... FAILED\n````"), "{p}");
        assert!(p.contains("## Its output (the end of it, at most 64 KB)\n"), "{p}");
        assert!(!p.contains("## The facts") && !p.contains("## Its own recap"), "{p}");
        let from: Timestamp = "2026-09-10T09:00:00Z".parse().unwrap();
        let p = prompt(&r, Some(from), Some(&[]), None, None, "What was said", "", false);
        assert!(p.contains("(no events)") && p.contains("(nothing)"), "{p}");
        assert!(p.contains("what happened since ") && p.contains(" in one run"), "{p}");
        assert!(
            p.contains("stretch: since 2026-09-10T09:00:00Z\n") && p.contains("## What was said\n"),
            "{p}"
        );
        // Deep, it is asked for more; a later part has no events of its own.
        let p = prompt(&r, None, None, None, None, "Part 2 of 3: Its output", "more\n", true);
        assert!(
            p.contains("what changed where") && p.contains("## Part 2 of 3: Its output\n"),
            "{p}"
        );
        assert!(!p.contains("## What happened"), "{p}");
        assert_eq!(tail_of("aébc", 3), "bc", "on a character boundary");
    }

    #[test]
    fn the_brief_asks_for_three_labelled_parts() {
        let t: Timestamp = "2026-09-10T11:00:00Z".parse().unwrap();
        let r = Run::new("r1", RunKind::Command, "t", vec![], "/w", "/w", t);
        let p = prompt(&r, None, None, None, None, "Its output", "x\n", false);
        for label in ["Worked on:", "Open questions:", "Next:", "none"] {
            assert!(p.contains(label), "{label}: {p}");
        }
        assert!(!p.contains("Ground it in the facts"), "no facts section follows: {p}");
        let f = RunFacts::default();
        let p = prompt(&r, None, None, Some(&f), None, "Its output", "x\n", false);
        assert!(p.contains("Ground it in the facts below"), "{p}");
    }

    #[test]
    fn the_facts_and_the_seed_are_sections_of_the_prompt() {
        let t: Timestamp = "2026-09-10T11:00:00Z".parse().unwrap();
        let r = Run::new("r1", RunKind::Command, "t", vec![], "/w", "/w", t);
        let f = RunFacts {
            commits: 3,
            added: 180,
            removed: 40,
            files: 4,
            top_paths: vec!["auth/".into()],
            branch: Some("fix".into()),
            pushed: false,
            active_secs: Some(600),
            tokens_in: Some(10),
            tokens_cached: Some(20),
            tokens_out: Some(5),
            errors: 2,
            ..Default::default()
        };
        let landed = Event {
            id: 1,
            at: t,
            workspace: "/w".into(),
            run: Some("r1".into()),
            group_key: "g".into(),
            kind: event_kind::COMMIT_LANDED.into(),
            summary: "fix the index".into(),
            data: None,
        };
        let p = prompt(
            &r,
            None,
            Some(&[landed]),
            Some(&f),
            Some("All done."),
            "Its output",
            "x\n",
            false,
        );
        for want in [
            "## The facts",
            "commits: 3\n- fix the index",
            "+180/\u{2212}40 across 4 files",
            "top paths: auth/",
            "errors: 2",
            "branch: fix (not pushed)",
            "10 fresh, 20 cached, 5 out",
            "## Its own recap",
            "All done.",
        ] {
            assert!(p.contains(want), "{want}: {p}");
        }
        let p = prompt(&r, None, None, Some(&f), None, "Its output", "x\n", false);
        assert!(p.contains("commits: 3\nchanges"), "the count alone: {p}");
    }

    #[test]
    fn many_are_told_by_a_heading_and_a_line_each() {
        let lines = [
            ("one · 09:00".to_string(), "finished · exit 0 · 2s".to_string()),
            ("two · 09:05".to_string(), "failed · exit 1 · 3s\n".to_string()),
        ];
        let p = prompt_over("the project ~/api", Some(Timestamp::now()), &lines);
        assert!(
            p.starts_with("You are telling the person what happened in the project ~/api since "),
            "{p}"
        );
        assert!(
            p.contains(
                ", who has not looked. Answer with 3–6 bullets, most important first: what needs them, \
                 then what broke, then what moved or shipped. One line each, no preamble.\n"
            ),
            "{p}"
        );
        assert!(
            p.ends_with("\n## one · 09:00\nfinished · exit 0 · 2s\n\n## two · 09:05\nfailed · exit 1 · 3s\n"),
            "{p}"
        );
    }

    #[test]
    fn a_long_read_is_cut_in_parts_on_line_ends() {
        let line = "a line of output to fill the journal\r\n";
        let long = line.repeat(123_000 / line.len());
        let ps = parts(&long);
        assert_eq!(
            ps.len(),
            long.len().div_ceil(CHUNK),
            "{:?}",
            ps.iter().map(|p| p.len()).collect::<Vec<_>>()
        );
        assert!(ps.iter().all(|p| p.len() <= CHUNK && p.ends_with('\n')));
        assert_eq!(ps.concat(), long);
        assert_eq!(parts("short\n"), ["short\n"]);
        // No line ends at all: cut where a chunk ends, on a character.
        let wide = "é".repeat(CHUNK);
        let ps = parts(&wide);
        assert!(ps.len() >= 2 && ps.iter().all(|p| p.len() <= CHUNK));
        assert_eq!(ps.concat(), wide);
    }

    #[test]
    fn a_runs_stretch_is_since_it_was_looked_at_if_it_did_anything_since() {
        let t = |s: &str| s.parse::<Timestamp>().unwrap();
        let mut r = Run::new(
            "r1",
            RunKind::Agent,
            "fix",
            vec![],
            "/w",
            "/w",
            t("2026-09-10T09:00:00Z"),
        );
        assert_eq!(run_from(&r, None, false), None, "never looked at: all of it");
        r.seen_at = Some(t("2026-09-10T10:00:00Z"));
        assert_eq!(
            run_from(&r, None, false),
            None,
            "still going, looked at just now, nothing since: all of it"
        );
        assert_eq!(
            run_from(&r, None, true),
            r.seen_at,
            "still going, something since: since you looked"
        );
        r.ended_at = Some(t("2026-09-10T11:00:00Z"));
        assert_eq!(run_from(&r, None, true), r.seen_at, "ended since you looked");
        r.seen_at = Some(t("2026-09-10T12:00:00Z"));
        assert_eq!(run_from(&r, None, false), None, "looked at once it ended: all of it");
        let asked = Some(t("2026-09-10T09:30:00Z"));
        assert_eq!(run_from(&r, asked, false), asked);
    }

    #[tokio::test]
    async fn a_join_past_chunk_is_joined_in_steps() {
        let said = |i: usize| (format!("run {i} · 09:00"), "x".repeat(2_000));
        let lines: Vec<(String, String)> = (0..40).map(said).collect();
        assert!(over_body(&lines).len() > CHUNK);
        let mut asked: Vec<String> = Vec::new();
        let (a, calls) = join("the project ~/api", None, &lines, |q| {
            asked.push(q);
            let n = asked.len();
            async move {
                Ok(Answer {
                    agent: "fakeai".into(),
                    by: format!("a{n}"),
                    text: "- it went".into(),
                })
            }
        })
        .await
        .unwrap();
        assert!(calls > 1, "{calls}");
        assert_eq!(calls as usize, asked.len());
        assert_eq!(a.by, format!("a{calls}"), "the last call is the answer");
        assert!(asked.iter().all(|q| q.len() <= CHUNK + 1024), "each call fits");
        let last = asked.last().unwrap();
        assert!(last.contains("## Part 1 of 2\n- it went\n"), "{last}");
        // What fits is one call.
        let (_, calls) = join("the project ~/api", None, &lines[..3], |_| async {
            Ok(Answer {
                agent: "fakeai".into(),
                by: "a".into(),
                text: "- it went".into(),
            })
        })
        .await
        .unwrap();
        assert_eq!(calls, 1);
    }

    #[test]
    fn many_runs_brief_fit_in_one_call() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let at = |s: i64| now - SignedDuration::from_secs(s);
        let mut runs = Vec::new();
        let mut lines = Vec::new();
        // The flow ran last; the 300 others before it.
        for i in 0..14 {
            let mut r = Run::new(
                format!("n{i:03}"),
                RunKind::Command,
                "nightly",
                vec![],
                "/w",
                "/w",
                at(14 - i),
            );
            r.cause = Cause::Flow {
                flow: "nightly".into(),
                step: "test".into(),
            };
            r.state = if i < 2 { RunState::Failed } else { RunState::Finished };
            runs.push(r);
            lines.push(format!("{} · exit {}", runs[i as usize].state, i));
        }
        for i in 0..300 {
            let title = format!("{i} {}", "a long title ".repeat(10));
            runs.push(Run::new(
                format!("m{i:03}"),
                RunKind::Command,
                &title,
                vec![],
                "/w",
                "/w",
                at(1000 - i),
            ));
            lines.push(format!("finished · {}", "it said a great deal ".repeat(15)));
        }
        let project = SummaryTarget::Project { workspace: "/w".into() };
        let told = many_lines(&project, &runs, lines.clone(), now);
        assert_eq!(
            told.len(),
            LINES_MAX + 1,
            "the earlier ones counted, then a line each, the flow's folded"
        );
        assert_eq!(told[0], ("… and 101 earlier".to_string(), String::new()));
        assert_eq!(
            told[1],
            (
                "nightly".to_string(),
                "14 runs · 2 failed · last: finished · exit 13".to_string()
            )
        );
        assert!(
            told[2].0.starts_with("101 a long title"),
            "the newest kept: {:?}",
            told[2]
        );
        assert!(over_body(&told).len() < CHUNK, "{}", over_body(&told).len());
        // A flow told is told run by run.
        let flow = SummaryTarget::Flow { name: "nightly".into() };
        let told = many_lines(&flow, &runs[..14], lines[..14].to_vec(), now);
        assert_eq!(told.len(), 14);
    }

    #[test]
    fn a_busy_flow_is_told_by_its_newest_runs() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let (mut runs, mut lines) = (Vec::new(), Vec::new());
        for i in 0..250 {
            let at = now - SignedDuration::from_mins(250 - i);
            let mut r = Run::new(format!("n{i:03}"), RunKind::Command, "poll", vec![], "/w", "/w", at);
            r.cause = Cause::Flow {
                flow: "poll".into(),
                step: "test".into(),
            };
            runs.push(r);
            lines.push(format!("finished · run {i}"));
        }
        let flow = SummaryTarget::Flow { name: "poll".into() };
        let told = many_lines(&flow, &runs, lines, now);
        assert_eq!(told.len(), LINES_MAX + 1);
        assert_eq!(told[0], ("… and 50 earlier".to_string(), String::new()));
        assert_eq!(told[1].1, "finished · run 50");
        assert_eq!(told[LINES_MAX].1, "finished · run 249", "the last run is told");
    }

    #[test]
    fn a_flow_keeps_its_newest_runs_wherever_they_ran() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let (mut runs, mut lines) = (Vec::new(), Vec::new());
        // By folder, then as they started: /a ran last, /b before it.
        for (ws, base) in [("/a", 0), ("/b", 1000)] {
            for i in 0..150 {
                let at = now - SignedDuration::from_mins(base + 150 - i);
                let mut r = Run::new(format!("{ws}{i:03}"), RunKind::Command, "poll", vec![], ws, ws, at);
                r.cause = Cause::Flow {
                    flow: "poll".into(),
                    step: "test".into(),
                };
                runs.push(r);
                lines.push(format!("finished · {ws} {i}"));
            }
        }
        let flow = SummaryTarget::Flow { name: "poll".into() };
        let told = many_lines(&flow, &runs, lines, now);
        assert_eq!(told.len(), LINES_MAX + 1, "one count of the earlier ones");
        assert_eq!(told[0], ("… and 100 earlier".to_string(), String::new()));
        let of = |ws: &str| told.iter().filter(|(_, l)| l.contains(ws)).count();
        assert_eq!((of("/a "), of("/b ")), (150, 50), "the newest, wherever they ran");
    }

    #[test]
    fn everything_keeps_the_newest_of_each_folder() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let (mut runs, mut lines) = (Vec::new(), Vec::new());
        // A busy folder sorts first; a quiet one, after it, ran earlier.
        for i in 0..300 {
            let at = now - SignedDuration::from_secs(300 - i);
            runs.push(Run::new(
                format!("a{i:03}"),
                RunKind::Command,
                format!("busy {i}"),
                vec![],
                "/a",
                "/a",
                at,
            ));
            lines.push(format!("finished · {i}"));
        }
        for i in 0..5 {
            let at = now - SignedDuration::from_hours(5 - i);
            runs.push(Run::new(
                format!("b{i:03}"),
                RunKind::Command,
                format!("quiet {i}"),
                vec![],
                "/b",
                "/b",
                at,
            ));
            lines.push(format!("finished · {i}"));
        }
        let told = many_lines(&SummaryTarget::All, &runs, lines, now);
        let of = |p: &str| told.iter().filter(|(h, _)| h.starts_with(p)).count();
        assert_eq!(of("quiet "), 5, "the quiet folder is told whole");
        assert_eq!(of("busy "), LINES_MAX - 5);
        assert_eq!(told[0], ("… and 105 earlier · /a".to_string(), String::new()));
        assert!(told[1].0.starts_with("busy 105 · /a"), "{:?}", told[1]);
        assert!(
            told.iter().any(|(h, _)| h.starts_with("busy 299")),
            "the newest of the busy one"
        );
    }

    #[test]
    fn a_runs_events_are_the_newest_that_fit() {
        let t: Timestamp = "2026-09-10T11:00:00Z".parse().unwrap();
        let r = Run::new("r1", RunKind::Command, "loud", vec![], "/w", "/w", t);
        let evs: Vec<Event> = (0..500)
            .map(|i| Event {
                id: i,
                at: t,
                workspace: "/w".into(),
                run: Some("r1".into()),
                group_key: "g".into(),
                kind: "run.emitted".into(),
                summary: format!("event {i} {}", "said ".repeat(20)),
                data: None,
            })
            .collect();
        let p = prompt(&r, None, Some(&evs), None, None, "Its output", "out\n", true);
        let happened = &p[p.find("## What happened").unwrap()..p.find("## Its output").unwrap()];
        assert!(happened.len() <= EVENTS_BYTES + 64, "{}", happened.len());
        assert!(happened.contains("earlier left out)\n"), "{happened}");
        assert!(happened.contains("event 499 "), "the newest kept");
        assert!(!happened.contains("event 0 "), "the oldest left out");
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
