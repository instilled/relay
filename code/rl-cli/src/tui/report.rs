//! The report as lanes: leads grouped by the work or team they belong to.
//!
//! Pure grouping and ordering, no rendering. `Lane`/`Row` borrow from the
//! `Report` (cheap; the renderer holds the report for the frame anyway).

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use rl_proto::brief::Brief;
use rl_proto::lead::{Band, Lead};
use rl_proto::methods::Report;
use rl_proto::types::Summary;

use super::rows::fit;
use super::theme;
use crate::render::fmt_clock;

pub struct Row<'a> {
    pub lead: &'a Lead,
}

pub struct Lane<'a> {
    /// The tag, or the project basename, or "my work".
    pub name: String,
    /// More than one distinct source lands in this lane.
    pub is_team: bool,
    pub needs: usize,
    pub live: usize,
    pub done: usize,
    /// Ordered needs -> live -> happened, newest first within a band.
    pub rows: Vec<Row<'a>>,
}

fn lane_name(lead: &Lead, tags: &HashMap<&str, &str>) -> String {
    if let Some(tag) = tags.get(lead.source.as_str()) {
        return (*tag).to_string();
    }
    lead.scope
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "my work".to_string())
}

pub fn lanes(report: &Report) -> Vec<Lane<'_>> {
    let tags: HashMap<&str, &str> = report
        .tags
        .iter()
        .map(|t| (t.source.as_str(), t.tag.as_str()))
        .collect();
    let mut by_name: HashMap<String, (Lane<'_>, HashSet<&str>)> = HashMap::new();
    let all = report.needs_you.iter().chain(&report.live).chain(&report.happened);
    for lead in all {
        let name = lane_name(lead, &tags);
        let (lane, sources) = by_name.entry(name.clone()).or_insert_with(|| {
            let lane = Lane {
                name,
                is_team: false,
                needs: 0,
                live: 0,
                done: 0,
                rows: Vec::new(),
            };
            (lane, HashSet::new())
        });
        match lead.band {
            Band::NeedsYou => lane.needs += 1,
            Band::Live => lane.live += 1,
            Band::Happened => lane.done += 1,
        }
        sources.insert(lead.source.as_str());
        lane.rows.push(Row { lead });
    }
    let mut out: Vec<Lane<'_>> = by_name
        .into_values()
        .map(|(mut lane, sources)| {
            lane.is_team = sources.len() > 1;
            lane.rows
                .sort_by(|a, b| (a.lead.band.rank(), b.lead.at).cmp(&(b.lead.band.rank(), a.lead.at)));
            lane
        })
        .collect();
    let newest = |l: &Lane<'_>| l.rows.iter().map(|r| r.lead.at).max();
    // Name is the final tiebreak so the order is deterministic.
    out.sort_by(|a, b| {
        (b.needs > 0)
            .cmp(&(a.needs > 0))
            .then_with(|| newest(b).cmp(&newest(a)))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// The lead column never gets narrower than this; the columns to its right give way first.
const LEAD_FLOOR: usize = 20;
const WHEN_W: usize = 12;
const PROJECT_W: usize = 14;
const SOURCE_W: usize = 12;

/// The one fact a run row leads with, by the kind of lead it is: an agent's churn, time and
/// commits; a shell's health; a flow's firing. Empty until the run's facts are frozen.
/// The kind comes from the lead's key and source, as the daemon builds them (`firing:` is a
/// flow, a `shell`-sourced `run:` is a shell run, any other run is an agent).
/// A flow's decision count is not on the lead, so a flow says only `fired`.
pub(crate) fn headline(lead: &Lead) -> String {
    let Some(f) = &lead.facts else {
        return String::new();
    };
    if lead.key.starts_with("firing:") {
        return "fired".into();
    }
    if lead.source == "shell" {
        return match f.errors {
            0 => "ok".into(),
            1 => "1 error".into(),
            n => format!("{n} errors"),
        };
    }
    let mut parts = Vec::new();
    if f.added > 0 || f.removed > 0 {
        parts.push(format!("+{}/−{}", f.added, f.removed));
    }
    if let Some(m) = f.active_secs.map(|s| s / 60).filter(|m| *m > 0) {
        parts.push(format!("{m}m"));
    }
    match f.commits {
        0 => {}
        1 => parts.push("1 commit".into()),
        n => parts.push(format!("{n} commits")),
    }
    parts.join(" · ")
}

/// The recap written for a run's lead, when there is one that still holds: a stale one
/// is as good as absent, so the row falls to its own line.
pub(crate) fn written<'a>(briefs: &'a HashMap<String, Summary>, lead: &Lead) -> Option<&'a Summary> {
    let id = lead.key.strip_prefix("run:")?;
    briefs.get(id).filter(|s| !s.stale)
}

/// The title never gives up more than this to the headline beside it.
const TITLE_MIN: usize = 12;

/// The report as lines for a body `width` columns wide: per lane a header with its
/// tally, a dim rule, then a row per lead. Rendering only; `lanes()` did the grouping.
pub fn report_lines(
    report: &Report,
    width: usize,
    now: Timestamp,
    briefs: &HashMap<String, Summary>,
) -> Vec<Line<'static>> {
    report_view(report, width, now, None, briefs).0
}

/// The lead rows in the order they are drawn, across lanes: what the cursor walks.
pub fn flat_leads(report: &Report) -> Vec<&Lead> {
    lanes(report)
        .into_iter()
        .flat_map(|l| l.rows.into_iter().map(|r| r.lead))
        .collect()
}

/// `report_lines`, with row number `sel` (in `flat_leads` order) highlighted; and the line it
/// landed on, so the caller can scroll to it.
pub fn report_view(
    report: &Report,
    width: usize,
    now: Timestamp,
    sel: Option<usize>,
    briefs: &HashMap<String, Summary>,
) -> (Vec<Line<'static>>, Option<usize>) {
    let lanes = lanes(report);
    if lanes.is_empty() {
        return (vec![Line::from(Span::raw(" nothing to report").dim())], None);
    }
    let mut at = 0usize;
    let mut sel_line = None;
    // Columns right of the lead, added in the order they matter; the lead keeps its floor.
    let avail = width.saturating_sub(3);
    let mut used = 0;
    let mut take = |w: usize| {
        let fits = avail.saturating_sub(used + 2 + w) >= LEAD_FLOOR;
        if fits {
            used += 2 + w;
        }
        fits
    };
    let show_project = take(PROJECT_W);
    let show_source = take(SOURCE_W);
    let show_when = take(WHEN_W);
    let lead_w = avail.saturating_sub(used);
    let mut out = Vec::new();
    for (n, lane) in lanes.iter().enumerate() {
        if n > 0 {
            out.push(Line::raw(""));
        }
        let tally = format!("{} needs · {} live · {} done", lane.needs, lane.live, lane.done);
        let team = if lane.is_team { "  team" } else { "" };
        let name_w = width.saturating_sub(tally.chars().count() + team.chars().count() + 5);
        let name = fit(&lane.name, name_w.min(lane.name.chars().count()).max(1));
        let mut spans = vec![Span::raw(" "), Span::styled(name.trim_end().to_string(), theme::name())];
        if lane.is_team {
            spans.push(Span::styled(team, theme::dim()));
        }
        spans.push(Span::styled(format!("  {tally}"), theme::dim()));
        out.push(Line::from(spans));
        out.push(Line::from(Span::raw("─".repeat(width)).dim()));
        for row in &lane.rows {
            let l = row.lead;
            let (glyph, gstyle) = match l.band {
                Band::NeedsYou => ("⚠", theme::needs()),
                Band::Live => ("•", theme::name()),
                Band::Happened => ("✓", theme::dim()),
            };
            // A happened run with its brief under it is not told twice: the
            // row's line (its recap's, else its tail) gives way to the brief.
            let brief = (l.band == Band::Happened)
                .then(|| written(briefs, l).filter(|s| !s.deep))
                .flatten();
            let text = match l.summary.as_ref().filter(|_| brief.is_none()) {
                Some(s) => format!("{} — {s}", l.title),
                None => l.title.clone(),
            };
            let body = if l.band == Band::Happened {
                theme::dim()
            } else {
                theme::name()
            };
            // The headline shares the lead cell: the title keeps TITLE_MIN, the headline
            // takes what it needs of the rest (cut with `…`), so the width budget holds.
            let head = headline(l);
            let head_w = head.chars().count().min(lead_w.saturating_sub(TITLE_MIN + 2));
            let mut spans = vec![Span::raw(" "), Span::styled(glyph, gstyle), Span::raw(" ")];
            if head_w >= 2 {
                spans.push(Span::styled(fit(&text, lead_w - head_w - 2), body));
                spans.push(Span::styled(format!("  {}", fit(&head, head_w)), theme::dim()));
            } else {
                spans.push(Span::styled(fit(&text, lead_w), body));
            }
            let dim = |t: String| Span::styled(format!("  {t}"), theme::dim());
            if show_project {
                let base = l
                    .scope
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                spans.push(dim(fit(&base, PROJECT_W)));
            }
            if show_source {
                spans.push(dim(fit(&l.source, SOURCE_W)));
            }
            if show_when {
                spans.push(dim(format!("{:>w$}", fmt_clock(l.at, now), w = WHEN_W)));
            }
            let mut line = Line::from(spans);
            if sel == Some(at) {
                line = line.style(theme::selected());
                sel_line = Some(out.len());
            }
            at += 1;
            out.push(line);
            if let Some(s) = brief {
                let b = Brief::parse(&s.text);
                for (label, part) in [
                    ("worked  ", &b.worked_on),
                    ("asks    ", &b.questions),
                    ("next    ", &b.next),
                ] {
                    if let Some(t) = part {
                        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
                        out.push(Line::from(Span::styled(
                            fit(&format!("     {label}{t}"), width),
                            theme::dim(),
                        )));
                    }
                }
            }
        }
    }
    (out, sel_line)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rl_proto::lead::LeadStatus;
    use rl_proto::methods::SourceTag;

    fn ts(n: i64) -> Timestamp {
        Timestamp::from_second(n).unwrap()
    }

    fn mk_lead(source: &str, key: &str, band: Band, scope: Option<&str>, at: Timestamp) -> Lead {
        Lead {
            source: source.into(),
            key: key.into(),
            band,
            title: key.into(),
            summary: None,
            scope: scope.map(Into::into),
            action: None,
            status: LeadStatus::Open,
            at,
            expires_at: None,
            facts: None,
        }
    }

    fn text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn facts(f: impl FnOnce(&mut rl_proto::facts::RunFacts)) -> Option<rl_proto::facts::RunFacts> {
        let mut x = rl_proto::facts::RunFacts::default();
        f(&mut x);
        Some(x)
    }

    #[test]
    fn agent_headline_from_facts() {
        let mut l = mk_lead("claude", "run:r1", Band::Happened, None, ts(1));
        l.facts = facts(|f| {
            f.added = 180;
            f.removed = 40;
            f.active_secs = Some(840);
            f.commits = 3;
        });
        assert_eq!(headline(&l), "+180/−40 · 14m · 3 commits");
    }

    #[test]
    fn agent_headline_omits_zero_clauses() {
        let mut l = mk_lead("claude", "run:r1", Band::Happened, None, ts(1));
        l.facts = facts(|f| f.commits = 2);
        assert_eq!(headline(&l), "2 commits");
        l.facts = facts(|f| f.active_secs = Some(30));
        assert_eq!(headline(&l), "");
    }

    #[test]
    fn shell_headline_errors() {
        let mut l = mk_lead("shell", "run:r1", Band::Happened, None, ts(1));
        l.facts = facts(|f| f.errors = 2);
        assert_eq!(headline(&l), "2 errors");
        l.facts = facts(|_| {});
        assert_eq!(headline(&l), "ok");
    }

    #[test]
    fn flow_headline_is_fired() {
        let mut l = mk_lead("triage", "firing:f1", Band::Happened, None, ts(1));
        l.facts = facts(|_| {});
        assert_eq!(headline(&l), "fired");
    }

    #[test]
    fn headline_absent_facts_is_empty() {
        let l = mk_lead("claude", "run:r1", Band::Happened, None, ts(1));
        assert_eq!(headline(&l), "");
    }

    #[test]
    fn headline_shows_on_the_row_within_the_width() {
        let mut r = sample();
        r.happened[0].facts = facts(|f| {
            f.added = 180;
            f.removed = 40;
            f.commits = 3;
        });
        let ls: Vec<String> = report_lines(&r, 80, ts(1_700_000_100), &HashMap::new())
            .iter()
            .map(text)
            .collect();
        let row = ls.iter().find(|l| l.contains('✓')).unwrap();
        assert!(row.contains("+180/−40"), "{ls:?}");
        assert!(ls.iter().all(|l| l.chars().count() <= 80), "{ls:?}");
    }

    fn sample() -> Report {
        Report {
            needs_you: vec![mk_lead(
                "claude",
                "a very long lead title that will certainly not fit in a narrow column of any kind",
                Band::NeedsYou,
                Some("/p/relay"),
                ts(1_700_000_000),
            )],
            live: vec![],
            happened: vec![mk_lead(
                "claude",
                "done",
                Band::Happened,
                Some("/p/relay"),
                ts(1_600_000_000),
            )],
            tags: vec![],
        }
    }

    #[test]
    fn report_lines_head_each_lane_with_its_tally_and_cut_long_leads() {
        let ls: Vec<String> = report_lines(&sample(), 80, ts(1_700_000_100), &HashMap::new())
            .iter()
            .map(text)
            .collect();
        assert!(
            ls[0].contains("relay") && ls[0].contains("1 needs · 0 live · 1 done"),
            "{ls:?}"
        );
        assert!(ls[1].starts_with('─'));
        assert!(ls[2].contains('⚠') && ls[2].contains('…'), "{ls:?}");
        assert!(ls.iter().all(|l| l.chars().count() <= 80), "{ls:?}");
        assert!(ls[2].contains("claude") && ls[2].contains("relay"), "{ls:?}");
        // project before co-worker, as in the mock
        assert!(ls[2].find("relay") < ls[2].find("claude"), "{ls:?}");
    }

    #[test]
    fn report_view_marks_the_selected_row_and_says_which_line() {
        let r = sample();
        let (lines, at) = report_view(&r, 80, ts(1_700_000_100), Some(1), &HashMap::new());
        assert_eq!(at, Some(3), "second lead row, past a head and a rule");
        assert_eq!(lines[3].style, theme::selected());
        assert_ne!(lines[2].style, theme::selected());
        assert_eq!(flat_leads(&r).len(), 2);
    }

    #[test]
    fn report_lines_drop_the_when_column_when_narrow_but_keep_the_lead() {
        let now = ts(1_700_000_100);
        let wide: Vec<String> = report_lines(&sample(), 80, now, &HashMap::new())
            .iter()
            .map(text)
            .collect();
        let narrow: Vec<String> = report_lines(&sample(), 40, now, &HashMap::new())
            .iter()
            .map(text)
            .collect();
        assert!(narrow[2].contains("a very long"), "{narrow:?}");
        assert!(narrow.iter().all(|l| l.chars().count() <= 40), "{narrow:?}");
        let clock = fmt_clock(ts(1_700_000_000), now);
        assert!(
            wide[2].contains(&clock) && !narrow[2].contains(&clock),
            "{wide:?} {narrow:?}"
        );
    }

    #[test]
    fn untagged_lead_lands_under_its_project_basename() {
        let r = Report {
            happened: vec![],
            needs_you: vec![],
            live: vec![mk_lead(
                "shell",
                "r1",
                Band::Live,
                Some("/Users/x/Projects/relay"),
                ts(1),
            )],
            tags: vec![],
        };
        let ls = lanes(&r);
        assert_eq!(ls[0].name, "relay");
        assert!(!ls[0].is_team);
    }

    #[test]
    fn tag_moves_every_band_of_a_source_into_one_lane() {
        let r = Report {
            needs_you: vec![mk_lead("claude", "a", Band::NeedsYou, Some("/p/relay"), ts(3))],
            live: vec![mk_lead("claude", "b", Band::Live, Some("/p/relay"), ts(2))],
            happened: vec![mk_lead("codex", "c", Band::Happened, Some("/p/relay"), ts(1))],
            tags: vec![
                SourceTag {
                    source: "claude".into(),
                    tag: "dev".into(),
                },
                SourceTag {
                    source: "codex".into(),
                    tag: "dev".into(),
                },
            ],
        };
        let ls = lanes(&r);
        assert_eq!(ls.len(), 1);
        assert_eq!(ls[0].name, "dev");
        assert!(ls[0].is_team);
        assert_eq!(ls[0].rows.len(), 3);
        assert_eq!(ls[0].rows[0].lead.band, Band::NeedsYou);
        assert_eq!((ls[0].needs, ls[0].live, ls[0].done), (1, 1, 1));
    }

    #[test]
    fn needs_bearing_lanes_sort_first() {
        let r = Report {
            needs_you: vec![mk_lead("claude", "a", Band::NeedsYou, Some("/p/relay"), ts(1))],
            live: vec![mk_lead("shell", "b", Band::Live, Some("/p/ops"), ts(9))],
            happened: vec![],
            tags: vec![],
        };
        let ls = lanes(&r);
        assert_eq!(ls[0].name, "relay");
    }

    #[test]
    fn scopeless_lead_falls_to_my_work() {
        let r = Report {
            needs_you: vec![],
            live: vec![],
            tags: vec![],
            happened: vec![mk_lead("agent", "x", Band::Happened, None, ts(1))],
        };
        assert_eq!(lanes(&r)[0].name, "my work");
    }

    pub(crate) fn summary_of(run: &str, text: &str) -> Summary {
        Summary {
            run: run.into(),
            agent: "claude".into(),
            by: "a1".into(),
            at: ts(1),
            text: text.into(),
            since: None,
            source: "summarised".into(),
            reach: String::new(),
            stale: false,
            target: String::new(),
            deep: false,
            from: None,
            calls: 1,
            runs: 1,
        }
    }

    #[test]
    fn a_happened_rows_brief_shows_under_it() {
        let r = Report {
            needs_you: vec![],
            live: vec![],
            happened: vec![Lead {
                summary: Some("the parser".into()),
                ..mk_lead("claude", "run:r1", Band::Happened, Some("/p/relay"), ts(5))
            }],
            tags: vec![],
        };
        let mut briefs = HashMap::new();
        briefs.insert(
            "r1".to_string(),
            summary_of("r1", "Worked on: the parser\nOpen questions: none\nNext: tests"),
        );
        let (lines, at) = report_view(&r, 80, ts(10), Some(0), &briefs);
        let ls: Vec<String> = lines.iter().map(text).collect();
        let row = ls.iter().position(|l| l.contains('✓')).unwrap();
        assert_eq!(
            ls.iter().filter(|l| l.contains("the parser")).count(),
            1,
            "told once, by the brief: {ls:?}"
        );
        assert!(ls[row + 1].contains("worked  the parser"), "{ls:?}");
        assert!(ls[row + 2].contains("next    tests"), "{ls:?}");
        assert!(!ls.iter().any(|l| l.contains("asks")), "{ls:?}");
        assert_eq!(at, Some(row));
        briefs.get_mut("r1").unwrap().stale = true;
        let none = report_lines(&r, 80, ts(10), &briefs);
        assert!(!none.iter().map(text).any(|l| l.contains("worked")));
    }
}
