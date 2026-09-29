//! The events view (`/events`): what the ledger saw, newest first, one line
//! each — how long ago, what kind, where, which run, what it said — and under
//! an event, indented, what it set off.
//!
//! What it set off is read from what the ledger keeps, never from words:
//!
//! - a schedule's firing is about the run it started (`schedule.fired`);
//! - an answer names the runs its options started (`decision.taken`'s and
//!   `decision.expired`'s `runs`);
//! - a reaction's run carries its rule (`cause: reaction:<name>`), and its
//!   `run.started` names the event it fired on (`data.event`);
//! - a reaction's question names its rule and the event it fired on
//!   (`decision.raised`'s `reaction` and `event`).
//!
//! Runs and questions from before reactions kept their event have no
//! `event`, so theirs is read back from what was kept then: a reaction's run
//! joined the chain the event's run was in (`flow`; its own id when the event
//! had no run), and the event is the newest one before it in that chain of a
//! kind the rule listens for, within the five minutes the reactor still acts
//! on; a question was grouped `reaction:<name>` and named the event's run,
//! and the event is the newest one of that run the rule listens for, before
//! it. A rule that named its own `group:` then asked questions that cannot be
//! told from a run's own, so those stand on their own line only.
//!
//! Pure: what was fetched in, rows and lines out. Fetching is the panel's.

use super::query::{Named, Query, Subject};
use super::rows::{self, Names, fit, place_parts};
use super::rules;
use super::theme;
use crate::render::fmt_clock;
use jiff::{SignedDuration, Timestamp};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use rl_core::time::fmt_age;
use rl_proto::decision::Decision;
use rl_proto::rule::{Rule, RuleKind};
use rl_proto::types::{Cause, Event, Run, event_kind};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// How many events one fetch takes: the newest page, and each older one.
pub const PAGE: u32 = 500;

/// How late the reactor still acts on an event (its `MAX_AGE`): what a
/// reaction started later than this after an event was not that event's doing.
const LATE: SignedDuration = SignedDuration::from_mins(5);

/// Where the kind column starts: a space, the age, two spaces.
const KIND_COL: usize = 7;

/// What the view holds: the events read so far, newest first, and what is
/// known of the runs and questions they name.
#[derive(Default)]
pub struct Feed {
    pub events: Vec<Event>,
    /// Whether the first page has come.
    pub read: bool,
    /// There may be older events than the oldest read.
    pub more: bool,
    pub runs: HashMap<String, Run>,
    pub questions: HashMap<String, Decision>,
    /// Asked for and not there: not asked for again.
    pub gone: HashSet<String>,
}

impl Feed {
    pub fn newest(&self) -> Option<i64> {
        self.events.first().map(|e| e.id)
    }

    pub fn oldest(&self) -> Option<i64> {
        self.events.last().map(|e| e.id)
    }

    /// What came since the newest one read. The first page, or a full one —
    /// which may have left a gap behind it — is all there is from then on.
    pub fn newer(&mut self, mut page: Vec<Event>, limit: usize) {
        page.sort_by_key(|e| std::cmp::Reverse(e.id));
        if !self.read || page.len() >= limit {
            self.more = page.len() >= limit;
            self.events = page;
        } else {
            let newest = self.newest();
            page.retain(|e| newest.is_none_or(|n| e.id > n));
            page.append(&mut self.events);
            self.events = page;
        }
        self.read = true;
    }

    /// The page before the oldest one read.
    pub fn older(&mut self, mut page: Vec<Event>, limit: usize) {
        self.more = page.len() >= limit;
        page.sort_by_key(|e| std::cmp::Reverse(e.id));
        let oldest = self.oldest();
        page.retain(|e| oldest.is_none_or(|o| e.id < o));
        self.events.append(&mut page);
    }

    /// The runs the events name that nothing is known of yet.
    pub fn unknown_runs(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .events
            .iter()
            .flat_map(runs_named)
            .filter(|id| !self.runs.contains_key(id) && !self.gone.contains(id))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The questions the events raise that nothing is known of yet.
    pub fn unknown_questions(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .events
            .iter()
            .filter(|e| e.kind == event_kind::DECISION_RAISED)
            .filter_map(|e| data_str(e, "decision"))
            .filter(|id| !self.questions.contains_key(*id) && !self.gone.contains(*id))
            .map(str::to_string)
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every kind there is to search for: the daemon's own, and whatever
    /// else was read.
    pub fn kinds(&self) -> Vec<String> {
        let mut out: Vec<String> = event_kind::ALL.iter().map(|k| k.to_string()).collect();
        out.extend(self.events.iter().map(|e| e.kind.clone()));
        out.sort();
        out.dedup();
        out
    }
}

/// One text field of an event's data.
fn data_str<'a>(e: &'a Event, field: &str) -> Option<&'a str> {
    e.data.as_ref()?.get(field)?.as_str()
}

/// One whole-number field of an event's data.
fn data_i64(e: &Event, field: &str) -> Option<i64> {
    e.data.as_ref()?.get(field)?.as_i64()
}

/// The runs an event names: the one it is about, and the ones an answer
/// started.
fn runs_named(e: &Event) -> Vec<String> {
    let mut out: Vec<String> = e.run.iter().cloned().collect();
    if let Some(serde_json::Value::Array(ids)) = e.data.as_ref().and_then(|d| d.get("runs")) {
        out.extend(ids.iter().filter_map(|v| v.as_str()).map(str::to_string));
    }
    out
}

/// Something an event set off: a line under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetOff {
    /// A reaction heard it and started a run.
    Reaction { rule: String, run: String },
    /// A reaction heard it and asked.
    Question { rule: String, question: String },
    /// A schedule fired and started a run.
    Schedule { rule: String, run: String },
    /// An option taken started a run.
    Answer { run: String },
}

impl SetOff {
    /// The rule behind it, when a rule is.
    pub fn rule(&self) -> Option<(RuleKind, &str)> {
        match self {
            SetOff::Reaction { rule, .. } | SetOff::Question { rule, .. } => Some((RuleKind::Reaction, rule)),
            SetOff::Schedule { rule, .. } => Some((RuleKind::Schedule, rule)),
            SetOff::Answer { .. } => None,
        }
    }

    pub fn run(&self) -> Option<&str> {
        match self {
            SetOff::Reaction { run, .. } | SetOff::Schedule { run, .. } | SetOff::Answer { run } => Some(run),
            SetOff::Question { .. } => None,
        }
    }

    /// The line in words, after its `↳`.
    pub fn words(&self, feed: &Feed) -> String {
        let title = |id: &str| feed.runs.get(id).map_or_else(|| id.to_string(), |r| r.title.clone());
        match self {
            SetOff::Reaction { rule, run } => format!("⚡ {rule} → started '{}'", title(run)),
            SetOff::Question { rule, question } => {
                let t = feed
                    .questions
                    .get(question)
                    .map_or(question.as_str(), |q| q.title.as_str());
                format!("? {rule} asked '{t}'")
            }
            SetOff::Schedule { rule, run } => format!("↻ {rule} → started '{}'", title(run)),
            SetOff::Answer { run } => format!("▶ the answer started '{}'", title(run)),
        }
    }
}

/// What each event set off, by the event's id; see the module's head for
/// how each is read.
pub fn links(feed: &Feed, rules: &[Rule]) -> HashMap<i64, Vec<SetOff>> {
    let evs = &feed.events;
    let run_of = |e: &Event| e.run.as_deref().and_then(|id| feed.runs.get(id));
    let flow_of = |e: &Event| run_of(e).map(|r| r.flow.as_str());
    // Whether a rule of that name listens for the kind; `None` when no rule
    // of that name is known any more.
    let listens = |name: &str, kind: &str| -> Option<bool> {
        let mut known = false;
        for r in rules {
            if let Rule::Reaction(x) = r
                && x.name == name
            {
                known = true;
                if x.spec.matches_kind(kind) {
                    return Some(true);
                }
            }
        }
        known.then_some(false)
    };
    // A rule never hears its own doing: a run it started, a question it asked.
    let own_doing = |e: &Event, name: &str| {
        run_of(e).is_some_and(|r| r.cause == Cause::Reaction(name.to_string()))
            || (e.kind.starts_with("decision.")
                && data_str(e, "decision")
                    .and_then(|id| feed.questions.get(id))
                    .and_then(|q| q.group.as_deref())
                    .is_some_and(|g| g.strip_prefix("reaction:") == Some(name)))
    };
    // The newest event before `i` that `fits`, no later than the reactor acts.
    let before = |i: usize, fits: &dyn Fn(&Event) -> bool| -> Option<i64> {
        let at = evs[i].at;
        evs[i + 1..]
            .iter()
            .take_while(|t| at.duration_since(t.at) <= LATE)
            .find(|t| fits(t))
            .map(|t| t.id)
    };
    // Each with the id of the event that says it, so they read in the order
    // they happened: they are found newest first.
    let mut out: HashMap<i64, Vec<(i64, SetOff)>> = HashMap::new();
    for (i, e) in evs.iter().enumerate() {
        match e.kind.as_str() {
            event_kind::SCHEDULE_FIRED => {
                let run = e.run.as_deref().or_else(|| data_str(e, "run"));
                let rule = data_str(e, "schedule").or_else(|| match run_of(e).map(|r| &r.cause) {
                    Some(Cause::Schedule(n)) => Some(n.as_str()),
                    _ => None,
                });
                if let (Some(run), Some(rule)) = (run, rule) {
                    let s = SetOff::Schedule {
                        rule: rule.into(),
                        run: run.into(),
                    };
                    out.entry(e.id).or_default().push((e.id, s));
                }
            }
            event_kind::DECISION_TAKEN | event_kind::DECISION_EXPIRED => {
                for run in runs_named(e).into_iter().filter(|r| Some(r) != e.run.as_ref()) {
                    out.entry(e.id).or_default().push((e.id, SetOff::Answer { run }));
                }
            }
            event_kind::RUN_STARTED => {
                let Some(r) = run_of(e) else { continue };
                let Cause::Reaction(name) = &r.cause else { continue };
                // The event's chain; none when it had no run of its own.
                let chain = (r.flow != r.id).then_some(r.flow.as_str());
                let fits = |t: &Event| {
                    flow_of(t) == chain && !own_doing(t, name) && listens(name, &t.kind).unwrap_or(chain.is_some())
                };
                if let Some(id) = data_i64(e, "event").or_else(|| before(i, &fits)) {
                    let s = SetOff::Reaction {
                        rule: name.clone(),
                        run: r.id.clone(),
                    };
                    out.entry(id).or_default().push((e.id, s));
                }
            }
            event_kind::DECISION_RAISED => {
                let Some(qid) = data_str(e, "decision") else { continue };
                let Some(name) = data_str(e, "reaction").or_else(|| {
                    feed.questions
                        .get(qid)
                        .and_then(|q| q.group.as_deref())
                        .and_then(|g| g.strip_prefix("reaction:"))
                }) else {
                    continue;
                };
                let fits = |t: &Event| {
                    t.run == e.run && !own_doing(t, name) && listens(name, &t.kind).unwrap_or(e.run.is_some())
                };
                if let Some(id) = data_i64(e, "event").or_else(|| before(i, &fits)) {
                    let s = SetOff::Question {
                        rule: name.to_string(),
                        question: qid.to_string(),
                    };
                    out.entry(id).or_default().push((e.id, s));
                }
            }
            _ => {}
        }
    }
    out.into_iter()
        .map(|(id, mut v)| {
            v.sort_by_key(|(by, _)| *by);
            (id, v.into_iter().map(|(_, s)| s).collect())
        })
        .collect()
}

/// One event as the view shows it.
pub struct Item<'a> {
    pub event: &'a Event,
    /// The run it is about, when that is known.
    pub run: Option<&'a Run>,
    /// Its folder, with `~` for home; `everywhere` when it had none.
    pub place: String,
    /// The name its project goes by where that is not its folder's own.
    pub own: Option<String>,
    /// The project's name, for the search.
    pub project: String,
    pub set_off: Vec<SetOff>,
}

impl Item<'_> {
    pub fn key(&self) -> String {
        format!("e:{}", self.event.id)
    }

    /// The keys of its lines: its own, then one per thing it set off.
    pub fn keys(&self) -> impl Iterator<Item = String> + '_ {
        std::iter::once(self.key()).chain((0..self.set_off.len()).map(|n| format!("e:{}/{n}", self.event.id)))
    }

    fn title(&self) -> &str {
        self.run.map_or("", |r| r.title.as_str())
    }
}

/// The events, newest first, narrowed to what the search hits.
pub fn build<'a>(feed: &'a Feed, rules: &[Rule], names: &Names, filter: &str) -> Vec<Item<'a>> {
    let mut links = links(feed, rules);
    let query = Query::parse(filter);
    feed.events
        .iter()
        .filter_map(|e| {
            let run = e.run.as_deref().and_then(|id| feed.runs.get(id));
            let (place, own, project) = if e.workspace.as_os_str().is_empty() {
                (rows::EVERYWHERE.to_string(), None, rows::EVERYWHERE.to_string())
            } else {
                let key = run.map_or_else(|| e.workspace.to_string_lossy().into_owned(), Run::place_key);
                (
                    names.place(&key, &e.workspace),
                    names.own(&key, &e.workspace),
                    names.group(&key),
                )
            };
            let item = Item {
                event: e,
                run,
                place,
                own,
                project,
                set_off: links.remove(&e.id).unwrap_or_default(),
            };
            query.hits(&subject(&item)).then_some(item)
        })
        .collect()
}

/// What a search matches an event on: its kind, its run's title and what it
/// said; its project; the rules behind it and those it set off.
fn subject<'a>(it: &'a Item) -> Subject<'a> {
    let e = it.event;
    let mut by: Vec<&str> = Vec::new();
    if let Some(Cause::Schedule(n) | Cause::Reaction(n)) = it.run.map(|r| &r.cause) {
        by.push(n);
    }
    by.extend(data_str(e, "schedule"));
    by.extend(it.set_off.iter().filter_map(|s| s.rule().map(|(_, n)| n)));
    Subject {
        text: vec![&e.kind, it.title(), &e.summary],
        project: vec![&it.project, &it.place, e.workspace.to_str().unwrap_or("")],
        is: vec![],
        kind: vec![&e.kind],
        by,
        on: it.run.and_then(|r| r.branch.as_deref()),
    }
}

/// An event as the search ranks it: its run's title, its project, what it
/// said, then its kind.
pub fn named(it: &Item) -> Named {
    Named {
        title: it.title().to_string(),
        project: it.project.clone(),
        summary: it.event.summary.clone(),
        rest: vec![it.event.kind.clone()],
        ..Named::default()
    }
}

/// Where `enter` on a line goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A run, on the list.
    Run(String),
    /// A rule, in the graph: its kind, its name, and the folder it acted in
    /// to tell it from another of the same name.
    Rule(RuleKind, String, Option<PathBuf>),
}

/// What `enter` on the line with `key` goes to: an event's run, or the rule
/// behind what it set off (an answer's, the run it started).
pub fn target(items: &[Item], feed: &Feed, key: &str) -> Option<Target> {
    let (id, n) = parse_key(key)?;
    let it = items.iter().find(|i| i.event.id == id)?;
    let e = it.event;
    match n {
        None => match (&e.run, data_str(e, "schedule")) {
            (Some(run), _) => Some(Target::Run(run.clone())),
            (None, Some(s)) => Some(Target::Rule(RuleKind::Schedule, s.into(), Some(e.workspace.clone()))),
            _ => None,
        },
        Some(n) => {
            let s = it.set_off.get(n)?;
            match s.rule() {
                Some((kind, name)) => {
                    let near = s
                        .run()
                        .and_then(|id| feed.runs.get(id))
                        .map_or_else(|| e.workspace.clone(), |r| r.workspace.clone());
                    Some(Target::Rule(kind, name.to_string(), Some(near)))
                }
                None => s.run().map(|r| Target::Run(r.to_string())),
            }
        }
    }
}

/// A line's key: the event's id, and which of what it set off.
fn parse_key(key: &str) -> Option<(i64, Option<usize>)> {
    let rest = key.strip_prefix("e:")?;
    Some(match rest.split_once('/') {
        Some((id, n)) => (id.parse().ok()?, Some(n.parse().ok()?)),
        None => (rest.parse().ok()?, None),
    })
}

/// The event a line is, or is under.
pub fn item_of<'a, 'b>(items: &'a [Item<'b>], key: &str) -> Option<&'a Item<'b>> {
    let (id, _) = parse_key(key)?;
    items.iter().find(|i| i.event.id == id)
}

/// The graph's key for a rule by its kind and name: the one in `near`'s
/// folder, else the global one, else any.
pub fn rule_key(rules: &[Rule], kind: RuleKind, name: &str, near: Option<&Path>) -> Option<String> {
    let mut of: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.kind() == kind && r.name() == name && r.removed_at().is_none())
        .collect();
    of.sort_by_key(|r| (near.is_none_or(|p| Path::new(r.scope()) != p), !r.is_global()));
    of.first().map(|r| rules::key(r))
}

/// The header's words: how many events, `+` when there are older ones to
/// read, and the search in force.
pub fn headline(shown: usize, feed: &Feed, filter: &str) -> String {
    let more = if feed.more { "+" } else { "" };
    let all = feed.events.len();
    if !feed.read {
        return "events · reading…".into();
    }
    if filter.is_empty() {
        format!("events · {all}{more}")
    } else {
        format!("events · {shown} of {all}{more} · ⌕ {filter}")
    }
}

/// A kind in the colour of what it means: red for what went wrong, amber
/// for what waits on you.
fn kind_style(kind: &str) -> Style {
    match kind {
        event_kind::RUN_FAILED | event_kind::RUN_LOST | event_kind::RUN_TROUBLE => {
            Style::default().fg(theme::BAD).bold()
        }
        event_kind::RUN_NEEDS_INPUT | event_kind::RUN_BLOCKED | event_kind::DECISION_RAISED => theme::needs(),
        _ => Style::default(),
    }
}

/// The columns, for a width: kind, project and title, the summary taking
/// what is left.
struct Cols {
    kind: usize,
    place: usize,
    title: usize,
}

fn cols(items: &[Item], w: usize) -> Cols {
    let widest = items.iter().map(|i| i.event.kind.chars().count()).max().unwrap_or(0);
    Cols {
        kind: widest.clamp(9, 22),
        place: (w * 24 / 100).clamp(10, 34),
        title: (w * 24 / 100).clamp(10, 36),
    }
}

/// The view's lines to the width, and each line's key — an event's, or one
/// of what it set off — in step, for the cursor and the mouse.
pub fn lines(items: &[Item], feed: &Feed, w: usize, now: Timestamp) -> (Vec<Line<'static>>, Vec<String>) {
    let c = cols(items, w);
    let mut lines = Vec::new();
    let mut keys = Vec::new();
    for it in items {
        let e = it.event;
        let (parent, last, own) = place_parts(&it.place, it.own.as_deref(), c.place);
        let used = [&parent, &last, &own].iter().map(|s| s.chars().count()).sum::<usize>();
        let fixed = KIND_COL + c.kind + 1 + c.place + 1 + c.title + 1;
        lines.push(Line::from(vec![
            Span::raw(format!(" {:>4}  ", fmt_age(e.at, now))).dim(),
            Span::styled(fit(&e.kind, c.kind), kind_style(&e.kind)),
            Span::raw(" "),
            Span::styled(parent, theme::dim()),
            Span::styled(last, theme::name()),
            Span::styled(own, theme::dim()),
            Span::raw(" ".repeat(c.place.saturating_sub(used) + 1)),
            Span::raw(fit(it.title(), c.title)),
            Span::raw(" "),
            Span::raw(
                fit(e.summary.lines().next().unwrap_or(""), w.saturating_sub(fixed))
                    .trim_end()
                    .to_string(),
            )
            .dim(),
        ]));
        keys.push(it.key());
        for (n, s) in it.set_off.iter().enumerate() {
            let room = w.saturating_sub(KIND_COL + 2);
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(KIND_COL)),
                Span::raw("↳ ").dim(),
                Span::raw(fit(&s.words(feed), room).trim_end().to_string()),
            ]));
            keys.push(format!("e:{}/{n}", e.id));
        }
    }
    (lines, keys)
}

fn fact(k: &str, v: impl Into<String>) -> Line<'static> {
    Line::from(vec![Span::raw(format!(" {k:<8} ")).dim(), Span::raw(v.into())])
}

/// The pane beside the view: the event in full — when, where, its run, all
/// it said and carried — and what it set off.
pub fn pane_lines(it: &Item, feed: &Feed, now: Timestamp) -> Vec<Line<'static>> {
    let e = it.event;
    let mut lines = vec![Line::from(Span::styled(
        format!(" {}", e.kind),
        kind_style(&e.kind).bold(),
    ))];
    lines.push(fact(
        "when",
        format!("{} ago · {}", fmt_age(e.at, now), fmt_clock(e.at, now)),
    ));
    let place = match &it.own {
        Some(own) => format!("{} · {own}", it.place),
        None => it.place.clone(),
    };
    lines.push(fact("where", place));
    if let Some(r) = it.run {
        lines.push(fact("run", format!("{} · {}", r.title, r.state)));
        lines.push(fact("started", rows::cause_words(&r.cause)));
    } else if let Some(id) = &e.run {
        lines.push(fact("run", id.clone()));
    }
    lines.push(Line::raw(""));
    for l in e.summary.lines() {
        lines.push(Line::from(Span::raw(format!(" {l}"))));
    }
    if let Some(serde_json::Value::Object(map)) = &e.data {
        lines.push(Line::raw(""));
        for (k, v) in map.iter().filter(|(_, v)| !v.is_null()) {
            let v = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            lines.push(fact(k, v));
        }
    }
    if !it.set_off.is_empty() {
        lines.push(Line::raw(""));
        for s in &it.set_off {
            lines.push(Line::from(vec![Span::raw(" ↳ ").dim(), Span::raw(s.words(feed))]));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::action::Task;
    use rl_proto::reaction::{Reaction, ReactionSpec};
    use rl_proto::schedule::{GLOBAL_SCOPE, Schedule, ScheduleSpec};
    use rl_proto::types::RunKind;
    use serde_json::json;

    fn t(s: &str) -> Timestamp {
        format!("2026-09-28T10:{s}Z").parse().unwrap()
    }

    fn ev(id: i64, at: &str, kind: &str, run: Option<&str>, summary: &str) -> Event {
        Event {
            id,
            at: t(at),
            workspace: PathBuf::from("/w/api"),
            run: run.map(str::to_string),
            group_key: "/w/api".into(),
            kind: kind.into(),
            summary: summary.into(),
            data: None,
        }
    }

    fn with(mut e: Event, data: serde_json::Value) -> Event {
        e.data = Some(data);
        e
    }

    fn run(id: &str, title: &str, cause: Cause, flow: &str) -> Run {
        let mut r = Run::new(id, RunKind::Command, title, vec![], "/w/api", "/w/api", t("00:00"));
        r.cause = cause;
        r.flow = flow.into();
        r
    }

    fn reaction(name: &str, on: &[&str]) -> Rule {
        Rule::Reaction(Reaction {
            scope: GLOBAL_SCOPE.into(),
            name: name.into(),
            hash: "h".into(),
            source: "/h/reactions.yaml".into(),
            spec: ReactionSpec {
                on: on.iter().map(|s| s.to_string()).collect(),
                run: Some(Task::Shell("echo reacted".into())),
                ..Default::default()
            },
            paused: false,
            last_fired_at: None,
            last_run: None,
            removed_at: None,
            approved_hash: Some("h".into()),
        })
    }

    fn schedule(scope: &str, name: &str) -> Rule {
        Rule::Schedule(Schedule {
            scope: scope.into(),
            name: name.into(),
            hash: "h".into(),
            source: "/h/schedules.yaml".into(),
            spec: ScheduleSpec {
                every: Some("1h".into()),
                run: Some(Task::Shell("make".into())),
                ..Default::default()
            },
            paused: false,
            last_fired_at: None,
            last_run: None,
            next_at: None,
            removed_at: None,
            approved_hash: Some("h".into()),
        })
    }

    fn question(id: &str, run: Option<&str>, group: Option<&str>, title: &str) -> Decision {
        serde_json::from_value(json!({
            "id": id, "workspace": "/w/api", "run": run, "flow": "f", "group": group, "title": title,
            "options": [], "status": "open", "created_at": "2026-09-28T10:00:00Z",
            "updated_at": "2026-09-28T10:00:00Z",
        }))
        .unwrap()
    }

    /// A manual run that failed, a reaction that heard it and started a run
    /// in its chain, another that asked about it, a schedule that fired, and
    /// an answer that started a run: newest first, as they are read.
    fn feed() -> Feed {
        let mut f = Feed::default();
        f.newer(
            vec![
                ev(1, "00:00", "run.started", Some("a"), "started `cargo test`"),
                ev(2, "00:05", "run.failed", Some("a"), "exit 1 · 5s"),
                ev(3, "00:06", "run.started", Some("r"), "started `echo reacted`"),
                with(
                    ev(4, "00:06", "decision.raised", Some("a"), "asks: retry?"),
                    json!({ "decision": "q1" }),
                ),
                with(
                    ev(5, "01:00", "schedule.fired", Some("s"), "schedule `nightly` fired"),
                    json!({ "schedule": "nightly", "run": "s" }),
                ),
                ev(6, "01:00", "run.started", Some("s"), "started `make`"),
                with(
                    ev(7, "02:00", "decision.taken", Some("a"), "chose retry"),
                    json!({ "decision": "q1", "chosen": ["retry"], "runs": ["o"] }),
                ),
            ],
            PAGE as usize,
        );
        for r in [
            run("a", "cargo test", Cause::Manual, "a"),
            run("r", "echo reacted", Cause::Reaction("notify".into()), "a"),
            run("s", "make", Cause::Schedule("nightly".into()), "s"),
            run("o", "retry it", Cause::Decision("q1".into()), "a"),
        ] {
            f.runs.insert(r.id.clone(), r);
        }
        f.questions.insert(
            "q1".into(),
            question("q1", Some("a"), Some("reaction:ask-me"), "retry?"),
        );
        f
    }

    fn rules_() -> Vec<Rule> {
        vec![
            reaction("notify", &["run.failed"]),
            reaction("ask-me", &["run.*"]),
            schedule(GLOBAL_SCOPE, "nightly"),
        ]
    }

    #[test]
    fn newest_first_and_pages_join_up() {
        let mut f = Feed::default();
        f.newer(
            vec![ev(3, "00:03", "a.b", None, ""), ev(5, "00:05", "a.b", None, "")],
            2,
        );
        assert!(f.more, "a full first page: there may be older ones");
        f.older(vec![ev(1, "00:01", "a.b", None, "")], 2);
        assert!(!f.more, "a short page: nothing older");
        assert_eq!(f.events.iter().map(|e| e.id).collect::<Vec<_>>(), [5, 3, 1]);
        f.newer(vec![ev(6, "00:06", "a.b", None, "")], 2);
        assert_eq!(
            f.events.iter().map(|e| e.id).collect::<Vec<_>>(),
            [6, 5, 3, 1],
            "on top"
        );
        // A full page since the newest: there may be a gap, so it starts over.
        f.newer(
            vec![ev(9, "00:09", "a.b", None, ""), ev(8, "00:08", "a.b", None, "")],
            2,
        );
        assert_eq!(f.events.iter().map(|e| e.id).collect::<Vec<_>>(), [9, 8]);
        assert!(f.more);
    }

    #[test]
    fn what_each_event_set_off_comes_from_what_the_ledger_keeps() {
        let f = feed();
        let l = links(&f, &rules_());
        assert_eq!(
            l[&2],
            [
                SetOff::Reaction {
                    rule: "notify".into(),
                    run: "r".into()
                },
                SetOff::Question {
                    rule: "ask-me".into(),
                    question: "q1".into()
                },
            ],
            "the failure: the run a rule started in its chain, and the question another asked of its run"
        );
        assert_eq!(
            l[&5],
            [SetOff::Schedule {
                rule: "nightly".into(),
                run: "s".into()
            }]
        );
        assert_eq!(l[&7], [SetOff::Answer { run: "o".into() }]);
        assert_eq!(l.len(), 3, "nothing else set anything off");
        assert!(
            !l.contains_key(&1),
            "`ask-me` hears run.* but not its run.started: it came before"
        );
    }

    #[test]
    fn a_reaction_is_not_pinned_on_what_it_could_not_have_heard() {
        // Another chain's failure, after its own and just before it started:
        // not the one it was in.
        let mut f = Feed::default();
        f.newer(
            vec![
                ev(1, "00:05", "run.failed", Some("a"), "exit 1"),
                ev(2, "00:05", "run.failed", Some("b"), "exit 2"),
                ev(3, "00:06", "run.started", Some("r"), "started `echo reacted`"),
            ],
            PAGE as usize,
        );
        for r in [
            run("a", "cargo test", Cause::Manual, "a"),
            run("b", "other", Cause::Manual, "b"),
            run("r", "echo reacted", Cause::Reaction("notify".into()), "a"),
        ] {
            f.runs.insert(r.id.clone(), r);
        }
        let l = links(&f, &rules_());
        assert!(!l.contains_key(&2), "not the other chain's");
        assert_eq!(
            l[&1],
            [SetOff::Reaction {
                rule: "notify".into(),
                run: "r".into()
            }],
            "the one in its chain"
        );

        // Longer ago than the reactor acts: history, not a cause.
        let mut f = feed();
        f.events.retain(|e| e.id != 3);
        f.events
            .insert(0, ev(30, "09:00", "run.started", Some("r"), "started `echo reacted`"));
        let l = links(&f, &rules_());
        assert!(
            l.get(&2)
                .is_none_or(|v| v.iter().all(|s| !matches!(s, SetOff::Reaction { .. })))
        );

        // A rule that does not listen for the kind did not fire on it.
        let f = feed();
        let l = links(
            &f,
            &[reaction("notify", &["run.finished"]), reaction("ask-me", &["run.*"])],
        );
        assert!(l[&2].iter().all(|s| !matches!(s, SetOff::Reaction { .. })));
    }

    /// Two failures in one chain, a moment apart, and a rule that ran for
    /// each and asked about the first: newest first, as they are read. With
    /// `kept`, the runs and the question name the event they fired on, as
    /// they do now; without, as they did before.
    fn two_failures(kept: bool) -> Feed {
        let on = |e: Event, id: i64| if kept { with(e, json!({ "event": id })) } else { e };
        let mut asked = json!({ "decision": "q1" });
        if kept {
            asked["reaction"] = json!("ask-me");
            asked["event"] = json!(1);
        }
        let mut f = Feed::default();
        f.newer(
            vec![
                ev(1, "00:05", "run.failed", Some("a"), "exit 1"),
                ev(2, "00:05", "run.failed", Some("b"), "exit 2"),
                on(ev(3, "00:06", "run.started", Some("r1"), "started `echo reacted`"), 1),
                on(ev(4, "00:06", "run.started", Some("r2"), "started `echo reacted`"), 2),
                with(ev(5, "00:06", "decision.raised", Some("a"), "asks: retry?"), asked),
            ],
            PAGE as usize,
        );
        for r in [
            run("a", "cargo test", Cause::Manual, "a"),
            run("b", "cargo clippy", Cause::Manual, "a"),
            run("r1", "echo reacted", Cause::Reaction("notify".into()), "a"),
            run("r2", "echo reacted", Cause::Reaction("notify".into()), "a"),
        ] {
            f.runs.insert(r.id.clone(), r);
        }
        // Its own group: nothing but what the event says ties it to a rule.
        f.questions
            .insert("q1".into(), question("q1", Some("a"), Some("retries"), "retry?"));
        f
    }

    #[test]
    fn a_reaction_that_kept_its_event_is_linked_to_that_one() {
        let reacted = |run: &str| SetOff::Reaction {
            rule: "notify".into(),
            run: run.into(),
        };
        let l = links(&two_failures(true), &rules_());
        assert_eq!(
            l[&1],
            [
                reacted("r1"),
                SetOff::Question {
                    rule: "ask-me".into(),
                    question: "q1".into()
                }
            ],
            "the first failure: its own run, and the question about it, though the second came after"
        );
        assert_eq!(l[&2], [reacted("r2")], "the second: its own run");

        // Without it, as runs from before were kept: read back from the chain,
        // both land on the newest failure, and a question grouped its own way
        // stands alone.
        let l = links(&two_failures(false), &rules_());
        assert_eq!(l[&2], [reacted("r1"), reacted("r2")]);
        assert!(!l.contains_key(&1));
    }

    #[test]
    fn a_reaction_to_an_event_with_no_run_is_found_by_kind_among_those_without() {
        let mut f = Feed::default();
        f.newer(
            vec![
                ev(1, "00:00", "log.issue", None, "disk full"),
                ev(2, "00:00", "log.issue", Some("x"), "from a run"),
                ev(3, "00:01", "run.started", Some("r"), "started `triage`"),
            ],
            PAGE as usize,
        );
        f.runs.insert("x".into(), run("x", "x", Cause::Manual, "x"));
        // Its own chain: the event it heard had none.
        f.runs
            .insert("r".into(), run("r", "triage", Cause::Reaction("triage".into()), "r"));
        let l = links(&f, &[reaction("triage", &["log.issue"])]);
        assert_eq!(
            l[&1],
            [SetOff::Reaction {
                rule: "triage".into(),
                run: "r".into()
            }]
        );
        assert!(!l.contains_key(&2));
    }

    fn names() -> Names {
        Names::new([(PathBuf::from("/w/api"), "backend".to_string())])
    }

    #[test]
    fn rows_are_newest_first_with_what_they_set_off_under_them() {
        let f = feed();
        let items = build(&f, &rules_(), &names(), "");
        let keys: Vec<String> = items.iter().flat_map(|i| i.keys().collect::<Vec<_>>()).collect();
        assert_eq!(
            keys,
            [
                "e:7", "e:7/0", "e:6", "e:5", "e:5/0", "e:4", "e:3", "e:2", "e:2/0", "e:2/1", "e:1"
            ]
        );
        let (lines, line_keys) = lines(&items, &f, 120, t("03:00"));
        assert_eq!(line_keys, keys, "a key for every line, in step");
        let text: Vec<String> = lines.iter().map(text).collect();
        assert!(
            text[8].trim_start().starts_with("↳ ⚡ notify → started 'echo reacted'"),
            "{}",
            text[8]
        );
        assert!(
            text[9].trim_start().starts_with("↳ ? ask-me asked 'retry?'"),
            "{}",
            text[9]
        );
        assert!(
            text[4].trim_start().starts_with("↳ ↻ nightly → started 'make'"),
            "{}",
            text[4]
        );
        assert!(text[1].trim_start().starts_with("↳ ▶ the answer started 'retry it'"));
    }

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The screen column `needle` starts at.
    fn col_of(line: &str, needle: &str) -> usize {
        let at = line.find(needle).unwrap_or_else(|| panic!("{needle} not in {line}"));
        line[..at].chars().map(|c| Span::raw(c.to_string()).width()).sum()
    }

    #[test]
    fn the_columns_line_up_whatever_the_lengths() {
        let mut f = feed();
        f.events[0].workspace = PathBuf::from("/somewhere/rather/much/deeper/than/the/rest");
        f.events[0].kind = "plugin.gchat.mention".into();
        f.events[0].run = None;
        let items = build(&f, &rules_(), &names(), "");
        let (lines, keys) = lines(&items, &f, 140, t("03:00"));
        let rows: Vec<String> = lines
            .iter()
            .zip(&keys)
            .filter(|(_, k)| !k.contains('/'))
            .map(|(l, _)| text(l))
            .collect();
        let kinds: Vec<usize> = items.iter().zip(&rows).map(|(i, l)| col_of(l, &i.event.kind)).collect();
        assert!(kinds.iter().all(|c| *c == KIND_COL), "{kinds:?}");
        let titles: Vec<usize> = items
            .iter()
            .zip(&rows)
            .filter(|(i, _)| !i.title().is_empty())
            .map(|(i, l)| col_of(l, i.title()))
            .collect();
        assert!(titles.windows(2).all(|w| w[0] == w[1]), "{titles:?}");
        let places: Vec<usize> = rows.iter().skip(1).map(|l| col_of(l, "api")).collect();
        assert!(places.windows(2).all(|w| w[0] == w[1]), "{places:?}");
        assert!(
            rows[1].contains("/w/api · backend"),
            "the folder and the name it goes by: {}",
            rows[1]
        );
        assert!(
            rows[0].contains("…"),
            "a long folder gives way from the left: {}",
            rows[0]
        );
    }

    fn shown(filter: &str) -> Vec<i64> {
        let f = feed();
        build(&f, &rules_(), &names(), filter)
            .iter()
            .map(|i| i.event.id)
            .collect()
    }

    #[test]
    fn the_search_narrows_by_kind_project_rule_and_words() {
        assert_eq!(shown("kind:run.failed"), [2]);
        assert_eq!(shown("kind:run.f"), [2], "by prefix");
        assert_eq!(shown("kind:decision"), [7, 4]);
        assert_eq!(shown("in:backend"), [7, 6, 5, 4, 3, 2, 1]);
        assert!(shown("in:web").is_empty());
        assert_eq!(
            shown("by:nightly"),
            [6, 5],
            "its run's rule, or the schedule that fired"
        );
        assert_eq!(shown("by:notify"), [3, 2], "the rule's own run, and what set it off");
        assert_eq!(shown("reacted"), [3], "words over the title and what it said");
        assert_eq!(shown("exit"), [2]);
        assert_eq!(shown("failed kind:run"), [2]);
    }

    #[test]
    fn enter_goes_to_the_run_or_to_the_rule() {
        let f = feed();
        let rules = rules_();
        let items = build(&f, &rules, &names(), "");
        let to = |k: &str| target(&items, &f, k);
        assert_eq!(to("e:2"), Some(Target::Run("a".into())), "an event: its run");
        assert_eq!(
            to("e:2/0"),
            Some(Target::Rule(RuleKind::Reaction, "notify".into(), Some("/w/api".into()))),
            "a reaction's line: its rule"
        );
        assert_eq!(
            to("e:5/0"),
            Some(Target::Rule(
                RuleKind::Schedule,
                "nightly".into(),
                Some("/w/api".into())
            ))
        );
        assert_eq!(
            to("e:7/0"),
            Some(Target::Run("o".into())),
            "an answer's: the run it started"
        );
        assert_eq!(to("e:2/9"), None);
        assert_eq!(to("q:1"), None);

        // A schedule that was skipped has no run: its rule, then.
        let mut f = Feed::default();
        f.newer(
            vec![with(
                ev(1, "00:00", "schedule.skipped", None, "skipped"),
                json!({ "schedule": "nightly" }),
            )],
            PAGE as usize,
        );
        let items = build(&f, &rules, &names(), "");
        assert_eq!(
            target(&items, &f, "e:1"),
            Some(Target::Rule(
                RuleKind::Schedule,
                "nightly".into(),
                Some("/w/api".into())
            ))
        );
    }

    #[test]
    fn a_rule_is_the_one_where_it_acted_else_the_global_one() {
        let rules = vec![
            schedule("/w/web", "nightly"),
            schedule(GLOBAL_SCOPE, "nightly"),
            schedule("/w/api", "nightly"),
        ];
        let key = |near: Option<&str>| rule_key(&rules, RuleKind::Schedule, "nightly", near.map(Path::new));
        assert_eq!(key(Some("/w/api")).as_deref(), Some("schedule:/w/api/nightly"));
        assert_eq!(key(Some("/elsewhere")).as_deref(), Some("schedule:global/nightly"));
        assert_eq!(rule_key(&rules, RuleKind::Reaction, "nightly", None), None);
    }

    #[test]
    fn the_header_says_how_many_and_the_search() {
        let mut f = Feed::default();
        assert_eq!(headline(0, &f, ""), "events · reading…");
        f.newer(
            vec![ev(1, "00:00", "a.b", None, ""), ev(2, "00:00", "a.b", None, "")],
            2,
        );
        assert_eq!(headline(2, &f, ""), "events · 2+");
        assert_eq!(headline(1, &f, "kind:a"), "events · 1 of 2+ · ⌕ kind:a");
    }

    #[test]
    fn what_is_still_to_ask_about_is_what_the_events_name() {
        let mut f = feed();
        f.runs.remove("o");
        f.questions.clear();
        assert_eq!(f.unknown_runs(), ["o"], "an answer's run too");
        assert_eq!(f.unknown_questions(), ["q1"]);
        f.gone.insert("o".into());
        assert!(f.unknown_runs().is_empty(), "not asked for twice");
        assert!(f.kinds().contains(&"run.failed".to_string()));
    }
}
