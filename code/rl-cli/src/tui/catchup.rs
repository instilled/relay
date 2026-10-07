//! The catch-up, drawn: what happened while you were away, in the list's
//! place. The header says since when; under it the top, a bullet a line;
//! then a line per project — folded to one when nothing in it is new — and
//! under each a line per row. What it holds is `crate::catchup`'s.

use super::{icon_span, pane, rows, theme};
use crate::catchup::{CatchUp, Project, row_line};
use jiff::Timestamp;
use ratatui::text::{Line, Span};
use rl_proto::types::Summary;
use rows::{Row, fit};

/// Where the top's bullets and the projects' lines start.
const INDENT: &str = "  ";
/// Where a row's icon starts: under its project's name.
const ROW_INDENT: &str = "   ";

pub use crate::catchup::headline;

/// The top from counts: a bullet a line, cut to the width. A stretch with
/// nothing in it says so.
pub fn top(cu: &CatchUp, w: usize) -> Vec<Line<'static>> {
    let lines = cu.counted();
    let tone = if cu.bullets().is_empty() {
        theme::dim()
    } else {
        ratatui::style::Style::default()
    };
    lines
        .iter()
        .map(|b| Line::styled(fit(&format!("{INDENT}{b}"), w).trim_end().to_string(), tone))
        .collect()
}

/// The top as the assistant wrote it, wrapped to the width — a bullet's
/// line goes on under its words, not under the bullet — and under it at the
/// right who wrote it and from what.
pub fn written(s: &Summary, w: usize) -> Vec<Line<'static>> {
    let room = w.saturating_sub(INDENT.len()).max(1);
    let mut out = Vec::new();
    for l in s.text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()) {
        let bullet = ["• ", "- ", "* "].iter().any(|b| l.trim_start().starts_with(b));
        let hang = if bullet { "  " } else { "" };
        for (n, part) in pane::wrap(l, room.saturating_sub(hang.len()).max(1))
            .into_iter()
            .enumerate()
        {
            let lead = if n == 0 { "" } else { hang };
            out.push(Line::raw(format!("{INDENT}{lead}{part}")));
        }
    }
    let label = crate::catchup::top_label(s);
    let pad = w.saturating_sub(label.chars().count() + 1);
    out.push(Line::styled(format!("{}{label}", " ".repeat(pad)), theme::dim()));
    out
}

/// The top cut to `h` lines: past that, its last line says where the rest
/// is — the pane, on the top.
pub fn capped(mut top: Vec<Line<'static>>, h: usize) -> Vec<Line<'static>> {
    if top.len() <= h || h == 0 {
        return top;
    }
    top.truncate(h - 1);
    top.push(Line::styled(
        format!("{INDENT}… ^o on the top shows the rest"),
        theme::dim(),
    ));
    top
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
    let right = p.tally();
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

/// A row's line: its icon, what `row_line` says of it, and on a question
/// the keys that answer it.
fn row(r: &Row, w: usize, now: Timestamp) -> Line<'static> {
    let (icon, ended) = match r {
        Row::Question(_) => (rows::QUESTION, false),
        Row::Run(i) => (rows::icon(&i.run), i.run.state.is_terminal()),
        Row::Flow(_) => (rows::JOB, false),
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
