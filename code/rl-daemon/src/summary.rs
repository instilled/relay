//! What happened, said for the person who was not looking: of a run, of a
//! job's runs (a schedule's or a reaction's), of a project's, or of
//! everything, over a stretch. Only ever on demand (`/summary` in the panel,
//! `rl summary`, or a reaction on `run.finished`) — Relay never asks an agent
//! on its own.
//!
//! Brief, a run is told from the cheapest source that has one:
//! - Claude writes its own recap into its transcript; the latest in the
//!   stretch is taken as it stands, no assistant asked;
//! - Codex keeps no recap, but its turns say what it last said and did: with
//!   `assistants.summary` set, an assistant condenses them, else they are
//!   the recap as facts;
//! - anything else — and Claude with no recap in the stretch — is read by
//!   `assistants.summary`: what was said, or the end of the output.
//!
//! Deep, a run is always read by the assistant, all of the stretch: Claude's
//! messages and tool calls, Codex's turns, or the whole output, with Relay's
//! events. Past `CHUNK` it is read in parts, each summarised, then those
//! joined — so a weekend of a busy agent is still one answer.
//!
//! Many runs (a job, a project, everything) are told from their runs' lines
//! in one call when brief; deep, from each run's deep read (kept ones as they
//! are) and one call to join them. `dry` only counts the calls.
//!
//! The stretch, unless given, is since you last looked: a run's `seen_at`
//! when it has written or had an event since, else all of it; a job's, from
//! the start of its oldest firing not seen, else the last day. A project or everything needs one given — the
//! client knows when you were last here. A stretch with nothing in it says so
//! without asking anyone.
//!
//! The one kept is given back while what it covers has not moved on: a run's
//! brief recap of the whole of it in `summaries` (where the agent's own recap
//! is also kept as it is written, `recap.rs`), everything else in
//! `summary_kept` by (what, how deep, from when). Never of an assistant's own
//! run, so a reaction cannot feed itself.

use crate::assistant;
use crate::server::Daemon;
use crate::transcript::{self, Agent};
use anyhow::{Result, bail};
use jiff::{SignedDuration, Timestamp, tz::TimeZone};
use rl_core::display_path;
use rl_core::time::fmt_duration;
use rl_ledger::events::NewEvent;
use rl_ledger::{events, kept, runs, session_io, summaries};
use rl_proto::methods::{AssistantParams, EventFilter, SummaryParams};
use rl_proto::types::{Cause, Event, Run, Summary, SummaryTarget, event_kind};
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
/// How far back a job is read when every firing has been seen.
const JOB_STRETCH: SignedDuration = SignedDuration::from_hours(24);

/// The one kept for the target, depth and stretch, marked stale when what it
/// covers has moved on since. A run's default stretch with no answer of its
/// own yet gives back the run's recap.
pub async fn get(d: &Daemon, p: &SummaryParams) -> Result<Option<Summary>> {
    let from = p.from.map(whole);
    match &p.target {
        SummaryTarget::Run { id } => {
            let run = load(d, id).await?;
            let from = match &run {
                Some(r) => run_stretch(d, r, from).await?,
                None => from,
            };
            if !p.deep && from.is_none() {
                return run_kept(d, id).await;
            }
            let Some(mut s) = kept_of(d, &p.target, p.deep, from).await? else {
                if !p.deep && p.from.is_none() {
                    return run_kept(d, id).await;
                }
                return Ok(None);
            };
            if let Some(r) = &run {
                let t = locate(d, r).await?;
                s.stale = reach(d, r, t.as_deref()).await? != s.reach;
            }
            Ok(Some(s))
        }
        t => {
            // A project or everything with no stretch has nothing kept for it.
            let Ok(from) = over_from(d, t, from, now()).await else {
                return Ok(None);
            };
            let Some(mut s) = kept_of(d, t, p.deep, Some(from)).await? else {
                return Ok(None);
            };
            s.stale = many_reach(&in_stretch(d, t, from).await?) != s.reach;
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
    d.ledger.call(move |c| runs::get(c, &id)).await
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
        .call(move |c| {
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
    let Some(mut s) = d.ledger.call(move |c| summaries::get(c, &id)).await? else {
        return Ok(None);
    };
    s.target = SummaryTarget::Run { id: run.to_string() }.key();
    s.calls = u32::from(!s.by.is_empty());
    let id = run.to_string();
    if let Some(r) = d.ledger.call(move |c| runs::get(c, &id)).await?
        && !s.reach.is_empty()
    {
        let t = locate(d, &r).await?;
        s.stale = reach(d, &r, t.as_deref()).await? != s.reach;
    }
    Ok(Some(s))
}

/// The one kept in `summary_kept`, with the run it is of, if any.
async fn kept_of(d: &Daemon, t: &SummaryTarget, deep: bool, from: Option<Timestamp>) -> Result<Option<Summary>> {
    let key = t.key();
    let found = d.ledger.call(move |c| kept::get(c, &key, deep, from)).await?;
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

/// How far many runs have got: how many, and the latest change of state.
fn many_reach(runs: &[Run]) -> String {
    let last = runs.iter().map(|r| r.state_at.as_millisecond()).max().unwrap_or(0);
    format!("n:{}:{last}", runs.len())
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
    }
}

/// A stretch with nothing in it, said without asking anyone.
fn nothing(s: &mut Summary) {
    s.agent = "relay".into();
    if s.source.is_empty() {
        s.source = "nothing new".into();
    }
    s.text = match s.from {
        Some(f) => format!("nothing happened since {}", clock(f, s.at)),
        None => "Nothing to go on yet.".into(),
    };
}

/// What a dry run says of one kept or free: no calls, no text.
fn dry_of(mut s: Summary) -> Summary {
    s.text.clear();
    s.calls = 0;
    s
}

/// `22:14` today, `Fri 18:40` this week, `Sep 07 18:40` before. Local time.
fn clock(t: Timestamp, now: Timestamp) -> String {
    let tz = TimeZone::system();
    let (z, n) = (t.to_zoned(tz.clone()), now.to_zoned(tz));
    if z.date() == n.date() {
        z.strftime("%H:%M").to_string()
    } else if now.duration_since(t) < SignedDuration::from_hours(6 * 24) {
        z.strftime("%a %H:%M").to_string()
    } else {
        z.strftime("%b %d %H:%M").to_string()
    }
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
        .call(move |c| match from {
            Some(f) => session_io::tail_since(c, &id, f, max),
            None => session_io::tail(c, &id, max),
        })
        .await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Relay's events for a run over the stretch, at most `limit`.
async fn run_events(d: &Daemon, run: &Run, from: Option<Timestamp>, limit: u32) -> Result<Vec<Event>> {
    let id = run.id.clone();
    d.ledger
        .call(move |c| {
            events::list(
                c,
                &EventFilter {
                    run: Some(id),
                    since: from,
                    limit: Some(limit),
                    ..Default::default()
                },
            )
        })
        .await
}

/// Where the assistant runs for a run: its folder, if it is still there.
fn run_cwd(d: &Daemon, run: &Run) -> PathBuf {
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
    // What an assistant would read, when the agent's own record does not do.
    let (heading, material) = match (Agent::of(run), body) {
        (Some(Agent::Claude), Some(body)) => {
            if let Some(r) = transcript::claude_recap(&body, from) {
                made.agent = "claude".into();
                made.source = "claude recap".into();
                made.text = r.text;
                return done(d, Some(run), made, p.dry).await;
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
                return done(d, Some(run), made, p.dry).await;
            }
            made.source = "codex turns".into();
            ("Its turns".to_string(), transcript::codex_facts(&turns))
        }
        _ => (
            format!("Its output (the end of it, at most {} KB)", OUTPUT_TAIL / 1024),
            output(d, run, from, OUTPUT_TAIL).await?,
        ),
    };
    if material.trim().is_empty() {
        nothing(&mut made);
        return done(d, Some(run), made, p.dry).await;
    }
    made.calls = 1;
    if p.dry {
        return Ok(made);
    }
    let evs = run_events(d, run, from, EVENTS_BRIEF).await?;
    let prompt = prompt(run, from, Some(&evs), &heading, tail_of(&material, OUTPUT_TAIL), false);
    let a = ask(d, run_cwd(d, run), &run.title, prompt, p).await?;
    if made.source.is_empty() {
        made.source = "summarised".into();
    }
    (made.agent, made.by, made.text) = (a.agent, a.by, a.text);
    keep(d, Some(run), made).await
}

/// A run's deep read kept for this stretch, while the run has not moved on.
async fn deep_kept(d: &Daemon, run: &Run, from: Option<Timestamp>) -> Result<Option<Summary>> {
    let file = locate(d, run).await?;
    let reach = reach(d, run, file.as_deref()).await?;
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
    let evs = run_events(d, run, from, EVENTS_DEEP).await?;
    let cwd = run_cwd(d, run);
    let a = if n == 1 {
        ask(
            d,
            cwd,
            &run.title,
            prompt(run, from, Some(&evs), &heading, parts[0], true),
            p,
        )
        .await?
    } else {
        let mut said = Vec::with_capacity(n);
        for (i, part) in parts.iter().enumerate() {
            let at = format!("Part {} of {n}", i + 1);
            let evs = (i == 0).then_some(evs.as_slice());
            let q = prompt(run, from, evs, &format!("{at}: {heading}"), part, true);
            said.push((at, ask(d, cwd.clone(), &run.title, q, p).await?.text));
        }
        let what = format!("the run `{}`", run.title);
        ask(d, cwd, &run.title, prompt_over(&what, from, &said), p).await?
    };
    made.source = if n == 1 {
        "summarised".into()
    } else {
        format!("read in {n} parts")
    };
    (made.agent, made.by, made.text) = (a.agent, a.by, a.text);
    keep(d, Some(run), made).await
}

/// Where many runs' stretch begins: as asked; for a job, the start of its
/// oldest firing not seen, else the last day; a project and everything need one.
async fn over_from(d: &Daemon, t: &SummaryTarget, asked: Option<Timestamp>, now: Timestamp) -> Result<Timestamp> {
    if let Some(f) = asked {
        return Ok(f);
    }
    match t {
        SummaryTarget::Job { rule, name, workspace } => {
            let (cause, ws) = (format!("{rule}:{name}"), workspace.clone());
            let oldest = d
                .ledger
                .call(move |c| runs::oldest_unseen(c, &cause, ws.as_deref()))
                .await?;
            Ok(oldest.unwrap_or(now - JOB_STRETCH))
        }
        SummaryTarget::Project { .. } => bail!("a project's summary needs where to start (from)"),
        SummaryTarget::All => bail!("a summary of everything needs where to start (from)"),
        SummaryTarget::Run { .. } => bail!("a run's stretch is its own"),
    }
}

/// The runs a job, a project or everything has in the stretch: those that
/// moved in it or are still going, never an assistant's own; by folder, then
/// as they started.
async fn in_stretch(d: &Daemon, t: &SummaryTarget, from: Timestamp) -> Result<Vec<Run>> {
    let found = d.ledger.call(move |c| runs::list_for_digest(c, from)).await?;
    let mut runs: Vec<Run> = found
        .into_iter()
        .filter(|r| !matches!(r.cause, Cause::Assistant(_)) && r.ended_at.is_none_or(|e| e >= from))
        .filter(|r| match t {
            SummaryTarget::Job { rule, name, workspace } => {
                r.cause.to_string() == format!("{rule}:{name}") && workspace.as_ref().is_none_or(|w| *w == r.workspace)
            }
            SummaryTarget::Project { workspace } => r.workspace == *workspace,
            SummaryTarget::All => true,
            SummaryTarget::Run { id } => r.id == *id,
        })
        .collect();
    runs.sort_by(|a, b| (&a.workspace, a.started_at, &a.id).cmp(&(&b.workspace, b.started_at, &b.id)));
    Ok(runs)
}

/// What many runs are, for the assistant.
fn what(t: &SummaryTarget) -> String {
    match t {
        SummaryTarget::Job { rule, name, .. } => format!("the {rule} `{name}`"),
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
            clock(r.started_at, now)
        ),
        _ => format!("{} · {}", r.title, clock(r.started_at, now)),
    }
}

/// Each run's line as the list says it: its recap, else where it stands.
async fn brief_lines(d: &Daemon, runs: &[Run]) -> Result<Vec<String>> {
    let runs = runs.to_vec();
    d.ledger
        .call(move |c| {
            let ids: Vec<String> = runs.iter().map(|r| r.id.clone()).collect();
            let recaps = summaries::lines(c, &ids)?;
            runs.iter()
                .map(|r| {
                    let line = match recaps.get(&r.id) {
                        Some(l) => l.clone(),
                        None => {
                            let last = events::latest_for_run(c, &r.id)?;
                            let emitted = events::emitted_for_run(c, &r.id)?;
                            crate::digest::summarise(r, last.as_ref(), &emitted)
                        }
                    };
                    Ok(format!("{} · {line}", r.state))
                })
                .collect()
        })
        .await
}

/// A job's, a project's or everything's runs: one call over their lines,
/// brief; deep, each run's deep read first (kept ones as they are), then one
/// call to join them.
async fn over(d: &Daemon, t: &SummaryTarget, from: Option<Timestamp>, p: &SummaryParams) -> Result<Summary> {
    let now = now();
    let from = over_from(d, t, from, now).await?;
    let runs = in_stretch(d, t, from).await?;
    let reach = many_reach(&runs);
    if !p.again
        && let Some(k) = kept_of(d, t, p.deep, Some(from)).await?.filter(|k| k.reach == reach)
    {
        return Ok(if p.dry { dry_of(k) } else { k });
    }
    let mut made = blank(t, "", p.deep, Some(from), reach);
    if runs.is_empty() {
        nothing(&mut made);
        return done(d, None, made, p.dry).await;
    }
    // A run that began in the stretch is read whole, so its deep read is the
    // one kept for it.
    let stretch = |r: &Run| (r.started_at < from).then_some(from);
    let lines = if !p.deep {
        if p.dry {
            made.calls = 1;
            return Ok(made);
        }
        brief_lines(d, &runs).await?
    } else if p.dry {
        let mut calls = 1;
        for r in &runs {
            if deep_kept(d, r, stretch(r)).await?.is_none() {
                calls += 1;
            }
        }
        made.calls = calls;
        return Ok(made);
    } else {
        let mut lines = Vec::with_capacity(runs.len());
        for r in &runs {
            let s = match deep_kept(d, r, stretch(r)).await? {
                Some(k) => k,
                None => {
                    let fresh = SummaryParams {
                        target: SummaryTarget::Run { id: r.id.clone() },
                        deep: true,
                        from: stretch(r),
                        again: true,
                        dry: false,
                        timeout: p.timeout.clone(),
                    };
                    let s = run_deep(d, r, stretch(r), &fresh).await?;
                    made.calls += s.calls;
                    s
                }
            };
            lines.push(s.text);
        }
        lines
    };
    let lines: Vec<(String, String)> = runs.iter().map(|r| heading(t, r, now)).zip(lines).collect();
    let what = what(t);
    let cwd = match t {
        SummaryTarget::Project { workspace }
        | SummaryTarget::Job {
            workspace: Some(workspace),
            ..
        } if workspace.is_dir() => workspace.clone(),
        _ => d.home.path().to_path_buf(),
    };
    let a = ask(d, cwd, &what, prompt_over(&what, Some(from), &lines), p).await?;
    made.calls += 1;
    made.source = format!("over {} run{}", runs.len(), if runs.len() == 1 { "" } else { "s" });
    (made.agent, made.by, made.text) = (a.agent, a.by, a.text);
    keep(d, None, made).await
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

/// Keeps a summary — a run's brief recap of the whole of it by the run,
/// anything else by what, how deep and from when — and says so on the run.
async fn keep(d: &Daemon, run: Option<&Run>, s: Summary) -> Result<Summary> {
    let run = run.cloned();
    d.ledger
        .call({
            let s = s.clone();
            move |c| {
                match &run {
                    Some(_) if !s.deep && s.from.is_none() => summaries::put(c, &s)?,
                    _ => kept::put(c, &s)?,
                }
                if let Some(run) = &run {
                    let e = NewEvent::for_run(run, s.at, event_kind::RUN_SUMMARISED, first_line(&s.text));
                    events::append(c, &e)?;
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
pub fn prompt(
    run: &Run,
    from: Option<Timestamp>,
    events: Option<&[Event]>,
    heading: &str,
    material: &str,
    deep: bool,
) -> String {
    let mut s = String::new();
    let stretch = match from {
        Some(f) => format!("what happened since {} in one run", clock(f, Timestamp::now())),
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
            "You are summarising {stretch} for the person who started it, who has not read its output. \
             Answer in plain prose, three sentences at most: what it did, what changed, and what is \
             left or went wrong. No headings, lists, YAML or preamble; do not repeat the command. If \
             nothing happened, say so in one sentence.\n"
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
        for e in events {
            let _ = writeln!(s, "{}  {}  {}", e.at, e.kind, e.summary);
        }
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
    let clock = match from {
        Some(f) => clock(f, Timestamp::now()),
        None => "it began".into(),
    };
    let mut s = format!(
        "You are telling the person what happened in {what} since {clock}, who has not looked. \
         Answer with 3–6 bullets, most important first: what needs them, then what broke, then \
         what moved or shipped. One line each, no preamble.\n"
    );
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
            Some(&[e]),
            "Its output (the end of it, at most 64 KB)",
            "test parser ... FAILED\n",
            false,
        );
        assert!(
            p.contains("title: nightly tests\ncommand: make test\nfolder: /w\nwhy: schedule:nightly\n"),
            "{p}"
        );
        assert!(p.contains("· after 2m30s · failed · exit 2"), "{p}");
        assert!(p.contains("run.failed  exit 2"), "{p}");
        assert!(p.contains("````\ntest parser ... FAILED\n````"), "{p}");
        assert!(p.contains("## Its output (the end of it, at most 64 KB)\n"), "{p}");
        assert!(p.contains("three sentences at most"), "{p}");
        let from: Timestamp = "2026-09-10T09:00:00Z".parse().unwrap();
        let p = prompt(&r, Some(from), Some(&[]), "What was said", "", false);
        assert!(p.contains("(no events)") && p.contains("(nothing)"), "{p}");
        assert!(p.contains("what happened since ") && p.contains(" in one run"), "{p}");
        assert!(
            p.contains("stretch: since 2026-09-10T09:00:00Z\n") && p.contains("## What was said\n"),
            "{p}"
        );
        // Deep, it is asked for more; a later part has no events of its own.
        let p = prompt(&r, None, None, "Part 2 of 3: Its output", "more\n", true);
        assert!(
            p.contains("what changed where") && p.contains("## Part 2 of 3: Its output\n"),
            "{p}"
        );
        assert!(!p.contains("## What happened"), "{p}");
        assert_eq!(tail_of("aébc", 3), "bc", "on a character boundary");
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
