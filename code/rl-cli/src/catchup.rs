//! The catch-up: what happened while you were away, as the list's own rows.
//!
//! The list's rows for the stretch, under the list's own headers: a question,
//! a run waiting on you, a run with news since you left — or that you have
//! not looked at yet — and a flow's runs in that time; never an assistant's
//! own run, which is Relay's doing. What needs you leads each project, and
//! the projects that need you lead; a project with nothing new keeps its
//! header and folds to one line. Counted — only what happened
//! in the stretch is counted as ran or failed — they are the top without an
//! assistant; in a line each, what an assistant is handed to write it.
//! Agents' own sessions are not in it: the list has them.

pub mod heads;

use crate::client::Client;
use crate::digest::since_items;
use crate::tui::rows::{self, Context, Head, Row};
use jiff::{SignedDuration, Timestamp};
use rl_core::time::{fmt_age, fmt_duration, fmt_when};
use rl_proto::decision::Decision;
use rl_proto::flow::Flow;
use rl_proto::methods::{ConfigGet, ConfigReport, Digest, DigestItem, SummaryGet, SummaryParams};
use rl_proto::types::{Cause, Run, RunState, Summary, SummaryTarget};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// How many of a flow's firings in the stretch its line shows as marks.
const MARKS: usize = 10;

#[cfg(test)]
thread_local! {
    /// How many catch-ups were built on this thread, for the tests that
    /// count them.
    pub static BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub struct CatchUp<'a> {
    pub since: Timestamp,
    pub projects: Vec<Project<'a>>,
    /// When it was built, for the ages on its lines.
    now: Timestamp,
}

pub struct Project<'a> {
    pub head: Head,
    /// The list's group key: a ref, a folder, or `everywhere`'s.
    key: String,
    /// Questions, runs that need you, and flows whose last run failed.
    pub need: usize,
    /// Runs, a flow's too, in the stretch.
    pub ran: usize,
    /// Of those, the ones that failed.
    pub failed: usize,
    pub rows: Vec<Row<'a>>,
}

impl<'a> Project<'a> {
    fn new(head: Head, key: String) -> Self {
        Self {
            head,
            key,
            need: 0,
            ran: 0,
            failed: 0,
            rows: Vec::new(),
        }
    }

    /// When the newest thing in it happened.
    fn latest(&self) -> Option<Timestamp> {
        self.rows.iter().filter_map(at).max()
    }
}

impl<'a> CatchUp<'a> {
    /// The list's rows for the stretch, grouped as the list groups them; what
    /// needs you first, projects that need you first, a project with nothing
    /// new keeps an empty `rows`.
    pub fn build(d: &'a Digest, cx: &Context<'_>) -> Self {
        #[cfg(test)]
        BUILT.with(|b| b.set(b.get() + 1));
        // The whole list, whatever the toggles are now but narrowed by its
        // search, with its day of ended runs counted back from when you left
        // rather than from now, so a long stretch keeps what ended early in
        // it.
        let list = Context {
            names: cx.names,
            flows: cx.flows,
            parked: cx.parked,
            filter: cx.filter,
            show_flows: true,
            show_agents: true,
            show_shells: true,
            show_hidden: false,
            now: d.since,
            relay: cx.relay,
        };
        let since = d.since;
        let news: HashSet<&str> = since_items(d).into_iter().map(|i| i.run.id.as_str()).collect();
        let news = |i: &DigestItem| news.contains(i.run.id.as_str());
        let waiting = |i: &DigestItem| matches!(i.run.state, RunState::NeedsInput | RunState::Blocked);
        // A flow heading the catch-up is told there, not again under its project.
        let heading: HashSet<&str> = heads::leads(cx.flows, &d.firings, since)
            .iter()
            .map(|l| l.flow)
            .collect();
        let mut projects: Vec<Project<'a>> = Vec::new();
        // Whether the last header's key has been read off a row under it.
        let mut keyed = true;
        // Runs pinned or come through, and their questions, stand above
        // every header on the list; here they go under their project's.
        let mut loose: Vec<Row<'a>> = Vec::new();
        for row in rows::build(d, &list) {
            if let (false, Some(p), Some(r)) = (keyed, projects.last_mut(), run_of(&row)) {
                p.key = rows::group_key(r, cx.names, cx.flows);
                keyed = true;
            }
            let row = match row {
                Row::Group(h) => {
                    let key = h.workspace.to_string_lossy().into_owned();
                    projects.push(Project::new(h, key));
                    keyed = false;
                    continue;
                }
                Row::Question(q) => Row::Question(q),
                // An assistant's own run — the top being written, a recap —
                // is Relay's doing, not what happened.
                Row::Run(i) if matches!(i.run.cause, Cause::Assistant(_)) => continue,
                Row::Run(i) if news(i) || waiting(i) => Row::Run(i),
                Row::Run(_) => continue,
                // Agents' own sessions are not news a catch-up tells: what
                // is open now is the list's.
                Row::Outside(_) => continue,
                Row::Flow(mut f) => {
                    if heading.contains(f.name.as_str()) || !f.runs.iter().any(|i| news(i)) {
                        continue;
                    }
                    // Its marks speak for its runs in the stretch; one you
                    // have not looked at from before it keeps the row, and
                    // is all it shows when there is nothing since.
                    let stretch: Vec<&DigestItem> = f.runs.iter().copied().filter(|i| i.at >= since).collect();
                    if stretch.is_empty() {
                        f.runs.retain(|i| i.unseen);
                    } else {
                        f.runs = stretch;
                    }
                    Row::Flow(f)
                }
            };
            match projects.last_mut() {
                Some(p) => p.rows.push(row),
                None => loose.push(row),
            }
        }
        for row in loose {
            let (run, ws) = match &row {
                Row::Question(q) => {
                    let asker = q.run.as_deref().and_then(|id| d.items.iter().find(|i| i.run.id == id));
                    (asker.map(|i| &i.run), q.workspace.as_path())
                }
                Row::Run(i) => (Some(&i.run), i.run.workspace.as_path()),
                Row::Group(_) | Row::Flow(_) | Row::Outside(_) => continue,
            };
            // Under the header the list would give it: its ref's, else its
            // folder's — not another folder that goes by the same name.
            let key = match run {
                Some(r) => rows::group_key(r, cx.names, cx.flows),
                None => ws.to_string_lossy().into_owned(),
            };
            let p = match projects.iter().position(|p| p.key == key) {
                Some(p) => p,
                None => {
                    projects.push(Project::new(head_of(d, cx, run, ws), key));
                    projects.len() - 1
                }
            };
            projects[p].rows.push(row);
        }
        // A header outside every registered folder stays only when something
        // happened under it: not for flows that are merely there, or ran long ago.
        projects.retain(|p| !p.rows.is_empty() || cx.names.registered(&p.head.workspace).is_some());
        let ran = |i: &DigestItem| i.at >= since;
        let failed = |i: &DigestItem| ran(i) && i.run.state == RunState::Failed;
        for p in &mut projects {
            p.rows.sort_by_key(|r| (tier(r), std::cmp::Reverse(at(r))));
            for r in &p.rows {
                match r {
                    Row::Question(_) => p.need += 1,
                    Row::Run(i) => {
                        p.need += usize::from(i.run.state.needs_attention());
                        p.ran += usize::from(ran(i));
                        p.failed += usize::from(failed(i));
                    }
                    Row::Flow(f) => {
                        p.need += usize::from(f.failed_last());
                        p.ran += f.runs.iter().filter(|i| ran(i)).count();
                        p.failed += f.runs.iter().filter(|i| failed(i)).count();
                    }
                    Row::Group(_) | Row::Outside(_) => {}
                }
            }
        }
        projects.sort_by(|a, b| b.need.cmp(&a.need).then_with(|| b.latest().cmp(&a.latest())));
        Self {
            since: d.since,
            projects,
            now: cx.now,
        }
    }

    /// The catch-up of `d` as `rl since` builds it: the folders by the names
    /// they were registered under, the flows as they stand, nothing filtered;
    /// `relay`, the Relay home.
    pub fn of(
        d: &'a Digest,
        workspaces: impl IntoIterator<Item = (PathBuf, String)>,
        flows: &[Flow],
        relay: Option<&Path>,
        now: Timestamp,
    ) -> Self {
        let names = rows::Names::new(workspaces);
        Self::build(
            d,
            &Context {
                names: &names,
                flows,
                parked: &[],
                filter: "",
                show_flows: true,
                show_agents: true,
                show_shells: true,
                show_hidden: false,
                now,
                relay,
            },
        )
    }

    /// The top without an assistant: a bullet per project with something to
    /// say, from counts.
    pub fn bullets(&self) -> Vec<String> {
        let mut out = Vec::new();
        let need: usize = self.projects.iter().map(|p| p.need).sum();
        if need > 0 {
            let who: Vec<&str> = self
                .projects
                .iter()
                .filter(|p| p.need > 0)
                .map(|p| p.head.name.as_str())
                .collect();
            let s = if need == 1 { "s" } else { "" };
            out.push(format!("{need} need{s} you: {}", who.join(", ")));
        }
        for p in self.projects.iter().filter(|p| !p.rows.is_empty()) {
            let mut said = Vec::new();
            if p.ran > 0 {
                said.push(format!("{} ran", p.ran));
            }
            if p.failed > 0 {
                said.push(format!("{} failed", p.failed));
            }
            if !said.is_empty() {
                out.push(format!("{}: {}", p.head.name, said.join(" · ")));
            }
        }
        out
    }

    /// The top from counts, a bullet a line; `nothing happened` when there
    /// is nothing to count.
    pub fn counted(&self) -> Vec<String> {
        let b = self.bullets();
        if b.is_empty() {
            return vec!["• nothing happened".into()];
        }
        b.iter().map(|b| format!("• {b}")).collect()
    }

    /// Whether a run or a flow is in it: questions alone give an assistant
    /// nothing to tell, and the counts say them.
    pub fn has_runs(&self) -> bool {
        self.projects
            .iter()
            .flat_map(|p| &p.rows)
            .any(|r| matches!(r, Row::Run(_) | Row::Flow(_)))
    }

    /// What the assistant is handed for the top: (project name, its rows'
    /// lines). A project with nothing new hands nothing.
    pub fn lines(&self) -> Vec<(String, String)> {
        self.projects
            .iter()
            .filter(|p| !p.rows.is_empty())
            .map(|p| {
                let lines: Vec<String> = p.rows.iter().map(|r| row_line(r, self.now)).collect();
                (p.head.name.clone(), lines.join("\n"))
            })
            .collect()
    }
}

/// A catch-up by where its rows stand in the digest it was built from,
/// rather than borrowing it: kept beside that digest, it is laid back over it
/// (`over`) without being built again.
pub struct Kept {
    since: Timestamp,
    now: Timestamp,
    projects: Vec<KeptProject>,
}

struct KeptProject {
    head: Head,
    key: String,
    need: usize,
    ran: usize,
    failed: usize,
    rows: Vec<Slot>,
}

/// A row by its place: an item's or a question's index in the digest.
enum Slot {
    Group(Head),
    Question(usize),
    Run(usize),
    Flow {
        key: String,
        name: String,
        when: Option<String>,
        runs: Vec<usize>,
    },
}

impl CatchUp<'_> {
    /// This catch-up, kept by place in `d`, the digest it was built from.
    pub fn keep(&self, d: &Digest) -> Kept {
        let items: HashMap<*const DigestItem, usize> = d
            .items
            .iter()
            .enumerate()
            .map(|(n, i)| (std::ptr::from_ref(i), n))
            .collect();
        let asked: HashMap<*const Decision, usize> = d
            .decisions
            .iter()
            .enumerate()
            .map(|(n, q)| (std::ptr::from_ref(q), n))
            .collect();
        let item = |i: &DigestItem| items.get(&std::ptr::from_ref(i)).copied();
        let slot = |r: &Row| -> Option<Slot> {
            Some(match r {
                Row::Group(h) => Slot::Group(h.clone()),
                Row::Question(q) => Slot::Question(*asked.get(&std::ptr::from_ref(*q))?),
                Row::Run(i) => Slot::Run(item(i)?),
                Row::Flow(f) => Slot::Flow {
                    key: f.key.clone(),
                    name: f.name.clone(),
                    when: f.when.clone(),
                    runs: f.runs.iter().map(|i| item(i)).collect::<Option<_>>()?,
                },
                Row::Outside(_) => return None,
            })
        };
        Kept {
            since: self.since,
            now: self.now,
            projects: self
                .projects
                .iter()
                .map(|p| KeptProject {
                    head: p.head.clone(),
                    key: p.key.clone(),
                    need: p.need,
                    ran: p.ran,
                    failed: p.failed,
                    rows: p.rows.iter().filter_map(slot).collect(),
                })
                .collect(),
        }
    }
}

impl Kept {
    /// The catch-up again, over `d`: the digest it was kept from.
    pub fn over<'a>(&self, d: &'a Digest) -> CatchUp<'a> {
        let row = |s: &Slot| -> Option<Row<'a>> {
            Some(match s {
                Slot::Group(h) => Row::Group(h.clone()),
                Slot::Question(n) => Row::Question(d.decisions.get(*n)?),
                Slot::Run(n) => Row::Run(d.items.get(*n)?),
                Slot::Flow { key, name, when, runs } => Row::Flow(rows::FlowRow {
                    key: key.clone(),
                    name: name.clone(),
                    runs: runs.iter().map(|n| d.items.get(*n)).collect::<Option<_>>()?,
                    when: when.clone(),
                    firing: d.firings.iter().find(|f| f.flow == *name),
                }),
            })
        };
        CatchUp {
            since: self.since,
            now: self.now,
            projects: self
                .projects
                .iter()
                .map(|p| Project {
                    head: p.head.clone(),
                    key: p.key.clone(),
                    need: p.need,
                    ran: p.ran,
                    failed: p.failed,
                    rows: p.rows.iter().filter_map(row).collect(),
                })
                .collect(),
        }
    }
}

impl Project<'_> {
    /// How its stretch went, in a few words: what needs you, what ran, what
    /// failed; `nothing new` when it is folded.
    pub fn tally(&self) -> String {
        if self.rows.is_empty() {
            return "nothing new".into();
        }
        let mut parts = Vec::new();
        if self.need > 0 {
            parts.push(format!(
                "{} need{} you",
                self.need,
                if self.need == 1 { "s" } else { "" }
            ));
        }
        if self.ran > 0 {
            parts.push(format!("{} ran", self.ran));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        if parts.is_empty() {
            parts.push(format!("{} new", self.rows.len()));
        }
        parts.join(" · ")
    }
}

/// The digest of the stretch from `since`, cut from the list's own (`d`,
/// which has everything): what moved since, what waits on you, what you have
/// not looked at, what is pinned, and the questions as the list has them —
/// so the catch-up asks the daemon for nothing more than the list does.
pub fn stretch(d: &Digest, since: Timestamp) -> Digest {
    let keeps = |i: &DigestItem| {
        i.at >= since
            || i.unseen
            || matches!(i.run.state, RunState::NeedsInput | RunState::Blocked)
            || i.run.pinned_at.is_some()
    };
    Digest {
        since,
        items: d.items.iter().filter(|i| keeps(i)).cloned().collect(),
        decisions: d.decisions.clone(),
        checkouts: d.checkouts.clone(),
        lost: d.lost.clone(),
        hidden: d.hidden.clone(),
        order: d.order.clone(),
        firings: d.firings.clone(),
        outside: d.outside.clone(),
    }
}

/// The catch-up's first words: since when, and how long that is.
pub fn headline(since: Timestamp, now: Timestamp) -> String {
    format!(
        "while you were away · since {} ({})",
        fmt_when(since, now),
        fmt_duration(now.duration_since(since))
    )
}

/// Whether a summary of everything can stand as the top: one that only says
/// nothing ran cannot — the counts say so, and what waits on you too.
pub fn top_worthy(s: &Summary) -> bool {
    s.source != "nothing new"
}

/// How the catch-up's top starts: the summary it shows (none: the counts),
/// and, when one is to be written, what to ask `summary.make`.
pub struct TopPlan {
    pub shown: Option<Summary>,
    pub ask: Option<SummaryParams>,
}

/// The top of a catch-up from `since`, for the panel and `rl since` alike:
/// the summary of everything kept for that stretch while it is up to date;
/// else, when it writes itself (`auto_top`) and there are runs to tell, one to
/// ask for — the counts stand until it lands; else the one kept, moved on or
/// not, or the counts. `came_back`: the stretch begins where presence says
/// you left (the panel opening on it by itself, a bare `rl since`), not
/// where you asked.
pub async fn plan_top(rpc: &mut Client, since: Timestamp, has_runs: bool, came_back: bool, now: Timestamp) -> TopPlan {
    let ask = SummaryParams {
        target: SummaryTarget::All,
        deep: false,
        from: Some(since),
        again: false,
        dry: false,
        timeout: None,
    };
    let kept = rpc
        .call::<SummaryGet>(ask.clone())
        .await
        .ok()
        .flatten()
        .filter(top_worthy);
    if kept.as_ref().is_some_and(|k| !k.stale) {
        return TopPlan { shown: kept, ask: None };
    }
    let writes = has_runs
        && came_back
        && rpc
            .call::<ConfigGet>(())
            .await
            .is_ok_and(|r| auto_top(&r, since, came_back, now));
    match writes {
        true => TopPlan {
            shown: None,
            ask: Some(ask),
        },
        false => TopPlan { shown: kept, ask: None },
    }
}

/// What a project's top is asked for over its stretch from `from`: brief, as the overview shows
/// it, and one kept that is current given back rather than written again.
pub fn project_ask(workspace: &Path, from: Timestamp) -> SummaryParams {
    SummaryParams {
        target: SummaryTarget::Project {
            workspace: workspace.to_path_buf(),
        },
        deep: false,
        from: Some(from),
        again: false,
        dry: false,
        timeout: None,
    }
}

/// How far back a project's overview summary reaches when it opens with nothing to start from,
/// and how long one kept over another stretch stands for it before it is written again.
pub const OVERVIEW_STRETCH: SignedDuration = SignedDuration::from_hours(24);

/// How long a summary kept of a project stands for its overview though it has moved on since:
/// runs going on keep moving it, and writing the top again at every open would be asking at every
/// `esc` and `enter`.
pub const OVERVIEW_FRESH: SignedDuration = SignedDuration::from_mins(15);

/// Whether a kept summary stands for the overview: while it is up to date, or, though it has
/// moved on, written less than `OVERVIEW_FRESH` ago. `within` is how old it may be at most for an
/// up-to-date one found by another stretch (`OVERVIEW_STRETCH`), or None for one of this stretch.
fn kept_current(k: &Summary, now: Timestamp, within: Option<SignedDuration>) -> bool {
    let age = now.duration_since(k.at);
    if age < OVERVIEW_FRESH {
        return true;
    }
    !k.stale && within.is_none_or(|w| age <= w)
}

/// A project's top, as its overview opens: the summary kept of it from `from` while it is up to
/// date or written less than `OVERVIEW_FRESH` ago; with none kept from there, the newest kept of
/// it from anywhere, while it is that and written within `OVERVIEW_STRETCH`. Else, when it writes itself (`auto_project_top`), one
/// to ask for, with the one kept, moved on or old, shown until it lands; else the one kept, or
/// none.
pub async fn plan_project_top(rpc: &mut Client, workspace: &Path, from: Timestamp) -> TopPlan {
    let ask = project_ask(workspace, from);
    let exact = rpc.call::<SummaryGet>(ask.clone()).await.ok().flatten();
    let (kept, current) = match exact {
        Some(k) => {
            let current = kept_current(&k, Timestamp::now(), None);
            (Some(k), current)
        }
        None => {
            let newest = SummaryParams {
                from: None,
                ..ask.clone()
            };
            let k = rpc.call::<SummaryGet>(newest).await.ok().flatten();
            let current = k
                .as_ref()
                .is_some_and(|k| kept_current(k, Timestamp::now(), Some(OVERVIEW_STRETCH)));
            (k, current)
        }
    };
    if current {
        return TopPlan { shown: kept, ask: None };
    }
    let writes = rpc.call::<ConfigGet>(()).await.is_ok_and(|r| auto_project_top(&r));
    TopPlan {
        shown: kept,
        ask: writes.then_some(ask),
    }
}

/// Whether a project's top writes itself as its overview opens: as the catch-up's does
/// (`auto_top`), with `catch_up.summary: auto` and by the assistant you named for summaries, but
/// whatever the time away — opening the overview is asking to look.
pub fn auto_project_top(r: &ConfigReport) -> bool {
    r.config.catch_up.auto_summary() && named_summariser(r)
}

/// Whether the top writes itself: only as you come back (`came_back`) after
/// longer away than `catch_up.after`, with `catch_up.summary: auto`, and by
/// the assistant you named for summaries (`assistants.summary`, as setup
/// writes it) — the first agent that happens to be here is asked only when
/// you ask, with `/summary`.
pub fn auto_top(r: &ConfigReport, since: Timestamp, came_back: bool, now: Timestamp) -> bool {
    let away = came_back
        && r.config
            .catch_up
            .after()
            .is_some_and(|after| now.duration_since(since) > after);
    away && r.config.catch_up.auto_summary() && named_summariser(r)
}

/// Whether an assistant was named for summaries (`assistants.summary`, as
/// setup writes it) and is here.
pub fn named_summariser(r: &ConfigReport) -> bool {
    r.summary.as_ref().is_some_and(|a| a.available && a.source == "config")
}

/// The top as the assistant wrote it, a line each, and last who wrote it and
/// from what (`# by haiku · from 23 runs`).
pub fn written(s: &Summary) -> Vec<String> {
    s.text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim_end().to_string())
        .chain(std::iter::once(format!("# {}", top_label(s))))
        .collect()
}

/// Who wrote the top and from what: `by haiku · from 23 runs`, `deep` first
/// when it read the logs, `moved on since` when more has happened.
pub fn top_label(s: &Summary) -> String {
    let mut parts = Vec::new();
    if s.deep {
        parts.push("deep".to_string());
    }
    parts.push(format!("by {}", s.agent));
    parts.push(format!("from {} run{}", s.runs, if s.runs == 1 { "" } else { "s" }));
    if s.stale {
        parts.push("moved on since".into());
    }
    parts.join(" · ")
}

/// The catch-up as `rl since` prints it: the headline, the `top` (a line
/// each, as given), then each project's line and under it its rows — a run
/// by its id, a question by its id and the keys that answer it.
pub fn text(cu: &CatchUp, top: &[String], now: Timestamp) -> String {
    let mut out = headline(cu.since, now);
    out.push_str("\n\n");
    for l in top {
        out.push_str(format!("  {l}").trim_end());
        out.push('\n');
    }
    for p in &cu.projects {
        let lead = if p.rows.is_empty() { '▸' } else { '▾' };
        out.push_str(&format!("\n{lead} {} · {}", p.head.name, p.tally()));
        for r in &p.rows {
            let line = row_line(r, now);
            out.push_str(&match r {
                Row::Question(q) => format!("\n    {} {}  {line}  [{}]", rows::QUESTION, q.id, q.keys()),
                Row::Run(i) => format!("\n    {} {}  {line}", rows::icon(&i.run), i.run.id),
                Row::Flow(_) => format!("\n    {} {line}", rows::FLOW),
                Row::Outside(c) => format!("\n    {} {line}", rows::outside_icon(c)),
                Row::Group(_) => continue,
            });
        }
    }
    out.push('\n');
    out
}

/// The header a question or a run goes under where the list gives it none:
/// its run's project — `everywhere`, a ref, or its folder — else the
/// question's folder.
fn head_of(d: &Digest, cx: &Context<'_>, run: Option<&Run>, ws: &Path) -> Head {
    match run {
        Some(r) if rows::everywhere_run(r, cx.names, cx.flows) => Head::everywhere(),
        Some(r) => rows::head(&r.place_key(), &r.workspace, d, cx),
        None => rows::head(&ws.to_string_lossy(), ws, d, cx),
    }
}

/// The run a row is, or a flow's last run.
fn run_of<'r>(r: &'r Row) -> Option<&'r Run> {
    match r {
        Row::Run(i) => Some(&i.run),
        Row::Flow(f) => f.last().map(|i| &i.run),
        Row::Group(_) | Row::Question(_) | Row::Outside(_) => None,
    }
}

/// Where a row stands in its project: a question, then what else needs you,
/// then the rest.
fn tier(r: &Row) -> u8 {
    match r {
        Row::Question(_) => 0,
        r if rows::wants_you(r) => 1,
        _ => 2,
    }
}

/// When a row last changed.
fn at(r: &Row) -> Option<Timestamp> {
    match r {
        Row::Group(_) => None,
        Row::Question(q) => Some(q.updated_at),
        Row::Run(i) => Some(i.at),
        Row::Flow(f) => f.last().map(|i| i.at),
        Row::Outside(c) => c.started_at,
    }
}

/// One line for a row: an agent's recap or state, a flow's marks and where
/// its latest firing stands, a shell's last line, a question's ask.
pub fn row_line(r: &Row, now: Timestamp) -> String {
    let ago = |t: Timestamp| format!("{} ago", fmt_age(t, now));
    let parts: Vec<String> = match r {
        Row::Group(h) => return h.name.clone(),
        Row::Question(q) => vec![
            format!("asks: {}", q.title),
            q.summary.clone().unwrap_or_default(),
            ago(q.updated_at),
        ],
        Row::Run(i) => vec![
            i.run.title.clone(),
            rows::run_words(&i.run),
            rows::said(i).to_string(),
            ago(i.at),
        ],
        Row::Flow(f) => {
            let mut parts = vec![format!("{} {}", f.marks(MARKS), f.name).trim_start().to_string()];
            match (f.firing, f.last()) {
                (None, Some(last)) => {
                    parts.extend([rows::state_words(last.run.state, last.run.confidence), f.summary()]);
                }
                _ => parts.push(crate::tui::flows::stands(f, None, now)),
            }
            parts.extend(f.last().map(|i| ago(i.at)));
            parts
        }
        Row::Outside(c) => {
            use crate::tui::outside;
            let mut parts = vec![c.title(), outside::state_words(c), outside::lives(c)];
            parts.extend(c.started_at.map(ago));
            parts
        }
    };
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::rows::Names;
    use rl_proto::decision::{Decision, DecisionOption, DecisionStatus};
    use rl_proto::flow::{Firing, FiringStatus, StepState, StepStatus};
    use rl_proto::methods::DigestItem;
    use rl_proto::types::{Cause, Confidence, Run, RunKind, RunState, Summary};
    use std::path::{Path, PathBuf};

    fn item(id: &str, state: RunState, ws: &str, at: &str, unseen: bool) -> DigestItem {
        let t: Timestamp = at.parse().unwrap();
        let mut r = Run::new(id, RunKind::Agent, format!("title {id}"), vec![], ws, ws, t);
        r.state = state;
        r.confidence = Confidence::Exact;
        r.state_at = t;
        if state.is_terminal() {
            r.ended_at = Some(t);
        }
        DigestItem {
            run: r,
            summary: format!("summary {id}"),
            at: t,
            unseen,
            joined_by: None,
            emitted: vec![],
            cwd: None,
            recap: None,
        }
    }

    fn fired(id: &str, flow: &str, state: RunState, at: &str) -> DigestItem {
        let mut i = item(id, state, API, at, true);
        i.run.cause = Cause::Flow {
            flow: flow.into(),
            step: "probe".into(),
        };
        i.summary = if state == RunState::Failed {
            "exit 1 · 0s".into()
        } else {
            "exit 0 · 0s".into()
        };
        i
    }

    fn question(id: &str, ws: &str, at: &str) -> Decision {
        let t: Timestamp = at.parse().unwrap();
        Decision {
            id: id.into(),
            workspace: ws.into(),
            run: None,
            chain: id.into(),
            group: None,
            title: format!("question {id}"),
            summary: None,
            options: vec![DecisionOption {
                key: "ok".into(),
                ..Default::default()
            }],
            multi: false,
            status: DecisionStatus::Open,
            created_at: t,
            updated_at: t,
            expires_at: None,
            resolved_at: None,
            chosen: vec![],
            outcome_runs: vec![],
            note: None,
        }
    }

    const API: &str = "/home/f/api";
    const RELAY: &str = "/home/f/relay";

    #[test]
    fn a_flow_line_says_its_latest_firing_and_what_it_came_to() {
        let (mut d, names) = (fixture(), names());
        d.firings.push(Firing {
            id: "fi1".into(),
            flow: "nightly".into(),
            trigger: "when".into(),
            event: None,
            status: FiringStatus::Failed,
            started_at: now(),
            ended_at: Some(now()),
            steps: vec![StepState {
                name: "probe".into(),
                status: StepStatus::Failed,
                run: None,
                decision: None,
            }],
            outcome: Some("disk full".into()),
        });
        let cu = build(&d, &names);
        let flow = cu.projects[0].rows.iter().find(|r| matches!(r, Row::Flow(_))).unwrap();
        let line = row_line(flow, now());
        assert!(line.contains("nightly · failed at probe · disk full · "), "{line}");
    }

    #[test]
    fn a_flow_heading_the_catch_up_is_not_a_line_under_its_project_too() {
        let (mut d, names) = (fixture(), names());
        d.firings.push(Firing {
            id: "fi1".into(),
            flow: "nightly".into(),
            trigger: "when".into(),
            event: None,
            status: FiringStatus::Failed,
            started_at: "2026-10-06T04:00:00Z".parse().unwrap(),
            ended_at: Some("2026-10-06T04:00:00Z".parse().unwrap()),
            steps: vec![],
            outcome: Some("disk full".into()),
        });
        let mut flows = flows();
        flows[0].def["catch_up"] = "top".into();
        let cu = CatchUp::build(
            &d,
            &Context {
                names: &names,
                flows: &flows,
                filter: "",
                show_flows: true,
                show_agents: true,
                show_shells: true,
                show_hidden: false,
                now: now(),
                relay: None,
                parked: &[],
            },
        );
        let api = cu.projects.iter().find(|p| p.head.name == "api").unwrap();
        assert!(
            !api.rows.iter().any(|r| matches!(r, Row::Flow(_))),
            "moved to the head, not copied: {:?}",
            keys(api)
        );
        assert_eq!(keys(api), ["q:q1", "r:a1"]);
        assert_eq!((api.ran, api.failed), (1, 1), "its runs are the head's to tell");

        // Its latest firing from before you left heads nothing: it stays a line.
        d.firings[0].started_at = "2026-10-05T04:00:00Z".parse().unwrap();
        d.firings[0].ended_at = d.firings[0].started_at.into();
        let cu = CatchUp::build(
            &d,
            &Context {
                names: &names,
                flows: &flows,
                filter: "",
                show_flows: true,
                show_agents: true,
                show_shells: true,
                show_hidden: false,
                now: now(),
                relay: None,
                parked: &[],
            },
        );
        assert!(cu.projects[0].rows.iter().any(|r| matches!(r, Row::Flow(_))));
    }

    fn now() -> Timestamp {
        "2026-10-06T12:00:00Z".parse().unwrap()
    }

    /// `api`: a question, a failed run, a nightly flow's runs that went ok, ok,
    /// failed, and a run you saw before you left; `relay`: only a run you saw
    /// before you left.
    fn fixture() -> Digest {
        Digest {
            since: "2026-10-06T00:00:00Z".parse().unwrap(),
            items: vec![
                item("old", RunState::Finished, API, "2026-10-05T20:00:00Z", false),
                item("a1", RunState::Failed, API, "2026-10-06T09:00:00Z", true),
                fired("n1", "nightly", RunState::Finished, "2026-10-06T02:00:00Z"),
                fired("n2", "nightly", RunState::Finished, "2026-10-06T03:00:00Z"),
                fired("n3", "nightly", RunState::Failed, "2026-10-06T04:00:00Z"),
                item("r1", RunState::Finished, RELAY, "2026-10-05T21:00:00Z", false),
            ],
            decisions: vec![question("q1", API, "2026-10-06T08:00:00Z")],
            checkouts: vec![],
            lost: vec![],
            hidden: vec![],
            order: vec![],
            firings: Vec::new(),
            outside: vec![],
        }
    }

    fn names() -> Names {
        Names::new([
            (PathBuf::from(API), "api".to_string()),
            (PathBuf::from(RELAY), "relay".to_string()),
        ])
    }

    /// The flows the fixture's runs are steps of, both `in` api.
    fn flows() -> Vec<Flow> {
        ["nightly", "hourly"]
            .map(|name| Flow {
                name: name.into(),
                hash: "h".into(),
                def: serde_json::json!({ "in_": API, "trigger": { "when": { "Every": 3600 }, "on": [] } }),
                source: format!("/h/flows/{name}").into(),
                folder: Some(API.into()),
                paused: false,
                approved_hash: Some("h".into()),
                last_fired_at: None,
                next_at: None,
                removed_at: None,
            })
            .to_vec()
    }

    fn build<'a>(d: &'a Digest, names: &Names) -> CatchUp<'a> {
        CatchUp::build(
            d,
            &Context {
                names,
                flows: &flows(),
                filter: "",
                show_flows: true,
                show_agents: true,
                show_shells: true,
                show_hidden: false,
                now: now(),
                relay: None,
                parked: &[],
            },
        )
    }

    fn keys(p: &Project) -> Vec<String> {
        p.rows.iter().filter_map(Row::key).collect()
    }

    #[test]
    fn what_needs_you_comes_first_and_a_quiet_project_folds() {
        let (d, names) = (fixture(), names());
        let cu = build(&d, &names);
        assert_eq!(cu.since, d.since);
        assert_eq!(cu.projects[0].head.name, "api");
        assert!(matches!(cu.projects[0].rows[0], Row::Question(_)));
        assert_eq!(
            keys(&cu.projects[0]),
            ["q:q1", "r:a1", "f:nightly"],
            "the question, then the run and the flow that failed, newest first; what you saw before is not news"
        );
        assert_eq!(cu.projects[0].need, 3, "the question, the failed run, the failed flow");
        assert_eq!(cu.projects[0].ran, 4, "the run and the flow's three");
        assert_eq!(cu.projects[0].failed, 2, "the run and one of the flow's");
        let relay = cu.projects.iter().find(|p| p.head.name == "relay").unwrap();
        assert!(relay.rows.is_empty(), "nothing since: folded");
        assert_eq!((relay.need, relay.ran, relay.failed), (0, 0, 0));
    }

    #[test]
    fn projects_that_need_you_come_first() {
        let mut d = fixture();
        // A failed run in relay, newer than anything in api: one need is fewer than three.
        d.items
            .push(item("r2", RunState::Failed, RELAY, "2026-10-06T11:00:00Z", true));
        let names = names();
        let cu = build(&d, &names);
        let order: Vec<&str> = cu.projects.iter().map(|p| p.head.name.as_str()).collect();
        assert_eq!(order, ["api", "relay"]);
        assert_eq!(cu.projects[1].need, 1);
    }

    #[test]
    fn what_the_list_puts_above_every_header_goes_under_its_project() {
        let mut d = fixture();
        // A pinned run stands above the projects on the list; a question from
        // a folder with no header has none to go under.
        let mut pinned = item("p1", RunState::Finished, RELAY, "2026-10-06T10:00:00Z", true);
        pinned.run.pinned_at = Some("2026-10-01T00:00:00Z".parse().unwrap());
        d.items.push(pinned);
        d.decisions
            .push(question("q2", "/home/f/scratch", "2026-10-06T07:00:00Z"));
        let names = names();
        let cu = build(&d, &names);
        let relay = cu.projects.iter().find(|p| p.head.name == "relay").unwrap();
        assert_eq!(keys(relay), ["r:p1"]);
        assert_eq!(relay.ran, 1);
        let scratch = cu.projects.iter().find(|p| p.head.name == "scratch").unwrap();
        assert_eq!(keys(scratch), ["q:q2"]);
        assert_eq!(scratch.need, 1);
        assert_eq!(scratch.head.workspace, PathBuf::from("/home/f/scratch"));
    }

    #[test]
    fn what_is_unseen_from_before_the_stretch_shows_but_is_not_counted() {
        let mut d = fixture();
        // Before `since`, more than a day before now, and never looked at.
        let mut old = fired("n0", "nightly", RunState::Failed, "2026-10-05T10:00:00Z");
        old.unseen = true;
        d.items.push(old);
        d.items
            .push(item("a0", RunState::Failed, API, "2026-10-05T09:00:00Z", true));
        let names = names();
        let cu = build(&d, &names);
        let api = &cu.projects[0];
        assert!(keys(api).contains(&"r:a0".to_string()), "still shown: {:?}", keys(api));
        assert_eq!((api.ran, api.failed), (4, 2), "only the stretch is counted");
        assert_eq!(api.need, 4, "but it still needs you");
        let flow = api.rows.iter().find(|r| matches!(r, Row::Flow(_))).unwrap();
        assert!(
            row_line(flow, now()).starts_with("··× nightly"),
            "{}",
            row_line(flow, now())
        );
    }

    #[test]
    fn a_long_stretch_keeps_what_ended_more_than_a_day_ago() {
        let mut d = fixture();
        d.since = "2026-10-03T12:00:00Z".parse().unwrap();
        // A stopped run is never news; a run you looked at elsewhere is
        // not news either. Both are in the stretch, and more than a day old.
        let mut stopped = fired("h1", "hourly", RunState::Stopped, "2026-10-04T10:00:00Z");
        stopped.unseen = false;
        d.items.push(stopped);
        d.items
            .push(item("seen", RunState::Finished, API, "2026-10-04T12:00:00Z", false));
        let names = names();
        let cu = build(&d, &names);
        let api = &cu.projects[0];
        let k = keys(api);
        assert!(k.contains(&"r:seen".to_string()), "{k:?}");
        assert!(k.contains(&"f:hourly".to_string()), "{k:?}");
        let relay = cu.projects.iter().find(|p| p.head.name == "relay").unwrap();
        assert_eq!(
            keys(relay),
            ["r:r1"],
            "the run you saw before you left is now in the stretch"
        );
    }

    #[test]
    fn the_stretch_is_cut_from_the_lists_own_digest() {
        // The list's digest, from the start of time: everything the daemon
        // has, and the questions.
        let mut list = fixture();
        list.since = Timestamp::UNIX_EPOCH;
        let mut pinned = item("p1", RunState::Finished, RELAY, "2026-09-01T00:00:00Z", false);
        pinned.run.pinned_at = Some("2026-09-01T00:00:00Z".parse().unwrap());
        list.items.extend([
            item("w1", RunState::NeedsInput, RELAY, "2026-09-02T00:00:00Z", false),
            item("u1", RunState::Finished, RELAY, "2026-09-03T00:00:00Z", true),
            pinned,
        ]);
        let since: Timestamp = "2026-10-06T00:00:00Z".parse().unwrap();
        let d = stretch(&list, since);
        assert_eq!(d.since, since);
        let ids: Vec<&str> = d.items.iter().map(|i| i.run.id.as_str()).collect();
        assert_eq!(
            ids,
            ["a1", "n1", "n2", "n3", "w1", "u1", "p1"],
            "what moved since, what waits on you, what you have not looked at, what is pinned"
        );
        assert_eq!(
            d.decisions.len(),
            list.decisions.len(),
            "the questions as the list has them"
        );
        // Built on it, the catch-up is the one the stretch's own would be.
        let names = names();
        let cu = build(&d, &names);
        let relay = cu.projects.iter().find(|p| p.head.name == "relay").unwrap();
        assert_eq!(keys(relay), ["r:w1", "r:u1"], "a pin alone is not news");
        assert_eq!(keys(&cu.projects[0]), keys(&build(&fixture(), &names).projects[0]));
    }

    #[test]
    fn a_catch_up_kept_by_place_comes_back_as_it_was() {
        let (d, names) = (fixture(), names());
        let cu = build(&d, &names);
        let again = cu.keep(&d).over(&d);
        let said = |cu: &CatchUp| -> Vec<(String, String, Vec<String>)> {
            cu.projects
                .iter()
                .map(|p| {
                    (
                        p.head.name.clone(),
                        p.tally(),
                        p.rows.iter().map(|r| row_line(r, now())).collect(),
                    )
                })
                .collect()
        };
        assert_eq!(said(&again), said(&cu));
        assert_eq!((again.since, again.counted()), (cu.since, cu.counted()));
    }

    #[test]
    fn a_run_waiting_on_you_is_kept_however_old() {
        let mut d = fixture();
        d.items
            .push(item("w1", RunState::NeedsInput, RELAY, "2026-10-05T20:00:00Z", false));
        d.items
            .push(item("r3", RunState::Finished, RELAY, "2026-10-06T10:00:00Z", true));
        let names = names();
        let cu = build(&d, &names);
        let relay = cu.projects.iter().find(|p| p.head.name == "relay").unwrap();
        assert_eq!(keys(relay), ["r:w1", "r:r3"]);
        assert_eq!((relay.need, relay.ran), (1, 1));
    }

    #[test]
    fn only_a_question_is_nothing_for_an_assistant_to_tell() {
        let mut d = fixture();
        d.items.clear();
        let names = names();
        let cu = build(&d, &names);
        assert!(!cu.has_runs(), "a question alone");
        assert_eq!(cu.counted(), ["• 1 needs you: api"]);
        assert!(build(&fixture(), &names).has_runs());
    }

    #[test]
    fn an_assistants_own_run_is_not_news() {
        let mut d = fixture();
        let mut top = item("s1", RunState::Working, RELAY, "2026-10-06T11:00:00Z", true);
        top.run.cause = Cause::Assistant("summary".into());
        d.items.push(top);
        let names = names();
        let cu = build(&d, &names);
        let relay = cu.projects.iter().find(|p| p.head.name == "relay").unwrap();
        assert!(relay.rows.is_empty(), "writing the top is not what happened");
        assert!(!cu.bullets().iter().any(|b| b.starts_with("relay")));
    }

    #[test]
    fn flows_that_did_nothing_in_the_stretch_raise_no_header() {
        // Two flows with no `in`: one that never ran, one whose only run
        // failed three days ago and was seen. The list has both, under
        // `everywhere`; nothing happened there, so the catch-up has no line.
        let mut old = fired("o1", "pulse", RunState::Failed, "2026-10-03T12:00:00Z");
        old.unseen = false;
        let d = Digest {
            since: "2026-10-06T00:00:00Z".parse().unwrap(),
            items: vec![old],
            decisions: vec![],
            checkouts: vec![],
            lost: vec![],
            hidden: vec![],
            order: vec![],
            firings: Vec::new(),
            outside: vec![],
        };
        let flows: Vec<Flow> = flows()
            .into_iter()
            .zip(["pulse", "quiet"])
            .map(|(f, name)| Flow {
                name: name.into(),
                folder: None,
                ..f
            })
            .collect();
        let cx = Context {
            names: &names(),
            flows: &flows,
            filter: "",
            show_flows: true,
            show_agents: true,
            show_shells: true,
            show_hidden: false,
            now: now(),
            relay: None,
            parked: &[],
        };
        let on_the_list = rows::build(&d, &cx);
        assert!(on_the_list.iter().any(|r| r.key().as_deref() == Some("f:quiet")));
        assert!(on_the_list.iter().any(|r| r.key().as_deref() == Some("f:pulse")));
        let cu = CatchUp::build(&d, &cx);
        let heads: Vec<&str> = cu.projects.iter().map(|p| p.head.name.as_str()).collect();
        assert_eq!(heads, ["api", "relay"], "registered folders only");
    }

    #[test]
    fn a_question_goes_under_its_own_folder_not_one_of_the_same_name() {
        let (one, two) = ("/home/f/app", "/home/g/app");
        let d = Digest {
            since: "2026-10-06T00:00:00Z".parse().unwrap(),
            items: vec![],
            decisions: vec![question("q1", two, "2026-10-06T08:00:00Z")],
            checkouts: vec![],
            lost: vec![],
            hidden: vec![],
            order: vec![],
            firings: Vec::new(),
            outside: vec![],
        };
        let names = Names::new([
            (PathBuf::from(one), "app".to_string()),
            (PathBuf::from(two), "app".to_string()),
        ]);
        let cu = build(&d, &names);
        let under = |ws: &str| {
            let p = cu.projects.iter().find(|p| p.head.workspace == Path::new(ws)).unwrap();
            keys(p)
        };
        assert_eq!(under(two), ["q:q1"]);
        assert!(under(one).is_empty());
        assert_eq!(cu.projects.len(), 2, "no third header for it");
    }

    #[test]
    fn bullets_without_an_assistant_say_counts() {
        let (d, names) = (fixture(), names());
        let b = build(&d, &names).bullets();
        assert_eq!(b[0], "3 need you: api");
        assert!(b.iter().any(|l| l == "api: 4 ran · 2 failed"), "{b:?}");
        assert!(
            !b.iter().any(|l| l.starts_with("relay")),
            "nothing to say is not said: {b:?}"
        );
    }

    #[test]
    fn a_flow_line_is_its_marks_and_last_outcome() {
        let (d, names) = (fixture(), names());
        let cu = build(&d, &names);
        let flow = cu.projects[0].rows.iter().find(|r| matches!(r, Row::Flow(_))).unwrap();
        let line = row_line(flow, now());
        assert!(line.starts_with("··×"), "{line}");
        assert!(line.contains("nightly") && line.contains("failed"), "{line}");
    }

    #[test]
    fn lines_are_what_the_assistant_is_handed_per_project() {
        let (d, names) = (fixture(), names());
        let lines = build(&d, &names).lines();
        assert_eq!(lines.len(), 1, "a project with nothing new hands nothing: {lines:?}");
        let (name, text) = &lines[0];
        assert_eq!(name, "api");
        assert_eq!(text.lines().count(), 3, "{text}");
        assert!(text.lines().next().unwrap().contains("question q1"), "{text}");
    }

    #[test]
    fn a_runs_line_says_its_recap_when_it_waits() {
        let mut i = item("a2", RunState::NeedsInput, API, "2026-10-06T10:00:00Z", true);
        i.recap = Some("Search fixed; nothing committed.".into());
        let line = row_line(&Row::Run(&i), now());
        assert!(line.contains("needs you"), "{line}");
        assert!(line.contains("Search fixed; nothing committed."), "{line}");
        assert!(line.ends_with("2h ago"), "{line}");
    }

    #[test]
    fn the_text_is_the_headline_the_top_then_each_project() {
        let (d, names) = (fixture(), names());
        let cu = build(&d, &names);
        let t = text(&cu, &["• it went fine".to_string()], now());
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].starts_with("while you were away · since "), "{t}");
        assert!(lines[0].ends_with("(12h00m)"), "{t}");
        assert_eq!(lines[2], "  • it went fine", "{t}");
        let t = text(
            &cu,
            &["↻ brief · done".into(), "  line".into(), String::new(), "• fine".into()],
            now(),
        );
        assert!(
            t.contains("\n  ↻ brief · done\n    line\n\n  • fine\n"),
            "a blank line has no spaces: {t}"
        );
        assert!(t.contains("\n▾ api · 3 need you · 4 ran · 2 failed\n"), "{t}");
        assert!(t.contains("\n    ? q1  asks: question q1 · 4h ago  [ok]\n"), "{t}");
        assert!(t.contains("    ✦ a1  title a1 · failed"), "a run by its id: {t}");
        assert!(t.contains("    ↻ ··× nightly · failed"), "{t}");
        assert!(t.ends_with("\n▸ relay · nothing new\n"), "{t}");
    }

    #[test]
    fn the_top_says_who_wrote_it_and_from_how_many_runs() {
        let mut s = Summary {
            run: String::new(),
            agent: "haiku".into(),
            by: "x1".into(),
            at: now(),
            text: "• fine".into(),
            since: None,
            source: "summarised".into(),
            reach: String::new(),
            stale: false,
            target: "all".into(),
            deep: false,
            from: Some(now()),
            calls: 1,
            runs: 23,
        };
        assert_eq!(top_label(&s), "by haiku · from 23 runs");
        s.runs = 1;
        s.deep = true;
        s.stale = true;
        assert_eq!(top_label(&s), "deep · by haiku · from 1 run · moved on since");
    }

    /// A summary kept `ago` back, moved on since or not.
    fn kept(ago: SignedDuration, stale: bool) -> Summary {
        Summary {
            run: String::new(),
            agent: "haiku".into(),
            by: "x1".into(),
            at: Timestamp::now() - ago,
            text: "• fine".into(),
            since: None,
            source: "summarised".into(),
            reach: String::new(),
            stale,
            target: "project:/p".into(),
            deep: false,
            from: None,
            calls: 1,
            runs: 2,
        }
    }

    #[test]
    fn a_summary_moved_on_stands_for_the_overview_for_fifteen_minutes() {
        let now = Timestamp::now();
        let young = kept(SignedDuration::from_mins(14), true);
        assert!(kept_current(&young, now, None), "a minute short of the floor");
        assert!(kept_current(&young, now, Some(OVERVIEW_STRETCH)));
        let old = kept(SignedDuration::from_mins(16), true);
        assert!(!kept_current(&old, now, None), "past it, written again");
        assert!(!kept_current(&old, now, Some(OVERVIEW_STRETCH)));
    }

    #[test]
    fn a_summary_up_to_date_stands_as_before() {
        let now = Timestamp::now();
        let day = SignedDuration::from_hours(23);
        assert!(kept_current(&kept(day, false), now, None));
        assert!(kept_current(&kept(day, false), now, Some(OVERVIEW_STRETCH)));
        let older = kept(SignedDuration::from_hours(25), false);
        assert!(kept_current(&older, now, None), "of its own stretch, any age");
        assert!(!kept_current(&older, now, Some(OVERVIEW_STRETCH)));
    }

    /// What `config.get` says: the summariser named (`config`) or not, and `catch_up`'s
    /// `summary` and `after`.
    fn report(source: &str, summary: Option<&str>, after: Option<&str>) -> ConfigReport {
        use rl_proto::methods::ResolvedAssistant;
        let mut config = rl_proto::config::Config::default();
        config.catch_up.summary = summary.map(String::from);
        config.catch_up.after = after.map(String::from);
        ConfigReport {
            file: PathBuf::from("/h/config.yaml"),
            present: true,
            config,
            interpret: None,
            summary: Some(ResolvedAssistant {
                agent: "haiku".into(),
                source: source.into(),
                argv: vec![],
                available: true,
            }),
            catch_up_after: String::new(),
            catch_up_summary: String::new(),
            errors: vec![],
        }
    }

    #[test]
    fn the_top_writes_itself_only_as_you_come_back_and_by_the_assistant_you_named() {
        let now = now();
        let two_days = now - jiff::SignedDuration::from_hours(48);
        let hour = now - jiff::SignedDuration::from_hours(1);
        let named = report("config", None, None);
        assert!(auto_top(&named, two_days, true, now), "away, auto, named");
        assert!(
            !auto_top(&named, two_days, false, now),
            "a stretch asked for is not coming back"
        );
        assert!(!auto_top(&named, hour, true, now), "not away longer than `after`");
        assert!(
            !auto_top(&report("default", None, None), two_days, true, now),
            "the first agent here is never asked by itself"
        );
        assert!(!auto_top(&report("config", Some("ask"), None), two_days, true, now));
        assert!(!auto_top(&report("config", None, Some("off")), two_days, true, now));
        let mut gone = report("config", None, None);
        gone.summary.as_mut().unwrap().available = false;
        assert!(!auto_top(&gone, two_days, true, now), "not on this machine");
    }

    #[test]
    fn a_projects_top_writes_itself_whatever_the_time_away_by_the_assistant_you_named() {
        assert!(auto_project_top(&report("config", None, None)), "auto, named");
        assert!(
            auto_project_top(&report("config", None, Some("off"))),
            "time away does not come into it"
        );
        assert!(
            !auto_project_top(&report("default", None, None)),
            "the first agent here is never asked by itself"
        );
        assert!(!auto_project_top(&report("config", Some("ask"), None)));
        let mut gone = report("config", None, None);
        gone.summary.as_mut().unwrap().available = false;
        assert!(!auto_project_top(&gone), "not on this machine");
    }

    #[test]
    fn a_projects_top_is_asked_of_the_project_brief_from_where_given() {
        let from = now() - jiff::SignedDuration::from_hours(24);
        let ask = project_ask(Path::new("/w/api"), from);
        assert_eq!(
            ask.target,
            SummaryTarget::Project {
                workspace: PathBuf::from("/w/api")
            }
        );
        assert_eq!(
            (ask.deep, ask.from, ask.again, ask.dry),
            (false, Some(from), false, false)
        );
    }
}
