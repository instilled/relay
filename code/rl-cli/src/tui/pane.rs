//! The right pane in words: a run's facts and recent events, or a question
//! and the keys that answer it. Pure — takes what the daemon said, gives
//! lines.

use super::rows::{Job, Names, asked_by, cause_words, fit};
use super::{rules, theme};
use crate::digest::signals;
use crate::render::{fmt_clock, state_label};
use jiff::Timestamp;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use rl_core::display_path;
use rl_core::time::{fmt_age, fmt_duration, fmt_when};
use rl_proto::decision::Decision;
use rl_proto::methods::DigestItem;
use rl_proto::rule::{NotLoaded, Rule};
use rl_proto::types::{Event, Run, RunState, Summary};
use std::path::Path;

/// Where a fact's value starts: past ` {key:<8} `.
const VALUE_COL: usize = 10;
/// How many events a run's pane lists.
const EVENTS: usize = 12;
/// Options past this have no key; they are still listed.
pub const MAX_KEYS: usize = 9;

pub fn state_style(s: RunState) -> Style {
    super::theme::state(s)
}

fn fact(k: &str, v: impl Into<String>) -> Line<'static> {
    Line::from(vec![Span::raw(format!(" {k:<8} ")).dim(), Span::raw(v.into())])
}

/// How deep a recap went, where it came from (and in how many calls, past
/// one), what stretch it covers, how old it is, and whether what it is of
/// has moved on since: `deep · by haiku · since 22:14 · 3m ago`.
pub fn recap_label(s: &Summary, now: Timestamp) -> String {
    let mut parts = Vec::new();
    if s.deep {
        parts.push("deep".to_string());
    }
    parts.push(recap_source(s));
    // One call is what `by` says already.
    if s.calls > 1 {
        parts.push(format!("{} calls", s.calls));
    }
    if let Some(from) = s.from {
        parts.push(format!("since {}", fmt_when(from, now)));
    } else if let Some(since) = &s.since {
        parts.push(format!("last {since}"));
    }
    parts.push(format!("{} ago", fmt_age(s.at, now)));
    if s.stale {
        parts.push("moved on since".into());
    }
    parts.join(" · ")
}

/// Who said it: the agent's own recap, its turns, or an assistant.
fn recap_source(s: &Summary) -> String {
    match s.source.as_str() {
        "claude recap" => "claude's own".to_string(),
        "codex turns" if s.by.is_empty() => "codex's turns".to_string(),
        "codex turns" => format!("codex's turns, by {}", s.agent),
        "codex turn" => "codex's last turn".to_string(),
        "nothing new" => "nothing new".to_string(),
        _ => format!("by {}", s.agent),
    }
}

/// What a summary is of, in words, from how it is kept (`SummaryTarget::key`);
/// None for a run.
fn summary_of(target: &str) -> Option<String> {
    if target == "all" {
        return Some("everything".into());
    }
    if let Some(path) = target.strip_prefix("p:") {
        return Some(format!("the project {}", display_path(Path::new(path))));
    }
    let job = target.strip_prefix("j:")?;
    let (rule, rest) = job.split_once(':')?;
    let name = rest.split_once('@').map_or(rest, |(n, _)| n);
    Some(format!("the {rule} {name}"))
}

/// What `rl summary` says under the text: what it is of, from when, how deep,
/// who wrote it, how many calls it took. A run's is its recap's label, and
/// `deep` first when it was read from the logs.
pub fn summary_label(s: &Summary, now: Timestamp) -> String {
    let Some(of) = summary_of(&s.target) else {
        return recap_label(s, now);
    };
    let mut parts = vec![format!("summary of {of}")];
    if let Some(from) = s.from {
        parts.push(format!("since {}", fmt_when(from, s.at)));
    }
    if s.deep {
        parts.push("deep".into());
    }
    parts.push(recap_source(s));
    if s.calls > 0 {
        parts.push(format!("{} call{}", s.calls, if s.calls == 1 { "" } else { "s" }));
    }
    if s.stale {
        parts.push("moved on since".into());
    }
    parts.join(" · ")
}

/// A summary aligned with the facts: where it came from on the `key`'s
/// line — and, but for a run's, what it is of — the words under it in the
/// value column.
fn said(key: &str, s: &Summary, width: usize, now: Timestamp) -> Vec<Line<'static>> {
    let value = width.saturating_sub(VALUE_COL + 1).max(20);
    let label = match summary_of(&s.target) {
        Some(of) => format!("of {of} · {}", recap_label(s, now)),
        None => recap_label(s, now),
    };
    let mut lines = Vec::new();
    for (i, t) in wrap(&label, value).into_iter().enumerate() {
        let key = if i == 0 { key } else { "" };
        lines.push(Line::from(Span::raw(format!(" {key:<8} {t}")).dim()));
    }
    for l in s.text.lines() {
        for t in wrap(l, value) {
            lines.push(Line::raw(format!(" {:<8} {t}", "")));
        }
    }
    lines
}

/// What a job, a project or everything was said to have done, kept from
/// the last `/summary` over it; else how to ask. A blank line first.
fn summary_section(summary: Option<&Summary>, width: usize, now: Timestamp) -> Vec<Line<'static>> {
    let mut lines = vec![Line::raw("")];
    match summary {
        Some(s) => lines.extend(said("summary", s, width, now)),
        None => lines.push(Line::from(
            Span::raw(format!(
                " {:<8} /summary says what happened · /summary deep reads the logs",
                "summary"
            ))
            .dim(),
        )),
    }
    lines
}

/// The command line as the user would have typed it: what Relay added for
/// its own hooks (`--settings <file>`) is left out.
fn shown_argv(argv: &[String]) -> String {
    let mut out = Vec::with_capacity(argv.len());
    let mut skip = false;
    for a in argv {
        if skip {
            skip = false;
            continue;
        }
        if a == "--settings" {
            skip = true;
            continue;
        }
        out.push(a.as_str());
    }
    out.join(" ")
}

/// A run: what it is, why it is, where it stands, what an assistant said
/// about it if asked, what happened lately.
/// A run in the pane, laid out for `width` columns where it wraps by hand (the
/// recap keeps to the value column).
pub fn run_lines(
    item: &DigestItem,
    events: &[Event],
    summary: Option<&Summary>,
    names: &Names,
    width: usize,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let r = &item.run;
    let mut lines = vec![Line::from(Span::raw(format!(" {}", r.title)).bold())];
    let mut state = vec![
        Span::raw(" "),
        Span::styled(state_label(r.state, r.confidence), state_style(r.state)),
        Span::raw(format!("  {} · via {}", fmt_age(r.state_at, now), r.source)).dim(),
    ];
    if let Some(j) = &item.joined_by {
        state.push(Span::raw(format!("  ● {j}")).dim());
    }
    lines.push(Line::from(state));
    lines.push(Line::raw(""));
    lines.push(fact(
        "why",
        format!(
            "{} · started {} ({} ago)",
            cause_words(&r.cause),
            fmt_clock(r.started_at, now),
            fmt_age(r.started_at, now)
        ),
    ));
    let mut place = names.workspace(&r.workspace);
    if let Some(b) = &r.branch {
        place.push_str(&format!(" · {b}"));
    }
    if let Some(x) = &r.ref_ {
        place.push_str(&format!(" · {x}"));
    }
    lines.push(fact("where", place));
    lines.push(fact("runs", shown_argv(&r.argv)));
    if !r.tags.is_empty() {
        lines.push(fact("tags", r.tags.join(", ")));
    }
    if let Some(e) = r.ended_at {
        let code = r.exit_code.map(|c| format!(" · exit {c}")).unwrap_or_default();
        lines.push(fact(
            "ended",
            format!(
                "{} · after {}{code}",
                fmt_clock(e, now),
                fmt_duration(e.duration_since(r.started_at))
            ),
        ));
    }
    if let Some(d) = &r.detail {
        lines.push(fact("doing", d.clone()));
    }
    if let Some(l) = &r.last_line {
        lines.push(fact("last", l.clone()));
    }
    // The latest thing that went wrong around the run without ending it. A
    // fact rather than a line in `lately`, which the newest events push out:
    // output lost while Relay was away is worth knowing days later.
    if let Some(t) = events
        .iter()
        .rev()
        .find(|e| e.kind == rl_proto::types::event_kind::RUN_TROUBLE)
    {
        for (i, w) in wrap(
            &format!("{} · {}", t.summary, fmt_age(t.at, now)),
            width.saturating_sub(VALUE_COL + 1).max(20),
        )
        .into_iter()
        .enumerate()
        {
            lines.push(fact(if i == 0 { "trouble" } else { "" }, w));
        }
    }
    // What the run raised itself: the reason a probe exists, and what a
    // reaction would have heard.
    if !item.emitted.is_empty() {
        for (i, t) in wrap(&signals(&item.emitted), width.saturating_sub(VALUE_COL + 1).max(20))
            .into_iter()
            .enumerate()
        {
            lines.push(fact(if i == 0 { "signals" } else { "" }, t));
        }
    }
    if let Some(s) = summary {
        lines.push(Line::raw(""));
        lines.extend(said("recap", s, width, now));
    } else if !matches!(r.cause, rl_proto::types::Cause::Assistant(_)) {
        lines.push(Line::raw(""));
        lines.push(Line::from(
            Span::raw(format!(
                " {:<8} /summary recaps it · /summary deep reads its logs · /summary 2h the last 2h",
                "recap"
            ))
            .dim(),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::raw(" lately").dim()));
    if events.is_empty() {
        lines.push(Line::from(Span::raw("   nothing yet").dim()));
    }
    for e in events.iter().rev().take(EVENTS) {
        lines.push(Line::from(vec![
            Span::raw(format!(" {}  ", fmt_clock(e.at, now))).dim(),
            Span::raw(format!("{:<14} ", e.kind.trim_start_matches("run."))),
            Span::raw(e.summary.clone()),
        ]));
    }
    lines
}

/// How many of a job's runs the pane lists, and how many marks it draws.
const JOB_RUNS: usize = 10;
const JOB_MARKS: usize = 40;

/// A job: the rule behind it when known, then how it has been going.
pub fn job_lines(
    j: &Job,
    rule: Option<&Rule>,
    summary: Option<&Summary>,
    names: &Names,
    width: usize,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let mut lines = match rule {
        Some(r) => {
            let mut l = rules::lines(r, names, now, Some(&j.last().run.title));
            l.push(Line::raw(""));
            l
        }
        None => {
            let mut l = vec![Line::from(vec![
                Span::raw(format!(" {}", j.name)).bold(),
                Span::raw(format!("  {} · not in any file now", j.kind)).dim(),
            ])];
            l.push(Line::raw(""));
            if let Some(w) = &j.when {
                l.push(fact("when", w.clone()));
                l.push(Line::raw(""));
            }
            l
        }
    };
    let (ok, failed, stopped) = (j.ok(), j.failed(), j.stopped());
    let mut tally = vec![format!(
        "{} run{}",
        j.runs.len(),
        if j.runs.len() == 1 { "" } else { "s" }
    )];
    if ok > 0 {
        tally.push(format!("{ok} ok"));
    }
    if failed > 0 {
        tally.push(format!("{failed} failed"));
    }
    if stopped > 0 {
        tally.push(format!("{stopped} stopped"));
    }
    lines.push(Line::from(vec![
        Span::raw(" lately   ").dim(),
        Span::raw(tally.join(" · ")),
    ]));
    // What the last firing raised, wrapping in the value column.
    if !j.last().emitted.is_empty() {
        let value = width.saturating_sub(VALUE_COL + 1).max(20);
        for (i, t) in wrap(&signals(&j.last().emitted), value).into_iter().enumerate() {
            lines.push(fact(if i == 0 { "signals" } else { "" }, t));
        }
    }
    let marks = j.marks(JOB_MARKS);
    let spans: Vec<Span> = marks
        .chars()
        .map(|c| match c {
            '×' => Span::raw("×").red().bold(),
            '○' => Span::raw("○").dim(),
            _ => Span::raw("·"),
        })
        .collect();
    let mut row = vec![Span::raw("          ")];
    row.extend(spans);
    lines.push(Line::from(row));
    lines.extend(summary_section(summary, width, now));
    lines.push(Line::raw(""));
    for i in j.runs.iter().take(JOB_RUNS) {
        lines.push(Line::from(vec![
            Span::raw(format!(" {}  ", fmt_clock(i.at, now))).dim(),
            Span::styled(
                format!("{:<8}", state_label(i.run.state, i.run.confidence)),
                state_style(i.run.state),
            ),
            Span::raw(format!(" {}", i.summary)),
        ]));
    }
    if j.runs.len() > JOB_RUNS {
        lines.push(Line::from(
            Span::raw(format!("   … {} more", j.runs.len() - JOB_RUNS)).dim(),
        ));
    }
    lines
}

/// A rules section: a leading blank line, the " rules" label, and the
/// schedule/reaction chips (or the empty-state line), computed via
/// `super::rows::chips` for one folder, or the global ones (`None`).
fn rules_section(rules: &[Rule], scope: Option<&std::path::Path>, now: Timestamp) -> Vec<Line<'static>> {
    let chips = super::rows::chips(rules, scope, now);
    let (sched, react): (Vec<_>, Vec<_>) = chips.iter().partition(|c| c.kind == rl_proto::rule::RuleKind::Schedule);
    let mut lines = vec![Line::raw(""), Line::from(Span::raw(" rules").dim())];
    if chips.is_empty() {
        lines.push(Line::from(
            Span::raw("   none · /new adds a schedule or reaction").dim(),
        ));
    } else {
        for (label, group) in [("schedule", &sched), ("reaction", &react)] {
            for c in group {
                lines.push(Line::from(vec![
                    Span::raw(format!("   {:<9} ", label)).dim(),
                    Span::raw(c.text()),
                ]));
            }
        }
    }
    lines
}

/// How some runs stand — the buckets in the order attention wants them —
/// and when the last of them moved.
fn runs_facts(mine: &[&DigestItem], now: Timestamp) -> Vec<Line<'static>> {
    let count = |f: &dyn Fn(RunState) -> bool| mine.iter().filter(|i| f(i.run.state)).count();
    let needs = count(&|s| matches!(s, RunState::NeedsInput | RunState::Blocked | RunState::Failed));
    let going = count(&|s| {
        matches!(
            s,
            RunState::Starting | RunState::Working | RunState::Idle | RunState::Stale
        )
    });
    let done = count(&|s| matches!(s, RunState::Finished | RunState::Stopped));
    let tally: Vec<String> = [("needs you", needs), ("going", going), ("done", done)]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(w, n)| format!("{n} {w}"))
        .collect();
    let n = mine.len();
    let word = if n == 1 { "run" } else { "runs" };
    let runs = if n == 0 {
        "no runs yet".to_string()
    } else if tally.is_empty() {
        format!("{n} {word}")
    } else {
        format!("{n} {word} · {}", tally.join(" · "))
    };
    let mut lines = vec![fact("runs", runs)];
    if let Some(last) = mine.iter().map(|i| i.at).max() {
        lines.push(fact("last", format!("{} ago", fmt_age(last, now))));
    }
    lines
}

/// A project: the folder itself, how its runs stand, and the rules that live
/// there. `items` is the whole digest; this picks the ones in the folder.
#[allow(clippy::too_many_arguments)]
pub fn project_lines(
    name: &str,
    place: &str,
    workspace: &std::path::Path,
    items: &[DigestItem],
    rules: &[Rule],
    broken: &[&NotLoaded],
    summary: Option<&Summary>,
    width: usize,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let mine: Vec<&DigestItem> = items.iter().filter(|i| i.run.workspace == workspace).collect();
    let mut lines = vec![Line::from(Span::raw(format!(" {name}")).bold())];
    lines.push(Line::from(Span::raw(format!(" {place}")).dim()));
    lines.push(Line::raw(""));
    // A file that did not load comes first: until it is fixed, what is
    // below is not what the file says.
    lines.extend(not_loaded_lines(broken, None));
    lines.extend(runs_facts(&mine, now));
    lines.extend(summary_section(summary, width, now));

    // The rules that live here, told the same way the header's chips are.
    lines.extend(rules_section(rules, Some(workspace), now));
    lines.push(Line::raw(""));
    let edit = if broken.is_empty() {
        "/edit opens its schedules"
    } else {
        "/edit opens what broke, at the line"
    };
    lines.push(Line::from(
        Span::raw(format!(" ^g shows its wiring · {edit} · ^r renames")).dim(),
    ));
    lines
}

/// The catch-up's top, in the pane: what was said of everything over the
/// stretch last asked of it — the top whole when it is cut, or another
/// stretch's (`/summary 2d` on it).
pub fn top_lines(summary: Option<&Summary>, width: usize, now: Timestamp) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::raw(" everything").bold())];
    lines.push(Line::from(Span::raw(" what happened, over every project").dim()));
    lines.extend(summary_section(summary, width, now));
    lines
}

/// The `everywhere` group, which is not a project: how the global rules'
/// runs outside every project stand, and the global rules themselves.
/// `mine` are those runs; `broken`, what of the global files did not load.
pub fn everywhere_lines(
    mine: &[&DigestItem],
    rules: &[Rule],
    broken: &[&NotLoaded],
    summary: Option<&Summary>,
    width: usize,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::raw(" everywhere").bold())];
    lines.push(Line::from(
        Span::raw(" what the global rules ran outside every project").dim(),
    ));
    lines.push(Line::raw(""));
    lines.extend(not_loaded_lines(broken, None));
    lines.extend(runs_facts(mine, now));
    lines.extend(summary_section(summary, width, now));
    lines.extend(rules_section(rules, None, now));
    lines.push(Line::raw(""));
    let edit = if broken.is_empty() {
        "/edit opens their schedules"
    } else {
        "/edit opens what broke, at the line"
    };
    lines.push(Line::from(Span::raw(format!(" ^g shows their wiring · {edit}")).dim()));
    lines
}

/// What did not load, two lines each: where in red — the file, its line and
/// column, the rule when it was one — then why, under it. With `names`, each says
/// whose file it is too (`global`, or the project as the list names it), for
/// a list over every scope. Nothing when all loaded; else a blank line after.
pub fn not_loaded_lines(broken: &[&NotLoaded], names: Option<&Names>) -> Vec<Line<'static>> {
    if broken.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![Line::from(Span::styled(" not loaded", theme::bad()))];
    for n in broken {
        let mut place = match names {
            Some(names) => format!("{} · {}", rules::owner(&n.scope, names), n.at()),
            None => n.at(),
        };
        if let Some(r) = &n.rule {
            place.push_str(&format!(" · {r}"));
        }
        lines.push(Line::from(Span::styled(format!("   {place}"), theme::bad())));
        lines.push(Line::from(Span::raw(format!("     {}", n.message))));
    }
    lines.push(Line::raw(""));
    lines
}

/// What can be done about a question, in the order the pane lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// An option, by its place among the options.
    Option(usize),
    /// A question taking several: answer with the options picked so far.
    Send,
    /// See the run that asked, in its terminal.
    Attach(String),
    /// Not this.
    Dismiss,
}

/// The choices a question offers: its options, answering with what is picked
/// where it takes several, the run that asked when there is one, and not this.
pub fn choices(d: &Decision, asker: Option<&Run>) -> Vec<Choice> {
    let mut v: Vec<Choice> = (0..d.options.len()).map(Choice::Option).collect();
    if d.multi {
        v.push(Choice::Send);
    }
    if let Some(r) = asker {
        v.push(Choice::Attach(r.id.clone()));
    }
    v.push(Choice::Dismiss);
    v
}

/// A question in the pane, ready to draw: every line fits the width it was
/// laid out for, so line `i` is row `i`, and `hits[i]` is the choice on it.
pub struct QuestionPane {
    pub lines: Vec<Line<'static>>,
    pub hits: Vec<Option<usize>>,
}

impl QuestionPane {
    fn push(&mut self, l: Line<'static>, hit: Option<usize>) {
        self.lines.push(l);
        self.hits.push(hit);
    }
}

/// Words laid out in lines of at most `width` characters, breaking at spaces
/// where it can and mid-word where it must.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in text.split(' ') {
        let n = cur.chars().count();
        if n > 0 && n + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut cur));
        } else if n > 0 {
            cur.push(' ');
        }
        cur.push_str(word);
        while cur.chars().count() > width {
            out.push(cur.chars().take(width).collect());
            cur = cur.chars().skip(width).collect();
        }
    }
    out.push(cur);
    out
}

/// A question: what it asks, who asked and what was said of that run (its
/// summary, as in the run's own pane), what it says, and what can be done
/// about it — each choice on a row of its own, `at` the highlighted one while
/// it is being answered. `picked` are the options marked so far where it
/// takes several. Laid out for `width` columns.
pub fn question_lines(
    d: &Decision,
    asker: Option<&Run>,
    summary: Option<&Summary>,
    picked: &[usize],
    at: Option<usize>,
    width: usize,
    now: Timestamp,
) -> QuestionPane {
    let mut p = QuestionPane {
        lines: Vec::new(),
        hits: Vec::new(),
    };
    let text = width.saturating_sub(1);
    for (i, t) in wrap(&d.title, text.saturating_sub(2)).into_iter().enumerate() {
        let lead = if i == 0 { "? " } else { "  " };
        p.push(
            Line::from(vec![
                Span::raw(" "),
                Span::raw(lead).yellow().bold(),
                Span::raw(t).bold(),
            ]),
            None,
        );
    }
    let mut who = format!("asked by {} · {} ago", asked_by(d, asker), fmt_age(d.created_at, now));
    if let Some(e) = d.expires_at {
        if e > now {
            who.push_str(&format!(" · {} left", fmt_duration(e.duration_since(now))));
        } else {
            who.push_str(" · due");
        }
    }
    for t in wrap(&who, text) {
        p.push(Line::from(Span::raw(format!(" {t}")).dim()), None);
    }
    if d.run.is_some() {
        for l in summary_section(summary, width, now) {
            p.push(l, None);
        }
    }
    if let Some(s) = &d.summary {
        p.push(Line::raw(""), None);
        for l in s.lines() {
            for t in wrap(l, text) {
                p.push(Line::raw(format!(" {t}")), None);
            }
        }
    }
    p.push(Line::raw(""), None);
    for (ci, c) in choices(d, asker).iter().enumerate() {
        // The number (or a sign), a name, what it says, and a mark: picked, or
        // taken on its own at the deadline.
        let (key, name, what, mark, action) = match c {
            Choice::Option(i) => {
                let o = &d.options[*i];
                let key = if *i < MAX_KEYS {
                    (i + 1).to_string()
                } else {
                    String::new()
                };
                let mark = if picked.contains(i) {
                    "  ◆"
                } else if o.default {
                    "  *"
                } else {
                    ""
                };
                (key, o.key.clone(), o.text().to_string(), mark, o.action().label())
            }
            Choice::Send => (
                "↵".into(),
                "answer".into(),
                format!("with the {} picked", picked.len()),
                "",
                String::new(),
            ),
            Choice::Attach(_) => {
                let (title, live) = asker.map_or((String::new(), false), |r| (r.title.clone(), !r.state.is_terminal()));
                let what = if live { "still going" } else { "what it printed" };
                (
                    "↗".into(),
                    "attach".into(),
                    format!("{title} · {what}"),
                    "",
                    String::new(),
                )
            }
            Choice::Dismiss => ("✕".into(), "dismiss".into(), "not this".into(), "", String::new()),
        };
        let lit = at == Some(ci);
        let caret = if lit { "❯" } else { " " };
        let row = fit(&format!("{caret} {key:<2} {name:<10} {what}{mark}"), text);
        let row = if lit {
            Span::styled(row, super::theme::selected())
        } else {
            Span::raw(row)
        };
        p.push(Line::from(vec![Span::raw(" "), row]), Some(ci));
        if !action.is_empty() {
            let row = fit(&format!("     {:<10} → {action}", ""), text);
            p.push(Line::from(vec![Span::raw(" "), Span::raw(row).dim()]), Some(ci));
        }
    }
    p.push(Line::raw(""), None);
    let n = d.options.len().min(MAX_KEYS);
    let keys = if n == 1 { "1".to_string() } else { format!("1-{n}") };
    let hint = match (at.is_some(), d.multi) {
        (true, true) => format!("↑↓ choose · space picks · enter takes it · {keys} picks · esc back"),
        (true, false) => format!("↑↓ choose · enter takes it · {keys} answers · esc back"),
        (false, _) => "enter to choose, or click · `1 and a note` on the line answers with it".to_string(),
    };
    for t in wrap(&hint, text) {
        p.push(Line::from(Span::raw(format!(" {t}")).dim()), None);
    }
    if d.options.iter().any(|o| o.default) {
        p.push(Line::from(Span::raw(" * taken on its own if it expires").dim()), None);
    }
    p
}

/// A line that answers a question: the option first — its number or its key,
/// several joined by commas where the question takes several — and whatever
/// follows as the note that goes with the answer. `None` when the first word
/// names no option, so the line means what it would anywhere else.
pub fn answer_of(d: &Decision, line: &str) -> Option<(Vec<String>, Option<String>)> {
    let line = line.trim();
    let (head, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let mut keys = Vec::new();
    for part in head.split(',').filter(|p| !p.is_empty()) {
        let opt = match part.parse::<usize>() {
            Ok(i) if (1..=d.options.len().min(MAX_KEYS)).contains(&i) => &d.options[i - 1],
            Ok(_) => return None,
            Err(_) => d.options.iter().find(|o| o.key.eq_ignore_ascii_case(part))?,
        };
        if !keys.contains(&opt.key) {
            keys.push(opt.key.clone());
        }
    }
    if keys.is_empty() || (keys.len() > 1 && !d.multi) {
        return None;
    }
    let note = rest.trim();
    Some((keys, (!note.is_empty()).then(|| note.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::action::Task;
    use rl_proto::decision::{DecisionOption, DecisionStatus};
    use rl_proto::types::Signal;
    use rl_proto::types::{Cause, Confidence, RunKind};

    fn text(lines: &[Line]) -> String {
        lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn a_run_reads_as_words() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let t: Timestamp = "2026-09-10T11:00:00Z".parse().unwrap();
        let mut r = Run::new(
            "k7x2m9qa",
            RunKind::Command,
            "nightly tests",
            vec!["./test.sh".into()],
            "/home/f/relay",
            "/home/f/relay",
            t,
        );
        r.state = RunState::Failed;
        r.confidence = Confidence::Exact;
        r.state_at = t;
        r.ended_at = Some(t);
        r.exit_code = Some(1);
        r.cause = Cause::Schedule("nightly-tests".into());
        r.tags = vec!["nightly".into()];
        let item = DigestItem {
            run: r,
            summary: "exit 1".into(),
            at: t,
            unseen: true,
            joined_by: None,
            cwd: None,
            recap: None,
            emitted: vec![
                Signal {
                    kind: "flint.product.new-feedback".into(),
                    count: 5,
                    said: "5 feedback rows to process".into(),
                    at: t,
                },
                Signal {
                    kind: "log.issue".into(),
                    count: 1,
                    said: "a note".into(),
                    at: t,
                },
            ],
        };
        let names = Names::new([("/home/f/relay".into(), "relay".to_string())]);
        let s = text(&run_lines(&item, &[], None, &names, 80, now));
        assert!(s.starts_with(" nightly tests\n failed  1h · via"), "{s}");
        assert!(s.contains(" why      schedule nightly-tests · started"), "{s}");
        assert!(s.contains(" where    relay\n"), "{s}");
        assert!(s.contains(" runs     ./test.sh\n"), "{s}");
        assert!(s.contains(" tags     nightly\n"), "{s}");
        assert!(
            s.contains(" signals  flint.product.new-feedback ×5 · log.issue\n"),
            "what the run raised, once each kind: {s}"
        );
        assert!(s.contains("· after 0s · exit 1"), "{s}");
        assert!(
            s.contains("\n recap    /summary recaps it · /summary deep reads its logs · /summary 2h the last 2h\n"),
            "{s}"
        );
        assert!(s.ends_with(" lately\n   nothing yet"), "{s}");
        assert!(!s.contains("k7x2m9qa"), "no id on screen: {s}");
        assert!(!s.contains(" trouble"), "nothing went wrong, so nothing is said: {s}");

        // Trouble keeps as a fact: `lately` shows the newest events and would
        // push it out, but "output was lost" is worth knowing days later.
        let quiet = Event {
            id: 1,
            at: "2026-09-10T10:00:00Z".parse().unwrap(),
            workspace: "/home/f/relay".into(),
            run: Some("k7x2m9qa".into()),
            group_key: "r:k7x2m9qa".into(),
            kind: "run.started".into(),
            summary: "started `./test.sh`".into(),
            data: None,
        };
        let older = Event {
            id: 2,
            kind: rl_proto::types::event_kind::RUN_TROUBLE.into(),
            summary: "18 bytes of output were lost while Relay was not running".into(),
            ..quiet.clone()
        };
        let newer = Event {
            id: 3,
            at: "2026-09-10T11:00:00Z".parse().unwrap(),
            kind: rl_proto::types::event_kind::RUN_TROUBLE.into(),
            summary: "the session holder went without reporting an exit (it closed the connection)".into(),
            ..quiet.clone()
        };
        let s = text(&run_lines(&item, &[quiet, older, newer], None, &names, 100, now));
        assert!(
            s.contains(" trouble  the session holder went without reporting an exit"),
            "the latest one, as a fact: {s}"
        );
        assert!(!s.contains(" trouble  18 bytes"), "only the latest: {s}");

        // Summarised: the assistant's words, marked as its, under the facts.
        let summary = Summary {
            run: "k7x2m9qa".into(),
            agent: "claude".into(),
            by: "a1".into(),
            at: "2026-09-10T11:58:00Z".parse().unwrap(),
            text: "The parser test failed on an empty file.\nNothing was changed.".into(),
            since: None,
            source: "summarised".into(),
            reach: "o:3".into(),
            stale: false,
            target: String::new(),
            deep: false,
            from: None,
            calls: 0,
            runs: 1,
        };
        let s = text(&run_lines(&item, &[], Some(&summary), &names, 80, now));
        // Narrow, the recap wraps under its own column.
        let narrow = text(&run_lines(&item, &[], Some(&summary), &names, 40, now));
        assert!(
            narrow.contains("\n          The parser test failed on an\n          empty file.\n"),
            "{narrow}"
        );
        assert!(
            s.contains("\n recap    by claude · 2m ago\n          The parser test failed on an empty file.\n          Nothing was changed.\n\n lately"),
            "{s}"
        );
        assert!(!s.contains("/summary recaps it"), "{s}");
        // Claude's own, over a stretch, and the run gone on since.
        let own = Summary {
            by: String::new(),
            since: Some("2h".into()),
            source: "claude recap".into(),
            stale: true,
            target: String::new(),
            deep: false,
            from: None,
            calls: 0,
            ..summary.clone()
        };
        assert_eq!(
            recap_label(&own, now),
            "claude's own · last 2h · 2m ago · moved on since"
        );
        let turns = Summary {
            source: "codex turns".into(),
            ..own.clone()
        };
        assert_eq!(
            recap_label(&turns, now),
            "codex's turns · last 2h · 2m ago · moved on since"
        );
        let last = Summary {
            source: "codex turn".into(),
            since: None,
            stale: false,
            target: String::new(),
            deep: false,
            from: None,
            calls: 0,
            ..own
        };
        assert_eq!(recap_label(&last, now), "codex's last turn · 2m ago");
    }

    #[test]
    fn a_summary_says_what_it_is_of() {
        let now = Timestamp::now();
        let s = Summary {
            run: String::new(),
            agent: "fakeai".into(),
            by: "abc".into(),
            at: now,
            text: String::new(),
            since: None,
            source: "summarised".into(),
            reach: String::new(),
            stale: false,
            target: "all".into(),
            deep: true,
            from: None,
            calls: 2,
            runs: 0,
        };
        assert_eq!(
            summary_label(&s, now),
            "summary of everything · deep · by fakeai · 2 calls"
        );
        let job = Summary {
            target: "j:schedule:nightly@everywhere".into(),
            deep: false,
            calls: 1,
            stale: true,
            ..s.clone()
        };
        assert_eq!(
            summary_label(&job, now),
            "summary of the schedule nightly · by fakeai · 1 call · moved on since"
        );
        // A folder may have an `@` in it; a job's name has none.
        let there = Summary {
            target: "j:reaction:fix@/w/me@work".into(),
            ..job.clone()
        };
        assert!(
            summary_label(&there, now).starts_with("summary of the reaction fix · "),
            "{}",
            summary_label(&there, now)
        );
        let run = Summary {
            target: "r:abc".into(),
            ..s
        };
        assert_eq!(summary_label(&run, now), "deep · by fakeai · 2 calls · 0s ago");
    }

    #[test]
    fn a_recap_says_how_deep_who_from_when_and_how_old() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let from: Timestamp = "2026-09-10T09:30:00Z".parse().unwrap();
        let s = Summary {
            run: String::new(),
            agent: "haiku".into(),
            by: "abc".into(),
            at: now - jiff::SignedDuration::from_mins(3),
            text: "It went.".into(),
            since: None,
            source: "summarised".into(),
            reach: String::new(),
            stale: false,
            target: "p:/w/api".into(),
            deep: true,
            from: Some(from),
            calls: 1,
            runs: 2,
        };
        let at = fmt_when(from, now);
        assert_eq!(recap_label(&s, now), format!("deep · by haiku · since {at} · 3m ago"));
        // Many calls are said; one is what `by` means.
        let many = Summary { calls: 4, ..s.clone() };
        assert_eq!(
            recap_label(&many, now),
            format!("deep · by haiku · 4 calls · since {at} · 3m ago")
        );
        let own = Summary {
            source: "claude recap".into(),
            deep: false,
            from: None,
            calls: 0,
            ..s.clone()
        };
        assert_eq!(recap_label(&own, now), "claude's own · 3m ago");

        // A project's pane and a job's show the one kept, under the facts.
        let lines = text(&project_lines(
            "api",
            "/w/api",
            Path::new("/w/api"),
            &[],
            &[],
            &[],
            Some(&s),
            80,
            now,
        ));
        assert!(
            lines.contains(&format!(
                "\n summary  of the project /w/api · deep · by haiku · since {at} · 3m ago\n          It went.\n"
            )),
            "what it is of, then where it came from: {lines}"
        );
        let none = text(&project_lines(
            "api",
            "/w/api",
            Path::new("/w/api"),
            &[],
            &[],
            &[],
            None,
            80,
            now,
        ));
        assert!(none.contains("/summary says what happened"), "{none}");
        let everything = Summary {
            target: "all".into(),
            ..s.clone()
        };
        let all = text(&everywhere_lines(&[], &[], &[], Some(&everything), 80, now));
        assert!(all.contains(" summary  of everything · deep · by haiku"), "{all}");
    }

    #[test]
    fn the_hooks_settings_are_not_shown() {
        let argv: Vec<String> = ["claude", "--settings", "/h/hooks/claude.json", "-p", "why?"]
            .map(String::from)
            .to_vec();
        assert_eq!(shown_argv(&argv), "claude -p why?");
        assert_eq!(shown_argv(&["make".to_string(), "test".to_string()]), "make test");
    }

    #[test]
    fn a_question_lists_keys_and_actions() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let t: Timestamp = "2026-09-10T05:00:00Z".parse().unwrap();
        let d = Decision {
            id: "kfmpg3sw".into(),
            workspace: "/w".into(),
            run: None,
            chain: "kfmpg3sw".into(),
            group: Some("reaction:tests-failed".into()),
            title: "nightly tests failed".into(),
            summary: Some("test parser ... FAILED".into()),
            options: vec![
                DecisionOption {
                    key: "look".into(),
                    label: "Have Claude look at it".into(),
                    run: Some(Task::Agent {
                        agent: "claude".into(),
                        prompt: Some("why?".into()),
                        prompt_file: None,
                        args: vec![],
                    }),
                    ..Default::default()
                },
                DecisionOption {
                    key: "ignore".into(),
                    default: true,
                    ..Default::default()
                },
            ],
            multi: false,
            status: DecisionStatus::Open,
            created_at: t,
            updated_at: t,
            expires_at: Some("2026-09-10T13:00:00Z".parse().unwrap()),
            resolved_at: None,
            chosen: vec![],
            outcome_runs: vec![],
            note: None,
        };
        let shown = |p: &QuestionPane| {
            p.lines
                .iter()
                .map(|l| l.to_string().trim_end().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let p = question_lines(&d, None, None, &[], None, 80, now);
        let s = shown(&p);
        assert!(
            s.starts_with(" ? nightly tests failed\n asked by rule tests-failed · 7h ago · 1h00m left"),
            "{s}"
        );
        assert!(s.contains("\n test parser ... FAILED\n"), "{s}");
        assert!(s.contains("\n   1  look       Have Claude look at it\n"), "{s}");
        assert!(s.contains("→ claude · why?"), "{s}");
        assert!(s.contains("\n   2  ignore     ignore  *\n"), "{s}");
        assert!(s.contains("\n   ✕  dismiss    not this\n"), "{s}");
        assert!(!s.contains("attach"), "nothing asked it, so nothing to attach to: {s}");
        assert!(s.contains("enter to choose, or click"), "{s}");
        assert!(!s.contains("kfmpg3sw"), "no id on screen: {s}");
        // Line `i` is row `i`: each choice's rows say which it is.
        let row = |needle: &str| p.lines.iter().position(|l| l.to_string().contains(needle)).unwrap();
        assert_eq!(p.hits[row("Have Claude")], Some(0));
        assert_eq!(p.hits[row("→ claude")], Some(0), "its action line is the same choice");
        assert_eq!(p.hits[row("dismiss")], Some(2));
        assert_eq!(p.hits[0], None);

        // Being answered: a caret on the highlighted choice, and the keys that move it.
        let s = shown(&question_lines(&d, None, None, &[1], Some(1), 80, now));
        assert!(s.contains("\n ❯ 2  ignore     ignore  ◆\n"), "{s}");
        assert!(s.contains("↑↓ choose · enter takes it · 1-2 answers · esc back"), "{s}");

        // A run that asked can be attached to; a question taking several can be sent.
        let asker = Run::new("r1", RunKind::Command, "nightly", vec![], "/w", "/w", t);
        let mut multi = d.clone();
        multi.multi = true;
        assert_eq!(
            choices(&multi, Some(&asker)),
            [
                Choice::Option(0),
                Choice::Option(1),
                Choice::Send,
                Choice::Attach("r1".into()),
                Choice::Dismiss
            ]
        );
        let s = shown(&question_lines(&multi, Some(&asker), None, &[0], None, 80, now));
        assert!(s.contains("↵  answer     with the 1 picked"), "{s}");
        assert!(s.contains("↗  attach     nightly · still going"), "{s}");

        // Narrow, a long summary wraps onto rows of its own.
        let mut long = d.clone();
        long.summary = Some("one two three four five six".into());
        let s = shown(&question_lines(&long, None, None, &[], None, 12, now));
        assert!(s.contains("\n one two\n three four\n five six\n"), "{s}");

        // The line answers by number or key, the rest the note.
        let some = |k: &[&str], n: Option<&str>| Some((k.iter().map(|x| x.to_string()).collect(), n.map(String::from)));
        assert_eq!(answer_of(&d, "1"), some(&["look"], None));
        assert_eq!(answer_of(&d, "2  not tonight "), some(&["ignore"], Some("not tonight")));
        assert_eq!(
            answer_of(&d, "LOOK only the parser"),
            some(&["look"], Some("only the parser"))
        );
        assert_eq!(answer_of(&d, "3"), None, "no third option");
        assert_eq!(answer_of(&d, "make test"), None, "not an option: the line is a line");
        assert_eq!(answer_of(&d, "1,2"), None, "one option only");
        let mut multi = d.clone();
        multi.multi = true;
        assert_eq!(
            answer_of(&multi, "1,ignore both"),
            some(&["look", "ignore"], Some("both"))
        );
    }
}
