//! The events view (`/events`): what the ledger saw, newest first, one line
//! each — how long ago, what kind, where, which run, what it said — and under
//! an event, indented, what it set off.
//!
//! What it set off is read from what the ledger keeps, never guessed:
//!
//! - a flow's firing is `flow.fired` (its `firing`, and for one fired by an
//!   event, that event's id in `event`);
//! - a step's run carries its flow and step (`cause: flow:<flow>/<step>`)
//!   and its firing as its chain, and its `run.started` names the event its
//!   firing was fired by (`data.event`);
//! - a step's question names its flow (`decision.raised`'s `flow`), and its
//!   chain is its firing;
//! - an answer names the runs its options started (`decision.taken`'s and
//!   `decision.expired`'s `runs`).
//!
//! A step's run and question go under the event its firing was fired by,
//! else under the firing's `flow.fired`.
//!
//! Pure: what was fetched in, rows and lines out. Fetching is the panel's.

use super::query::{Named, Query, Subject};
use super::rows::{self, Names, fit, place_parts};
use super::theme;
use crate::render::fmt_clock;
use jiff::Timestamp;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use rl_core::time::fmt_age;
use rl_proto::decision::Decision;
use rl_proto::types::{Cause, Event, Run, event_kind};
use std::collections::{HashMap, HashSet};

/// How many events one fetch takes: the newest page, and each older one.
pub const PAGE: u32 = 500;

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
    /// The facts of runs opened from the report, with their recap split once asked.
    pub facts: HashMap<String, rl_proto::RunFacts>,
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
    /// A flow's step started a run.
    Flow { flow: String, run: String },
    /// A flow's step asked.
    Question { flow: String, question: String },
    /// An option taken started a run.
    Answer { run: String },
}

impl SetOff {
    /// The flow behind it, when a flow is.
    pub fn flow(&self) -> Option<&str> {
        match self {
            SetOff::Flow { flow, .. } | SetOff::Question { flow, .. } => Some(flow),
            SetOff::Answer { .. } => None,
        }
    }

    pub fn run(&self) -> Option<&str> {
        match self {
            SetOff::Flow { run, .. } | SetOff::Answer { run } => Some(run),
            SetOff::Question { .. } => None,
        }
    }

    /// The line in words, after its `↳`.
    pub fn words(&self, feed: &Feed) -> String {
        let title = |id: &str| feed.runs.get(id).map_or_else(|| id.to_string(), |r| r.title.clone());
        match self {
            SetOff::Flow { flow, run } => {
                let step = match feed.runs.get(run).map(|r| &r.cause) {
                    Some(Cause::Flow { step, .. }) => format!(" · {step}"),
                    _ => String::new(),
                };
                format!("↻ {flow}{step} → started '{}'", title(run))
            }
            SetOff::Question { flow, question } => {
                let t = feed
                    .questions
                    .get(question)
                    .map_or(question.as_str(), |q| q.title.as_str());
                format!("? {flow} asked '{t}'")
            }
            SetOff::Answer { run } => format!("▶ the answer started '{}'", title(run)),
        }
    }
}

/// What each event set off, by the event's id; see the module's head for
/// how each is read.
pub fn links(feed: &Feed) -> HashMap<i64, Vec<SetOff>> {
    let evs = &feed.events;
    // Each firing read so far: its `flow.fired`, and the event that fired it.
    let fired: HashMap<&str, (i64, Option<i64>)> = evs
        .iter()
        .filter(|e| e.kind == event_kind::FLOW_FIRED)
        .filter_map(|e| Some((data_str(e, "firing")?, (e.id, data_i64(e, "event")))))
        .collect();
    let read: HashSet<i64> = evs.iter().map(|e| e.id).collect();
    // Where a firing's doing goes: under the event that fired it, else under
    // its firing; none when neither was read.
    let under = |firing: &str, event: Option<i64>| {
        let (fired_at, by) = match fired.get(firing) {
            Some((id, by)) => (Some(*id), by.or(event)),
            None => (None, event),
        };
        by.filter(|id| read.contains(id)).or(fired_at)
    };
    // Each with the id of the event that says it, so they read in the order
    // they happened: they are found newest first.
    let mut out: HashMap<i64, Vec<(i64, SetOff)>> = HashMap::new();
    for e in evs {
        match e.kind.as_str() {
            event_kind::DECISION_TAKEN | event_kind::DECISION_EXPIRED => {
                for run in runs_named(e).into_iter().filter(|r| Some(r) != e.run.as_ref()) {
                    out.entry(e.id).or_default().push((e.id, SetOff::Answer { run }));
                }
            }
            event_kind::RUN_STARTED => {
                let Some(r) = e.run.as_deref().and_then(|id| feed.runs.get(id)) else {
                    continue;
                };
                let Cause::Flow { flow, .. } = &r.cause else { continue };
                if let Some(id) = under(&r.chain, data_i64(e, "event")) {
                    let s = SetOff::Flow {
                        flow: flow.clone(),
                        run: r.id.clone(),
                    };
                    out.entry(id).or_default().push((e.id, s));
                }
            }
            event_kind::DECISION_RAISED => {
                let (Some(qid), Some(flow)) = (data_str(e, "decision"), data_str(e, "flow")) else {
                    continue;
                };
                let chain = feed.questions.get(qid).map(|q| q.chain.as_str());
                if let Some(id) = chain.and_then(|c| under(c, None)) {
                    let s = SetOff::Question {
                        flow: flow.to_string(),
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
pub fn build<'a>(feed: &'a Feed, names: &Names, filter: &str) -> Vec<Item<'a>> {
    let mut links = links(feed);
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
/// said; its project; the flows behind it and those it set off.
fn subject<'a>(it: &'a Item) -> Subject<'a> {
    let e = it.event;
    let mut by: Vec<&str> = Vec::new();
    if let Some(Cause::Flow { flow, .. }) = it.run.map(|r| &r.cause) {
        by.push(flow);
    }
    by.extend(data_str(e, "flow"));
    by.extend(it.set_off.iter().filter_map(SetOff::flow));
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
    /// A flow, by name, in its project's overview.
    Flow(String),
}

/// What `enter` on the line with `key` goes to: an event's run, else the
/// flow it is about (`flow.fired` and the like); the flow behind what it set
/// off, or an answer's run.
pub fn target(items: &[Item], key: &str) -> Option<Target> {
    let (id, n) = parse_key(key)?;
    let it = items.iter().find(|i| i.event.id == id)?;
    let e = it.event;
    match n {
        None => match (&e.run, data_str(e, "flow")) {
            (Some(run), _) => Some(Target::Run(run.clone())),
            (None, Some(flow)) => Some(Target::Flow(flow.into())),
            _ => None,
        },
        Some(n) => {
            let s = it.set_off.get(n)?;
            match s.flow() {
                Some(flow) => Some(Target::Flow(flow.to_string())),
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
    use rl_proto::types::RunKind;
    use serde_json::json;
    use std::path::PathBuf;

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

    fn run(id: &str, title: &str, cause: Cause, chain: &str) -> Run {
        let mut r = Run::new(id, RunKind::Command, title, vec![], "/w/api", "/w/api", t("00:00"));
        r.cause = cause;
        r.chain = chain.into();
        r
    }

    fn step(flow: &str, step: &str) -> Cause {
        Cause::Flow {
            flow: flow.into(),
            step: step.into(),
        }
    }

    fn question(id: &str, run: Option<&str>, chain: &str, title: &str) -> Decision {
        serde_json::from_value(json!({
            "id": id, "workspace": "/w/api", "run": run, "chain": chain, "title": title,
            "options": [], "status": "open", "created_at": "2026-09-28T10:00:00Z",
            "updated_at": "2026-09-28T10:00:00Z",
        }))
        .unwrap()
    }

    /// A run you started that failed; a flow fired on that failure whose
    /// step ran, and another whose step asked; a flow fired on time whose
    /// step ran; and an answer to the failed run's own question that started
    /// a run: newest first, as they are read.
    fn feed() -> Feed {
        let mut f = Feed::default();
        f.newer(
            vec![
                ev(1, "00:00", "run.started", Some("a"), "started `cargo test`"),
                ev(2, "00:05", "run.failed", Some("a"), "exit 1 · 5s"),
                with(
                    ev(3, "00:06", "flow.fired", None, "flow `notify` fired"),
                    json!({ "flow": "notify", "firing": "f1", "trigger": "on", "event": 2 }),
                ),
                with(
                    ev(4, "00:06", "run.started", Some("r"), "started `notify · tell`"),
                    json!({ "event": 2 }),
                ),
                with(
                    ev(5, "00:06", "flow.fired", None, "flow `ask-me` fired"),
                    json!({ "flow": "ask-me", "firing": "f2", "trigger": "on", "event": 2 }),
                ),
                with(
                    ev(6, "00:06", "decision.raised", None, "asks: retry?"),
                    json!({ "decision": "q1", "flow": "ask-me" }),
                ),
                with(
                    ev(7, "01:00", "flow.fired", None, "flow `nightly` fired"),
                    json!({ "flow": "nightly", "firing": "f3", "trigger": "when" }),
                ),
                ev(8, "01:00", "run.started", Some("s"), "started `nightly · make`"),
                with(
                    ev(9, "02:00", "decision.taken", Some("a"), "chose retry"),
                    json!({ "decision": "q0", "chosen": ["retry"], "runs": ["o"] }),
                ),
            ],
            PAGE as usize,
        );
        for r in [
            run("a", "cargo test", Cause::Manual, "a"),
            run("r", "notify · tell", step("notify", "tell"), "f1"),
            run("s", "nightly · make", step("nightly", "make"), "f3"),
            run("o", "retry it", Cause::Decision("q0".into()), "a"),
        ] {
            f.runs.insert(r.id.clone(), r);
        }
        f.questions
            .insert("q0".into(), question("q0", Some("a"), "a", "try again?"));
        f.questions.insert("q1".into(), question("q1", None, "f2", "retry?"));
        f
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
        let l = links(&feed());
        assert_eq!(
            l[&2],
            [
                SetOff::Flow {
                    flow: "notify".into(),
                    run: "r".into()
                },
                SetOff::Question {
                    flow: "ask-me".into(),
                    question: "q1".into()
                },
            ],
            "the failure: the run one flow's step started on it, and the question another's asked"
        );
        assert_eq!(
            l[&7],
            [SetOff::Flow {
                flow: "nightly".into(),
                run: "s".into()
            }],
            "a firing on time: its step's run under it"
        );
        assert_eq!(l[&9], [SetOff::Answer { run: "o".into() }]);
        assert_eq!(l.len(), 3, "nothing else set anything off");
    }

    #[test]
    fn what_a_firing_did_goes_under_it_when_its_event_was_not_read() {
        let mut f = feed();
        f.events.retain(|e| e.id != 2);
        let l = links(&f);
        assert_eq!(
            l[&3],
            [SetOff::Flow {
                flow: "notify".into(),
                run: "r".into()
            }]
        );
        assert_eq!(
            l[&5],
            [SetOff::Question {
                flow: "ask-me".into(),
                question: "q1".into()
            }]
        );

        // Nor its firing: the run's own word for the event, then.
        let mut f = feed();
        f.events.retain(|e| e.id != 3);
        assert_eq!(
            links(&f)[&2][0],
            SetOff::Flow {
                flow: "notify".into(),
                run: "r".into()
            }
        );
        // A question with neither stands on its own line.
        let mut f = feed();
        f.events.retain(|e| e.id != 5 && e.id != 2);
        assert!(
            links(&f)
                .values()
                .flatten()
                .all(|s| !matches!(s, SetOff::Question { .. }))
        );
    }

    #[test]
    fn an_event_a_flow_set_off_reads_from_its_cause() {
        let mut f = Feed::default();
        f.newer(
            vec![
                with(
                    ev(1, "00:00", "flow.fired", None, "flow `nightly` fired"),
                    json!({ "flow": "nightly", "firing": "f1", "trigger": "when" }),
                ),
                ev(2, "00:00", "run.started", Some("p"), "started `nightly · probe`"),
            ],
            PAGE as usize,
        );
        f.runs
            .insert("p".into(), run("p", "nightly · probe", step("nightly", "probe"), "f1"));
        let items = build(&f, &names(), "");
        let (lines, keys) = lines(&items, &f, 120, t("01:00"));
        let text: Vec<String> = lines.iter().map(text).collect();
        let at = keys.iter().position(|k| k == "e:1/0").expect("a line under the firing");
        assert!(
            text[at]
                .trim_start()
                .starts_with("↳ ↻ nightly · probe → started 'nightly · probe'"),
            "{}",
            text[at]
        );
    }

    fn names() -> Names {
        Names::new([(PathBuf::from("/w/api"), "backend".to_string())])
    }

    #[test]
    fn rows_are_newest_first_with_what_they_set_off_under_them() {
        let f = feed();
        let items = build(&f, &names(), "");
        let keys: Vec<String> = items.iter().flat_map(|i| i.keys().collect::<Vec<_>>()).collect();
        assert_eq!(
            keys,
            [
                "e:9", "e:9/0", "e:8", "e:7", "e:7/0", "e:6", "e:5", "e:4", "e:3", "e:2", "e:2/0", "e:2/1", "e:1"
            ]
        );
        let (lines, line_keys) = lines(&items, &f, 120, t("03:00"));
        assert_eq!(line_keys, keys, "a key for every line, in step");
        let text: Vec<String> = lines.iter().map(text).collect();
        assert!(
            text[10]
                .trim_start()
                .starts_with("↳ ↻ notify · tell → started 'notify · tell'"),
            "{}",
            text[10]
        );
        assert!(
            text[11].trim_start().starts_with("↳ ? ask-me asked 'retry?'"),
            "{}",
            text[11]
        );
        assert!(
            text[4]
                .trim_start()
                .starts_with("↳ ↻ nightly · make → started 'nightly · make'"),
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
        let items = build(&f, &names(), "");
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
        build(&f, &names(), filter).iter().map(|i| i.event.id).collect()
    }

    #[test]
    fn the_search_narrows_by_kind_project_flow_and_words() {
        assert_eq!(shown("kind:run.failed"), [2]);
        assert_eq!(shown("kind:run.f"), [2], "by prefix");
        assert_eq!(shown("kind:decision"), [9, 6]);
        assert_eq!(shown("in:backend"), [9, 8, 7, 6, 5, 4, 3, 2, 1]);
        assert!(shown("in:web").is_empty());
        assert_eq!(shown("by:nightly"), [8, 7], "its step's run, and its firing");
        assert_eq!(
            shown("by:notify"),
            [4, 3, 2],
            "its step's run, its firing, and what set it off"
        );
        assert_eq!(shown("by:ask-me"), [6, 5, 2], "its question too");
        assert_eq!(shown("tell"), [4], "words over the title and what it said");
        assert_eq!(shown("exit"), [2]);
        assert_eq!(shown("failed kind:run"), [2]);
    }

    #[test]
    fn enter_goes_to_the_run_or_to_the_flow() {
        let f = feed();
        let items = build(&f, &names(), "");
        let to = |k: &str| target(&items, k);
        assert_eq!(to("e:2"), Some(Target::Run("a".into())), "an event: its run");
        assert_eq!(
            to("e:2/0"),
            Some(Target::Flow("notify".into())),
            "a step's line: its flow"
        );
        assert_eq!(
            to("e:2/1"),
            Some(Target::Flow("ask-me".into())),
            "a step's question: its flow"
        );
        assert_eq!(to("e:7"), Some(Target::Flow("nightly".into())), "a firing: its flow");
        assert_eq!(to("e:7/0"), Some(Target::Flow("nightly".into())));
        assert_eq!(
            to("e:9/0"),
            Some(Target::Run("o".into())),
            "an answer's: the run it started"
        );
        assert_eq!(to("e:2/9"), None);
        assert_eq!(to("q:1"), None);
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
