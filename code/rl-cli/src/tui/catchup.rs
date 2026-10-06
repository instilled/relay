//! The catch-up, drawn: what happened while you were away, in the list's
//! place. The header says since when; under it the top, a bullet a line;
//! then a line per project — folded to one when nothing in it is new — and
//! under each a line per row. What it holds is `crate::catchup`'s.

use super::{icon_span, pane, rows, theme};
use crate::catchup::{CatchUp, Project, row_line};
use jiff::Timestamp;
use ratatui::text::{Line, Span};
use rl_core::time::{fmt_duration, fmt_when};
use rows::{Row, fit};

/// Where the top's bullets and the projects' lines start.
const INDENT: &str = "  ";
/// Where a row's icon starts: under its project's name.
const ROW_INDENT: &str = "   ";

/// The header's words: since when, and how long that is.
pub fn headline(since: Timestamp, now: Timestamp) -> String {
    format!(
        "while you were away · since {} ({})",
        fmt_when(since, now),
        fmt_duration(now.duration_since(since))
    )
}

/// The top: a bullet a line, cut to the width. A stretch with nothing in it
/// says so.
pub fn top(cu: &CatchUp, w: usize) -> Vec<Line<'static>> {
    let bullets = cu.bullets();
    if bullets.is_empty() {
        return vec![Line::styled(format!("{INDENT}• nothing happened"), theme::dim())];
    }
    bullets
        .iter()
        .map(|b| Line::raw(fit(&format!("{INDENT}• {b}"), w).trim_end().to_string()))
        .collect()
}

/// The projects and their rows, a line each, and in step with them each
/// line's row key, for the cursor and the mouse.
pub fn lines(cu: &CatchUp, w: usize, now: Timestamp) -> (Vec<Line<'static>>, Vec<Option<String>>) {
    let mut lines = Vec::new();
    let mut keys = Vec::new();
    for p in &cu.projects {
        lines.push(project_line(p, w));
        keys.push(Row::Group(p.head.clone()).key());
        for r in &p.rows {
            lines.push(row(r, w, now));
            keys.push(r.key());
        }
    }
    (lines, keys)
}

/// A project's line: open, or folded when nothing in it is new; its name at
/// the left and how the stretch went at the right.
fn project_line(p: &Project, w: usize) -> Line<'static> {
    let lead = if p.rows.is_empty() { "▸" } else { "▾" };
    let right = tally(p);
    let lead = format!("{INDENT}{lead} ");
    let room = w.saturating_sub(lead.chars().count() + right.chars().count() + 1);
    let tone = if p.need > 0 { theme::needs() } else { theme::dim() };
    Line::from(vec![
        Span::raw(lead),
        Span::styled(fit(&p.head.name, room), theme::name()),
        Span::raw(" "),
        Span::styled(right, tone),
    ])
}

/// How a project's stretch went, in a few words: what needs you, what ran,
/// what failed; `nothing new` when it is folded.
fn tally(p: &Project) -> String {
    if p.rows.is_empty() {
        return "nothing new".into();
    }
    let mut parts = Vec::new();
    if p.need > 0 {
        parts.push(format!("{} need{} you", p.need, if p.need == 1 { "s" } else { "" }));
    }
    if p.ran > 0 {
        parts.push(format!("{} ran", p.ran));
    }
    if p.failed > 0 {
        parts.push(format!("{} failed", p.failed));
    }
    if parts.is_empty() {
        parts.push(format!("{} new", p.rows.len()));
    }
    parts.join(" · ")
}

/// A row's line: its icon, what `row_line` says of it, and on a question
/// the keys that answer it.
fn row(r: &Row, w: usize, now: Timestamp) -> Line<'static> {
    let (icon, ended) = match r {
        Row::Question(_) => (rows::QUESTION, false),
        Row::Run(i) => (rows::icon(&i.run), i.run.state.is_terminal()),
        Row::Job(_) => (rows::JOB, false),
        Row::Group(h) => return Line::raw(h.name.clone()),
    };
    let right = match r {
        Row::Question(q) => match q.options.len().min(pane::MAX_KEYS) {
            0 => String::new(),
            1 => "1 answers".into(),
            n => format!("1-{n} answers"),
        },
        _ => String::new(),
    };
    let mut spans = vec![Span::raw(ROW_INDENT)];
    spans.extend(icon_span(icon, ended));
    let used = ROW_INDENT.len() + 3 + right.chars().count() + 1;
    let text = fit(&row_line(r, now), w.saturating_sub(used));
    spans.push(Span::raw(text));
    if !right.is_empty() {
        spans.push(Span::styled(format!(" {right}"), theme::dim()));
    }
    Line::from(spans)
}
