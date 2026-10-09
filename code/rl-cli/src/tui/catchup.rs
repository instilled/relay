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
/// top's first two lines (`squeezed`), and the top what is left of half the rows, two lines at
/// least (`capped`).
pub fn head_block(
    leads: (Vec<Line<'static>>, Vec<String>),
    top: Vec<Line<'static>>,
    top_key: &str,
    h: usize,
) -> (Vec<Line<'static>>, Vec<String>) {
    let room = (h * 2 / 3).saturating_sub(top.len().min(2));
    let (mut lines, mut keys) = squeezed(leads, room);
    let top = capped(top, (h / 2).saturating_sub(lines.len()).max(2));
    keys.extend(std::iter::repeat_n(top_key.to_string(), top.len()));
    lines.extend(top);
    (lines, keys)
}

/// The leads in `room` lines. Each keeps its title and the blank after it; the bodies get what
/// is left, the newest first, a cut one saying where the rest is. Past even that, titles alone,
/// as many as fit, the last line saying how many more: those have no line, and so no key.
fn squeezed(leads: (Vec<Line<'static>>, Vec<String>), room: usize) -> (Vec<Line<'static>>, Vec<String>) {
    let (lines, keys) = leads;
    if lines.len() <= room {
        return (lines, keys);
    }
    // Each lead: its key, its title, its body, the blank after it left out.
    let mut each: Vec<(String, Line<'static>, Vec<Line<'static>>)> = Vec::new();
    for (line, key) in lines.into_iter().zip(keys) {
        match each.last_mut() {
            Some((k, _, body)) if *k == key => body.push(line),
            _ => each.push((key, line, vec![])),
        }
    }
    for (_, _, body) in &mut each {
        if body.last().is_some_and(|l| l.width() == 0) {
            body.pop();
        }
    }
    let n = each.len();
    let (mut lines, mut keys) = (Vec::new(), Vec::new());
    if 2 * n <= room {
        let mut left = room - 2 * n;
        for (key, title, mut body) in each {
            let take = body.len().min(left);
            left -= take;
            if take < body.len() {
                body.truncate(take);
                if let Some(last) = body.last_mut() {
                    *last = Line::styled(format!("{INDENT}  … ^o on it shows the rest"), theme::dim());
                }
            }
            lines.push(title);
            lines.extend(body);
            lines.push(Line::raw(""));
            keys.resize(lines.len(), key);
        }
        return (lines, keys);
    }
    if room == 0 {
        return (lines, keys);
    }
    let shown = if n < room { n } else { room - 1 };
    for (key, title, _) in each.into_iter().take(shown) {
        lines.push(title);
        keys.push(key);
    }
    lines.push(if shown < n {
        Line::styled(format!("{INDENT}+{} more", n - shown), theme::dim())
    } else {
        Line::raw("")
    });
    keys.push(String::new());
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
        // 15 rows: the leads take 10 less 2; the newest's body gets what the titles leave.
        let (got, k) = head_block(long_leads(2), top5(), "top", 15);
        let text: Vec<String> = got.iter().map(|l| l.to_string()).collect();
        assert_eq!(text.len(), 10, "{text:#?}");
        assert!(text[0].starts_with("  ↻ brief-0 · "), "{text:#?}");
        assert_eq!(
            &text[1..6],
            [
                "    line 1",
                "    line 2",
                "    line 3",
                "    … ^o on it shows the rest",
                ""
            ]
        );
        assert!(text[6].starts_with("  ↻ brief-1 · "), "{text:#?}");
        assert_eq!(text[7], "", "its body all cut, its title stays");
        assert_eq!(&text[8..], ["top 0", "  … ^o on the top shows the rest"]);
        assert!(k[..6].iter().all(|k| k == "h:brief-0") && k[6..8].iter().all(|k| k == "h:brief-1"));
        assert!(k[8..].iter().all(|k| k == "top"));

        // Room for all of them: nothing cut.
        let (got, _) = head_block(long_leads(2), top5(), "top", 90);
        assert_eq!(got.len(), 2 * (1 + heads::LINES + 1) + 5);
    }

    /// `n` leads, each of a long outcome, drawn 60 wide, newest first.
    fn long_leads(n: usize) -> (Vec<Line<'static>>, Vec<String>) {
        let now: Timestamp = "2026-10-10T08:00:00Z".parse().unwrap();
        let names: Vec<String> = (0..n).map(|i| format!("brief-{i}")).collect();
        let firings: Vec<Firing> = names
            .iter()
            .map(|name| Firing {
                id: format!("fi-{name}"),
                flow: name.clone(),
                trigger: "when".into(),
                event: None,
                status: FiringStatus::Done,
                started_at: "2026-10-10T05:30:00Z".parse().unwrap(),
                ended_at: None,
                steps: vec![],
                outcome: None,
            })
            .collect();
        let mut o = Outcomes::default();
        let long: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        for f in &firings {
            o.put(
                f,
                Some(Outcome {
                    firing: f.id.clone(),
                    step: "brief".into(),
                    text: long.clone(),
                }),
            );
        }
        let all: Vec<Lead> = names
            .iter()
            .zip(&firings)
            .map(|(flow, firing)| Lead { flow, firing })
            .collect();
        leads(&all, &o, 60, now)
    }

    fn top5() -> Vec<Line<'static>> {
        (0..5).map(|i| Line::raw(format!("top {i}"))).collect()
    }

    #[test]
    fn every_lead_keeps_its_title_when_the_room_is_short() {
        // 15 rows: two thirds less the top's two is 8, for three leads of 10 lines each.
        let (got, k) = head_block(long_leads(3), top5(), "top", 15);
        let text: Vec<String> = got.iter().map(|l| l.to_string()).collect();
        assert_eq!(k.len(), got.len());
        for i in 0..3 {
            let title = format!("  ↻ brief-{i} · ");
            let at = text.iter().position(|t| t.starts_with(&title));
            let at = at.unwrap_or_else(|| panic!("brief-{i}'s title shows: {text:#?}"));
            assert_eq!(k[at], format!("h:brief-{i}"));
        }
        assert_eq!(
            &text[text.len() - 2..],
            ["top 0", "  … ^o on the top shows the rest"],
            "the top keeps two lines: {text:#?}"
        );
        assert_eq!(
            text[text.len() - 3],
            "",
            "a blank between the leads and the top: {text:#?}"
        );
        // Every line keyed to a lead is under that lead's title.
        for (n, key) in k.iter().enumerate() {
            if let Some(flow) = heads::flow_of(key) {
                let above = text[..=n].iter().rev().find(|t| t.starts_with("  ↻ "));
                assert!(
                    above.is_some_and(|t| t.starts_with(&format!("  ↻ {flow} · "))),
                    "line {n} is keyed to {flow} under another's title: {text:#?}"
                );
            }
        }
    }

    #[test]
    fn leads_past_even_their_titles_say_how_many_more() {
        // 12 rows: two thirds less two is 6, for ten leads.
        let (got, k) = head_block(long_leads(10), top5(), "top", 12);
        let text: Vec<String> = got.iter().map(|l| l.to_string()).collect();
        let titles = text.iter().filter(|t| t.starts_with("  ↻ ")).count();
        assert_eq!(titles, 5, "{text:#?}");
        assert_eq!(text[5], "  +5 more", "{text:#?}");
        assert_eq!(&text[6..], ["top 0", "  … ^o on the top shows the rest"]);
        let shown: std::collections::BTreeSet<&str> = k.iter().filter_map(|k| heads::flow_of(k)).collect();
        assert_eq!(shown.len(), 5, "the unshown leads have no key: {k:?}");
    }
}
