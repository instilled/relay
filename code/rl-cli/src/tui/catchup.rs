//! The catch-up, drawn: what happened while you were away, in the list's
//! place. The header says since when; under it the top, a bullet a line;
//! then a line per project — folded to one when nothing in it is new — and
//! under each a line per row. What it holds is `crate::catchup`'s.
//! Above the top, the firings that head it (`crate::catchup::heads`).

use super::flows::state_words;
use super::{icon_span, pane, rows, theme};
use crate::catchup::heads::{self, Lead, Outcomes};
use crate::catchup::{CatchUp, Project, row_line};
use jiff::Timestamp;
use ratatui::text::{Line, Span};
use rl_core::time::fmt_when;
use rl_proto::flow::Outcome;
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

/// The leads, above the top: each one's title — `↻`, the flow's name bold, when it fired and
/// where it stands dim — then what it came to, wrapped, blank lines left out; past
/// `heads::LINES` lines it is cut, its last saying where the rest is; a blank line after each.
/// In step with the lines, the key each belongs to.
pub fn leads(leads: &[Lead], outcomes: &Outcomes, w: usize, now: Timestamp) -> (Vec<Line<'static>>, Vec<String>) {
    let mut lines = Vec::new();
    let mut keys = Vec::new();
    for l in leads {
        let when = format!(" · {} · {}", fmt_when(l.firing.started_at, now), state_words(l.firing));
        lines.push(Line::from(vec![
            Span::raw(INDENT),
            Span::styled(
                format!("{} ", rows::FLOW),
                ratatui::style::Style::default().fg(theme::LIVE),
            ),
            Span::styled(l.flow.to_string(), theme::name()),
            Span::styled(when, theme::dim()),
        ]));
        let room = w.saturating_sub(INDENT.len() + 2).max(1);
        let mut body: Vec<String> = heads::body(outcomes.of(&l.firing.id))
            .iter()
            .flat_map(|b| pane::wrap(b, room))
            .collect();
        let cut = body.len() > heads::LINES;
        if cut {
            body.truncate(heads::LINES - 1);
        }
        lines.extend(body.into_iter().map(|b| Line::raw(format!("{INDENT}  {b}"))));
        if cut {
            lines.push(Line::styled(
                format!("{INDENT}  … ^o on it shows the rest"),
                theme::dim(),
            ));
        }
        lines.push(Line::raw(""));
        keys.resize(lines.len(), heads::key(l.flow));
    }
    (lines, keys)
}

/// What heads the catch-up in `h` rows: the leads (`leads`, in step with their keys), then
/// Relay's own `top`, keyed `top_key`. The leads take at most two thirds of the rows less the
/// top's first two lines — cut there, the last line kept says where the rest is — and the top
/// what is left of half the rows, two lines at least (`capped`).
pub fn head_block(
    leads: (Vec<Line<'static>>, Vec<String>),
    top: Vec<Line<'static>>,
    top_key: &str,
    h: usize,
) -> (Vec<Line<'static>>, Vec<String>) {
    let (mut lines, mut keys) = leads;
    let room = (h * 2 / 3).saturating_sub(top.len().min(2));
    if lines.len() > room {
        lines.truncate(room);
        keys.truncate(room);
        // A lead's title alone, its body all cut, goes too: the cut is said under the one before.
        if keys.len() >= 2 && keys[keys.len() - 1] != keys[keys.len() - 2] {
            lines.pop();
            keys.pop();
        }
        if let Some(last) = lines.last_mut() {
            *last = Line::styled(format!("{INDENT}  … ^o on it shows the rest"), theme::dim());
        }
    }
    let top = capped(top, (h / 2).saturating_sub(lines.len()).max(2));
    keys.extend(std::iter::repeat_n(top_key.to_string(), top.len()));
    lines.extend(top);
    (lines, keys)
}

/// `lines` with those whose key is `sel` lit to the width, as the list lights its selection.
pub fn lit(lines: Vec<Line<'static>>, keys: &[String], sel: Option<&str>, w: usize) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .zip(keys)
        .map(|(mut l, k)| {
            if sel == Some(k.as_str()) {
                let short = w.saturating_sub(l.width());
                l.spans.push(Span::raw(" ".repeat(short)));
                l = l.style(theme::selected());
            }
            l
        })
        .collect()
}

/// A lead in the pane: its flow, when it fired and where it stands, all of what it came to —
/// blank lines kept, links as printed (the catch-up says them by their text) — then what
/// `/summary` said of the firing.
pub fn lead_pane(
    l: &Lead,
    outcome: Option<&Outcome>,
    said: Option<&Summary>,
    w: usize,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(format!(" {}", l.flow), theme::name())),
        Line::styled(
            format!(
                " flow · fired {} · {}",
                fmt_when(l.firing.started_at, now),
                state_words(l.firing)
            ),
            theme::dim(),
        ),
        Line::raw(""),
    ];
    match outcome {
        Some(o) => {
            lines.push(Line::styled(format!(" what {} printed", o.step), theme::dim()));
            for text in o.text.lines() {
                for t in pane::wrap(text, w.saturating_sub(2).max(1)) {
                    lines.push(Line::raw(format!(" {t}")));
                }
            }
        }
        None => lines.push(Line::styled(" nothing printed yet", theme::dim())),
    }
    lines.extend(pane::summary_section(said, w, now));
    lines
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
    let (icon, cli, ended) = match r {
        Row::Question(_) => (rows::QUESTION, None, false),
        Row::Run(i) => (rows::icon(&i.run), i.run.cli.as_deref(), i.run.state.is_terminal()),
        Row::Flow(_) => (rows::FLOW, None, false),
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
    spans.extend(icon_span(icon, cli, ended));
    let used = ROW_INDENT.len() + 3 + right.chars().count() + 1;
    let text = fit(&row_line(r, now), w.saturating_sub(used));
    spans.push(Span::raw(text));
    if !right.is_empty() {
        spans.push(Span::styled(format!(" {right}"), theme::dim()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catchup::heads::{self, Lead, Outcomes};
    use rl_proto::flow::{Firing, FiringStatus, Outcome};

    #[test]
    fn a_lead_is_cut_where_its_room_ends() {
        let now: Timestamp = "2026-10-10T08:00:00Z".parse().unwrap();
        let f = Firing {
            id: "fi1".into(),
            flow: "morning-brief".into(),
            trigger: "when".into(),
            event: None,
            status: FiringStatus::Done,
            started_at: "2026-10-10T05:30:00Z".parse().unwrap(),
            ended_at: None,
            steps: vec![],
            outcome: None,
        };
        let mut o = Outcomes::default();
        let long: String = (1..=30).map(|i| format!("line {i}\n\n")).collect();
        o.put(
            &f,
            Some(Outcome {
                firing: "fi1".into(),
                step: "brief".into(),
                text: long,
            }),
        );
        let (lines, keys) = leads(
            &[Lead {
                flow: "morning-brief",
                firing: &f,
            }],
            &o,
            60,
            now,
        );
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert!(text[0].starts_with("  ↻ morning-brief · "), "{text:#?}");
        assert_eq!(text[1], "    line 1", "blank lines left out");
        assert_eq!(
            text.len(),
            1 + heads::LINES + 1,
            "the title, LINES of it, a blank: {text:#?}"
        );
        assert_eq!(text[heads::LINES], "    … ^o on it shows the rest");
        assert_eq!(keys.len(), lines.len());
        assert!(keys.iter().all(|k| k == "h:morning-brief"));

        let lit = lit(lines, &keys, Some("h:morning-brief"), 60);
        assert!(lit.iter().all(|l| l.width() == 60), "lit to the edge");
    }

    #[test]
    fn the_leads_are_cut_so_relays_top_keeps_two_lines() {
        let lead = |k: &str| {
            let lines: Vec<Line<'static>> = (0..10).map(|i| Line::raw(format!("{k} {i}"))).collect();
            (lines, vec![format!("h:{k}"); 10])
        };
        let (mut lines, mut keys) = lead("a");
        let (b, bk) = lead("b");
        lines.extend(b);
        keys.extend(bk);
        let top = || (0..5).map(|i| Line::raw(format!("top {i}"))).collect::<Vec<_>>();

        // 15 rows: the leads take 10 less 2, the top keeps two lines.
        let (got, k) = head_block((lines.clone(), keys.clone()), top(), "top", 15);
        let text: Vec<String> = got.iter().map(|l| l.to_string()).collect();
        assert_eq!(text.len(), 10, "{text:#?}");
        assert_eq!(text[7], "    … ^o on it shows the rest");
        assert_eq!(&text[8..], ["top 0", "  … ^o on the top shows the rest"]);
        assert_eq!(k.len(), 10);
        assert!(k[..8].iter().all(|k| k == "h:a") && k[8..].iter().all(|k| k == "top"));

        // Cut just past b's title: it goes with its body, a's line says the cut.
        let (got, k) = head_block((lines.clone(), keys.clone()), top(), "top", 20);
        assert_eq!(got.len(), 10 + 2, "{got:#?}");
        assert_eq!(got[9].to_string(), "    … ^o on it shows the rest");
        assert_eq!(k[9], "h:a");

        // Room for all of them: nothing cut.
        let (got, _) = head_block((lines, keys), top(), "top", 90);
        assert_eq!(got.len(), 20 + 5);
    }
}
