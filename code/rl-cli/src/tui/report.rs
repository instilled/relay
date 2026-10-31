//! The report drawn: the inbox's list pane and reading pane, as lines.

use std::collections::HashMap;
use std::ops::Range;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use rl_core::time::{fmt_age, fmt_when};
use rl_proto::lead::Lead;
use rl_proto::methods::Report;
use rl_proto::queue::{Changed, Hint, ItemView, RestKind};
use rl_proto::types::Summary;

use super::inbox::{Inbox, Mark, Sel, Team, mark, to_review};
use super::pane::wrap;
use super::rows::fit;
use super::theme;

pub(crate) fn lane_name(lead: &Lead, tags: &HashMap<&str, &str>) -> String {
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

/// The recap written for a run's lead, when there is one that still holds: a stale one
/// is as good as absent, so the row falls to its own line.
pub(crate) fn written<'a>(briefs: &'a HashMap<String, Summary>, lead: &Lead) -> Option<&'a Summary> {
    let id = lead.key.strip_prefix("run:")?;
    briefs.get(id).filter(|s| !s.stale)
}

/// Two panes from this many columns; one below.
pub const INBOX_SPLIT_W: u16 = 100;

/// The list pane's width in a frame `w` columns wide.
pub fn list_w(w: u16) -> u16 {
    (u32::from(w) * 36 / 100).clamp(34, 52) as u16
}

/// A block trim: at most this many wrapped lines.
const BLOCK_LINES: usize = 6;

fn glyph(m: Mark) -> (&'static str, Style) {
    match m {
        Mark::Asks => ("●", theme::needs()),
        Mark::Failed => ("✕", theme::bad()),
        Mark::Done => ("✓", theme::dim()),
        Mark::Working => ("◐", Style::new().fg(theme::LIVE)),
    }
}

pub(crate) fn commits(n: u32) -> String {
    match n {
        1 => "1 commit".into(),
        n => format!("{n} commits"),
    }
}

pub(crate) fn needs_you(n: usize) -> String {
    match n {
        1 => "1 needs you".into(),
        n => format!("{n} need you"),
    }
}

/// `+X/−Y · N commits` over the leads that work in a repo; `None` when none does.
pub(crate) fn git(leads: &[&Lead]) -> Option<String> {
    let changed: Vec<&Changed> = leads.iter().filter_map(|l| l.rest.as_ref()?.changed.as_ref()).collect();
    if changed.is_empty() {
        return None;
    }
    let (a, r, c) = changed
        .iter()
        .fold((0, 0, 0), |(a, r, c), x| (a + x.added, r + x.removed, c + x.commits));
    Some(format!("+{a}/−{r} · {}", commits(c)))
}

/// What the session last said: its rest's message, else the lead's own line.
pub(crate) fn last(lead: &Lead) -> Option<&str> {
    lead.rest
        .as_ref()
        .and_then(|r| r.last.as_deref())
        .or(lead.summary.as_deref())
        .filter(|t| !t.trim().is_empty())
}

/// A line trim's source: the first line with anything on it.
pub(crate) fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// The time of an item's rest, or how long it has worked.
fn when(lead: &Lead, now: Timestamp) -> String {
    match mark(lead) {
        Mark::Working => fmt_age(lead.at, now),
        _ => fmt_when(lead.at, now),
    }
}

fn clock(t: Timestamp) -> String {
    t.to_zoned(TimeZone::system()).strftime("%H:%M").to_string()
}

fn chip(h: Hint) -> Span<'static> {
    let style = match h {
        Hint::Question | Hint::Permission => theme::needs(),
        Hint::Failed => theme::bad(),
        Hint::Stopped | Hint::Unsure => theme::dim(),
    };
    Span::styled(h.as_str(), style)
}

fn chips(lead: &Lead) -> Vec<Span<'static>> {
    let hints = lead.rest.as_ref().map_or(&[][..], |r| &r.item.hints[..]);
    let mut out = Vec::new();
    for h in hints {
        if !out.is_empty() {
            out.push(Span::raw(" "));
        }
        out.push(chip(*h));
    }
    out
}

/// Parts joined by a dim ` · `, empty ones left out.
fn joined(parts: Vec<Vec<Span<'static>>>) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for p in parts.into_iter().filter(|p| !p.is_empty()) {
        if !out.is_empty() {
            out.push(Span::styled(" · ", theme::dim()));
        }
        out.extend(p);
    }
    out
}

/// A count as a part: left out when zero.
fn count(n: usize, words: String, style: Style) -> Vec<Span<'static>> {
    if n == 0 {
        Vec::new()
    } else {
        vec![Span::styled(words, style)]
    }
}

/// `spans` cut to `width` columns, the one across the edge ending in `…`.
fn clip(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut out = Vec::new();
    let mut used = 0;
    for s in spans {
        let w = s.width();
        if used + w <= width {
            used += w;
            out.push(s);
            continue;
        }
        if width > used {
            out.push(Span::styled(fit(&s.content, width - used), s.style));
        }
        break;
    }
    Line::from(out)
}

/// `left` and `right` on a line `width` wide, `right` against the edge; the left gives way.
fn spread(left: Vec<Span<'static>>, right: Span<'static>, width: usize) -> Line<'static> {
    let rw = right.width();
    if rw + 2 > width {
        return clip(left, width);
    }
    let mut line = clip(left, width - rw - 2);
    let pad = width - rw - line.width();
    line.spans.push(Span::raw(" ".repeat(pad)));
    line.spans.push(right);
    line
}

/// The selected line: filled to the edge and lit.
fn lit(mut line: Line<'static>, width: usize) -> Line<'static> {
    let short = width.saturating_sub(line.width());
    if short > 0 {
        line.spans.push(Span::raw(" ".repeat(short)));
    }
    line.style(theme::selected())
}

/// Where the selection sits among the list pane's lines: its block, and the heading of the
/// section it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    pub head: usize,
    pub lines: Range<usize>,
}

/// Adds a block of lines under the heading at `head`, lit through when it is the selection;
/// `at` says where it is.
fn put(
    out: &mut Vec<Line<'static>>,
    at: &mut Option<Picked>,
    head: usize,
    block: Vec<Line<'static>>,
    on: bool,
    width: usize,
) {
    if on {
        let start = out.len();
        out.extend(block.into_iter().map(|l| lit(l, width)));
        *at = Some(Picked {
            head,
            lines: start..out.len(),
        });
    } else {
        out.extend(block);
    }
}

/// The first of `len` lines to show in `height` rows, moved from `top` only as far as keeps
/// the whole selection in view, and its section's heading too where that fits.
pub fn scroll(top: usize, height: usize, len: usize, picked: Option<&Picked>) -> usize {
    let mut top = top.min(len.saturating_sub(height));
    if let Some(p) = picked {
        if p.lines.end > top + height {
            top = p.lines.end.saturating_sub(height);
        }
        // A block taller than the pane shows from its start.
        if p.lines.start < top {
            top = p.lines.start;
        }
        if p.head < top && p.lines.end <= p.head + height {
            top = p.head;
        }
    }
    top
}

fn dim(t: impl Into<String>) -> Span<'static> {
    Span::styled(t.into(), theme::dim())
}

fn team_of<'a>(inbox: &'a Inbox, key: &str) -> Option<&'a Team<'a>> {
    inbox.teams.iter().find(|t| t.items.iter().any(|l| l.key == key))
}

/// A team's counts: needs you, failed, done, working, then commits where there is git.
fn team_counts(t: &Team) -> Vec<Span<'static>> {
    joined(vec![
        count(t.needs, needs_you(t.needs), theme::needs()),
        count(t.failed, format!("{} failed", t.failed), theme::bad()),
        count(t.done, format!("{} done", t.done), theme::dim()),
        count(
            t.working,
            format!("{} working", t.working),
            Style::new().fg(theme::LIVE),
        ),
        t.commits.map(|c| vec![dim(commits(c))]).unwrap_or_default(),
    ])
}

/// An item's two lines in the list: its mark, name and time; then its hint chips,
/// git where there is one, and the last message's first line.
fn item_rows(l: &Lead, width: usize, now: Timestamp) -> Vec<Line<'static>> {
    let (g, gs) = glyph(mark(l));
    let top = vec![
        Span::raw(" "),
        Span::styled(g, gs),
        Span::raw(" "),
        Span::styled(l.title.clone(), theme::name()),
    ];
    let mut sub = vec![Span::raw("   ")];
    sub.extend(joined(vec![
        chips(l),
        git(&[l]).map(|g| vec![dim(g)]).unwrap_or_default(),
        last(l).map(|t| vec![dim(first_line(t))]).unwrap_or_default(),
    ]));
    vec![spread(top, dim(when(l, now)), width), clip(sub, width)]
}

/// The list pane, `width` columns wide, and where its selection is. Outside a team: All
/// teams, the NEEDS YOU strip, then a summary per team. Inside one: its items.
pub fn list_lines(
    inbox: &Inbox,
    sel: &Sel,
    inside: Option<&str>,
    width: usize,
    now: Timestamp,
) -> (Vec<Line<'static>>, Option<Picked>) {
    let mut out = Vec::new();
    let mut at = None;
    let picked = |l: &Lead| matches!(sel, Sel::Item(k) if *k == l.key);
    if let Some(name) = inside {
        out.push(clip(
            vec![dim(" ‹ "), Span::styled(name.to_string(), theme::name())],
            width,
        ));
        let team = inbox.teams.iter().find(|t| t.name == name);
        for l in team.map_or(&[][..], |t| &t.items[..]) {
            put(&mut out, &mut at, 0, item_rows(l, width, now), picked(l), width);
        }
        return (out, at);
    }
    let failed: usize = inbox.teams.iter().map(|t| t.failed).sum();
    let mut tally = joined(vec![
        count(inbox.needs.len(), needs_you(inbox.needs.len()), theme::needs()),
        count(failed, format!("{failed} failed"), theme::bad()),
        count(inbox.to_review, format!("{} to review", inbox.to_review), theme::dim()),
        inbox.commits.map(|c| vec![dim(commits(c))]).unwrap_or_default(),
    ]);
    if tally.is_empty() {
        tally.push(dim("nothing to review"));
    }
    tally.insert(0, Span::raw("   "));
    let all = vec![
        clip(vec![Span::styled(" ≡ All teams", theme::name())], width),
        clip(tally, width),
    ];
    put(&mut out, &mut at, 0, all, *sel == Sel::All, width);
    if !inbox.needs.is_empty() {
        out.push(Line::raw(""));
        let head = out.len();
        out.push(clip(
            vec![Span::styled(
                format!(" NEEDS YOU · {}", inbox.needs.len()),
                theme::needs(),
            )],
            width,
        ));
        for l in &inbox.needs {
            let team = team_of(inbox, &l.key).map_or("", |t| t.name.as_str());
            let left = vec![
                Span::raw(" "),
                Span::styled("●", theme::needs()),
                Span::raw(" "),
                Span::styled(format!("{team} · {}", l.title), theme::name()),
            ];
            let ask = l.rest.as_ref().and_then(|r| {
                r.item
                    .hints
                    .iter()
                    .find(|h| matches!(h, Hint::Question | Hint::Permission | Hint::Unsure))
            });
            let line = match ask {
                Some(h) => spread(left, chip(*h), width),
                None => clip(left, width),
            };
            put(&mut out, &mut at, head, vec![line], picked(l), width);
        }
    }
    out.push(Line::raw(""));
    let head = out.len();
    out.push(clip(vec![dim(format!(" TEAMS · {}", inbox.teams.len()))], width));
    for t in &inbox.teams {
        let mut counts = vec![Span::raw("   ")];
        counts.extend(team_counts(t));
        let block = vec![
            clip(vec![dim(" ▸ "), Span::styled(t.name.clone(), theme::name())], width),
            clip(counts, width),
            clip(vec![dim(format!("   {}", t.gist))], width),
        ];
        let on = matches!(sel, Sel::Team(n) if *n == t.name);
        put(&mut out, &mut at, head, block, on, width);
    }
    (out, at)
}

/// `text` wrapped to `width` after an `indent`, as lines of `style`.
fn wrapped(text: &str, indent: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let w = width.saturating_sub(indent.chars().count()).max(1);
    text.lines()
        .flat_map(|l| wrap(l.trim(), w))
        .filter(|l| !l.is_empty())
        .map(|l| Line::from(vec![Span::raw(indent.to_string()), Span::styled(l, style)]))
        .collect()
}

/// A block trim: the text's lines wrapped to `width`, at most `BLOCK_LINES`, the last
/// marked `…` when it was cut.
fn block(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .flat_map(|l| wrap(l.trim(), width))
        .collect();
    if lines.len() > BLOCK_LINES {
        lines.truncate(BLOCK_LINES);
        let cut = &mut lines[BLOCK_LINES - 1];
        if cut.chars().count() >= width {
            cut.pop();
        }
        cut.push('…');
    }
    lines
}

fn label(words: impl Into<String>, style: Style) -> Line<'static> {
    Line::from(Span::styled(format!(" {}", words.into()), style))
}

/// The reading pane, `width` columns wide: whatever `sel` is.
pub fn reading_lines(
    report: &Report,
    inbox: &Inbox,
    sel: &Sel,
    view: Option<&ItemView>,
    briefs: &HashMap<String, Summary>,
    width: usize,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let lines = match sel {
        Sel::All => all_page(inbox, width),
        Sel::Team(name) => match inbox.teams.iter().find(|t| t.name == *name) {
            Some(t) => team_page(t, width, now),
            None => vec![label("gone from the report", theme::dim())],
        },
        Sel::Item(key) => {
            let lead = report
                .needs_you
                .iter()
                .chain(&report.live)
                .chain(&report.happened)
                .find(|l| l.key == *key);
            match lead {
                Some(l) => item_page(l, inbox, view, briefs, width, now),
                None => vec![label("gone from the report", theme::dim())],
            }
        }
    };
    lines.into_iter().map(|l| clip(l.spans, width)).collect()
}

/// To review, working and git, the zeros left out.
fn team_tally(t: &Team) -> Vec<Span<'static>> {
    let review = to_review(&t.items);
    joined(vec![
        count(review, format!("{review} to review"), theme::dim()),
        count(t.working, format!("{} working", t.working), theme::dim()),
        git(&t.items).map(|g| vec![dim(g)]).unwrap_or_default(),
    ])
}

fn headed(name: &str, tally: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::raw(" "), Span::styled(name.to_string(), theme::name())];
    if !tally.is_empty() {
        spans.push(dim(" · "));
        spans.extend(tally);
    }
    Line::from(spans)
}

/// All teams: a block per team, in the list's order.
fn all_page(inbox: &Inbox, width: usize) -> Vec<Line<'static>> {
    if inbox.teams.is_empty() {
        return vec![label("nothing to report", theme::dim())];
    }
    let mut out = vec![label("All teams", theme::name())];
    for t in &inbox.teams {
        out.push(Line::raw(""));
        out.push(headed(&t.name, team_tally(t)));
        out.extend(wrapped(&t.gist, "   ", width, Style::new()));
    }
    out
}

/// A team: what happened since you looked, then its items by group.
fn team_page(t: &Team, width: usize, now: Timestamp) -> Vec<Line<'static>> {
    let mut out = vec![headed(&t.name, team_tally(t)), Line::raw("")];
    out.push(label("SINCE YOU LOOKED", theme::dim()));
    let said: Vec<String> = [
        (t.done, "done"),
        (t.failed, "failed"),
        (t.needs, "ask"),
        (t.working, "working"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, w)| format!("{n} {w}"))
    .collect();
    if !said.is_empty() {
        out.extend(wrapped(&format!("{}.", said.join(", ")), " ", width, Style::new()));
    }
    if let Some(g) = git(&t.items) {
        out.push(Line::from(vec![Span::raw(" "), dim(g)]));
    }
    out.extend(wrapped(&t.gist, " ", width, Style::new()));
    let title_w = (width / 3).clamp(12, 28);
    for (head, style, marks) in [
        ("NEEDS YOU", theme::needs(), &[Mark::Asks][..]),
        ("DONE", theme::dim(), &[Mark::Failed, Mark::Done][..]),
        ("WORKING", Style::new().fg(theme::LIVE), &[Mark::Working][..]),
    ] {
        let items: Vec<&Lead> = t.items.iter().copied().filter(|l| marks.contains(&mark(l))).collect();
        if items.is_empty() {
            continue;
        }
        out.push(Line::raw(""));
        out.push(label(format!("{head} · {}", items.len()), style));
        for l in items {
            let (g, gs) = glyph(mark(l));
            let what: Vec<String> = [
                git(&[l]).map(|g| g.split(" · ").next().unwrap_or("").to_string()),
                last(l).map(|t| first_line(t).to_string()),
            ]
            .into_iter()
            .flatten()
            .collect();
            let left = vec![
                Span::raw(" "),
                Span::styled(g, gs),
                Span::raw(" "),
                Span::styled(fit(&l.title, title_w), theme::name()),
                Span::raw("  "),
                dim(what.join(" · ")),
            ];
            out.push(spread(left, dim(when(l, now)), width));
        }
    }
    out
}

/// An item: who and where, what it said, what changed, the deep read, and the log.
fn item_page(
    l: &Lead,
    inbox: &Inbox,
    view: Option<&ItemView>,
    briefs: &HashMap<String, Summary>,
    width: usize,
    now: Timestamp,
) -> Vec<Line<'static>> {
    let team = team_of(inbox, &l.key).map_or("", |t| t.name.as_str());
    let mut out = vec![Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{team} · {}", l.title), theme::name()),
    ])];
    let rest = l.rest.as_ref();
    let changed = view
        .and_then(|v| v.changed.as_ref())
        .or(rest.and_then(|r| r.changed.as_ref()));
    let branch = view
        .and_then(|v| v.branch.clone())
        .or_else(|| changed.and_then(|c| c.branch.clone()));
    let mut who = vec![l.source.clone()];
    who.extend(branch);
    if let Some(i) = rest.map(|r| &r.item).filter(|i| i.rests > 1) {
        who.push(format!("{} turns since {}", i.rests, fmt_when(i.from, now)));
    }
    out.push(Line::from(vec![Span::raw(" "), dim(who.join(" · "))]));
    out.push(Line::raw(""));

    let m = mark(l);
    let (head, style) = match m {
        Mark::Asks => ("ASKS", theme::needs()),
        Mark::Failed => ("FAILED", theme::bad()),
        Mark::Done => ("DONE", theme::dim()),
        Mark::Working => ("WORKING", Style::new().fg(theme::LIVE)),
    };
    let mut heading = vec![Span::styled(format!(" {head} · {}", when(l, now)), style)];
    let hints = chips(l);
    if !hints.is_empty() {
        heading.push(Span::raw("  "));
        heading.extend(hints);
    }
    out.push(Line::from(heading));
    // An ask's message in a box with its question picked out; anything else's alone.
    let boxed = m == Mark::Asks && width >= 10;
    let inner = if boxed { width - 5 } else { width.saturating_sub(1) };
    // A finished message that says nothing ("Done.") gives way to what changed, when git
    // has something to say; with no repo it stands.
    let says_nothing = rest.is_some_and(|r| r.item.kind == RestKind::Finished)
        && changed.is_some()
        && last(l).is_some_and(|t| t.split_whitespace().count() <= 3);
    let said = match last(l) {
        Some(t) if !says_nothing => block(t, inner),
        _ => Vec::new(),
    };
    let styled = |t: &str| if t.contains('?') { theme::name() } else { Style::new() };
    if boxed {
        out.push(dim(format!(" ┌{}┐", "─".repeat(width - 3))).into());
        for t in &said {
            out.push(Line::from(vec![
                dim(" │ "),
                Span::styled(fit(t, inner), styled(t)),
                dim(" │"),
            ]));
        }
        out.push(dim(format!(" └{}┘", "─".repeat(width - 3))).into());
    } else {
        out.extend(
            said.iter()
                .map(|t| Line::from(vec![Span::raw(" "), Span::styled(t.clone(), styled(t))])),
        );
    }

    if let Some(c) = changed {
        out.push(Line::raw(""));
        out.push(label("CHANGED SINCE YOU LOOKED", theme::dim()));
        let mut facts = vec![format!("+{}/−{} · {}", c.added, c.removed, commits(c.commits))];
        facts.push(if c.pushed { "pushed".into() } else { "not pushed".into() });
        out.push(Line::raw(format!(" {}", facts.join(" · "))));
        out.extend(c.subjects.iter().map(|s| Line::from(dim(format!("   {s}")))));
    }

    out.push(Line::raw(""));
    out.push(label("DEEP READ", theme::dim()));
    match written(briefs, l).filter(|s| s.deep) {
        Some(s) => out.extend(wrapped(&s.text, " ", width, Style::new())),
        None => out.push(Line::from(dim(" not made · p reads the log"))),
    }

    out.push(Line::raw(""));
    out.push(label("LOG SINCE YOU LOOKED", theme::dim()));
    match view {
        None => out.push(Line::from(dim(" reading…"))),
        Some(v) if v.log.is_empty() => out.push(Line::from(dim(" nothing since"))),
        Some(v) => {
            for e in &v.log {
                let who = if e.who == "you" { theme::name() } else { theme::dim() };
                out.push(Line::from(vec![
                    dim(format!(" {}  ", clock(e.at))),
                    Span::styled(fit(&e.who, 8), who),
                    Span::raw(format!("  {}", first_line(&e.text))),
                ]));
            }
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rl_proto::lead::{Band, LeadStatus};

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
            rest: None,
        }
    }

    fn text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_team_is_its_tag_else_its_folder_else_my_work() {
        let tags = HashMap::from([("claude", "dev")]);
        let tagged = mk_lead("claude", "a", Band::NeedsYou, Some("/p/relay"), ts(1));
        assert_eq!(lane_name(&tagged, &tags), "dev");
        let untagged = mk_lead("shell", "b", Band::Live, Some("/Users/x/Projects/relay"), ts(1));
        assert_eq!(lane_name(&untagged, &tags), "relay");
        let scopeless = mk_lead("agent", "c", Band::Happened, None, ts(1));
        assert_eq!(lane_name(&scopeless, &tags), "my work");
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

    use super::super::inbox::{Sel, inbox};
    use rl_proto::queue::{Changed, Hint, How, ItemView, LeadRest, LogLine, QueueItem, RestKind};

    fn queued(mut l: Lead, kind: RestKind, hints: &[Hint], last: &str, changed: Option<Changed>) -> Lead {
        l.rest = Some(LeadRest {
            item: QueueItem {
                source: "run".into(),
                reference: l.key.clone(),
                kind,
                how: How::Reply,
                hints: hints.to_vec(),
                at: l.at,
                rests: 3,
                from: ts(1_000),
                from_head: None,
            },
            last: Some(last.into()),
            changed,
        });
        l
    }

    fn titled(mut l: Lead, title: &str) -> Lead {
        l.title = title.into();
        l
    }

    fn git(added: u64, removed: u64, commits: u32) -> Option<Changed> {
        Some(Changed {
            commits,
            added,
            removed,
            subjects: vec!["fix the parser".into()],
            branch: Some("main".into()),
            pushed: false,
        })
    }

    /// Two teams: relay with an ask, a done item in git and a working run;
    /// docs with a failed item and a done one, no repo.
    fn board(relay_git: bool) -> Report {
        let g = |a, r, c| if relay_git { git(a, r, c) } else { None };
        Report {
            needs_you: vec![titled(
                queued(
                    mk_lead("claude", "run:a1", Band::NeedsYou, Some("/p/relay"), ts(5_000)),
                    RestKind::Asks,
                    &[Hint::Question],
                    "Removed 23 worktrees and their branches, none with uncommitted work.\nShould I prune the stale local branches too?",
                    None,
                ),
                "clean worktrees with a title long enough to need cutting in any narrow pane",
            )],
            live: vec![titled(
                mk_lead("claude", "run:w1", Band::Live, Some("/p/relay"), ts(4_000)),
                "review-queue spike",
            )],
            happened: vec![
                titled(
                    queued(
                        mk_lead("claude", "run:d1", Band::Happened, Some("/p/relay"), ts(3_000)),
                        RestKind::Finished,
                        &[],
                        "All eight review findings fixed, gates green, and a very long tail of words after that",
                        g(412, 96, 3),
                    ),
                    "report step 4 fixes",
                ),
                titled(
                    queued(
                        mk_lead("codex", "run:f1", Band::Happened, Some("/p/docs"), ts(2_000)),
                        RestKind::Finished,
                        &[Hint::Failed],
                        "The nightly checks broke in the checkout spec",
                        None,
                    ),
                    "nightly checks",
                ),
                titled(
                    queued(
                        mk_lead("codex", "run:d2", Band::Happened, Some("/p/docs"), ts(1_500)),
                        RestKind::Finished,
                        &[],
                        "Done.",
                        None,
                    ),
                    "docs summary",
                ),
            ],
            tags: vec![],
        }
    }

    fn texts(ls: &[Line<'_>]) -> Vec<String> {
        ls.iter().map(text).collect()
    }

    fn sels() -> Vec<Sel> {
        vec![
            Sel::All,
            Sel::Team("relay".into()),
            Sel::Team("docs".into()),
            Sel::Item("run:a1".into()),
            Sel::Item("run:d1".into()),
            Sel::Item("run:f1".into()),
            Sel::Item("run:w1".into()),
        ]
    }

    fn view() -> ItemView {
        ItemView {
            log: vec![
                LogLine {
                    at: ts(4_900),
                    who: "you".into(),
                    text: "clean up the worktrees that are merged\nand more".into(),
                },
                LogLine {
                    at: ts(5_000),
                    who: "claude".into(),
                    text: "Should I prune them too?".into(),
                },
            ],
            changed: None,
            branch: None,
        }
    }

    #[test]
    fn list_lines_fit_the_width() {
        let now = ts(6_000);
        let r = board(true);
        let i = inbox(&r);
        for width in [40, 52] {
            for inside in [None, Some("relay"), Some("docs")] {
                for sel in sels() {
                    let (ls, _) = list_lines(&i, &sel, inside, width, now);
                    let ls = texts(&ls);
                    assert!(
                        ls.iter().all(|l| l.chars().count() <= width),
                        "{width} {inside:?} {ls:#?}"
                    );
                }
            }
            for sel in sels() {
                let v = view();
                let ls = texts(&reading_lines(&r, &i, &sel, Some(&v), &HashMap::new(), width, now));
                assert!(ls.iter().all(|l| l.chars().count() <= width), "{width} {sel:?} {ls:#?}");
            }
        }
    }

    #[test]
    fn the_list_heads_with_all_teams_then_needs_you_then_teams() {
        let r = board(true);
        let i = inbox(&r);
        let (ls, at) = list_lines(&i, &Sel::Team("docs".into()), None, 52, ts(6_000));
        let t = texts(&ls);
        assert!(t[0].contains("≡ All teams"), "{t:#?}");
        assert!(t[1].contains("1 needs you") && t[1].contains("1 failed"), "{t:#?}");
        assert!(t[1].contains("4 to review") && t[1].contains("3 commits"), "{t:#?}");
        let needs = t.iter().position(|l| l.contains("NEEDS YOU · 1")).unwrap();
        assert!(t[needs + 1].contains("● relay · clean worktrees"), "{t:#?}");
        assert!(t[needs + 1].trim_end().ends_with("question"), "{t:#?}");
        let teams = t.iter().position(|l| l.contains("TEAMS · 2")).unwrap();
        assert!(teams > needs);
        assert!(t[teams + 1].contains("▸ relay"), "{t:#?}");
        let counts = &t[teams + 2];
        assert!(
            counts.contains("1 needs you · 1 done · 1 working · 3 commits"),
            "{t:#?}"
        );
        assert!(t[teams + 4].contains("▸ docs"), "{t:#?}");
        assert!(t[teams + 5].contains("1 failed · 1 done") && !t[teams + 5].contains("commit"));
        // The selected team is lit through its three lines, and its first is said.
        assert_eq!(
            at,
            Some(Picked {
                head: teams,
                lines: teams + 4..teams + 7
            })
        );
        assert!(ls[teams + 4..teams + 7].iter().all(|l| l.style == theme::selected()));
        assert_ne!(ls[teams + 3].style, theme::selected());
    }

    #[test]
    fn no_git_draws_no_git() {
        let now = ts(6_000);
        let r = board(false);
        let i = inbox(&r);
        let mut all = Vec::new();
        for inside in [None, Some("relay"), Some("docs")] {
            for sel in sels() {
                all.extend(texts(&list_lines(&i, &sel, inside, 52, now).0));
            }
        }
        for sel in sels() {
            let v = view();
            all.extend(texts(&reading_lines(&r, &i, &sel, Some(&v), &HashMap::new(), 80, now)));
        }
        for bad in ["+", "commits", "CHANGED", "git", "main", "pushed"] {
            assert!(!all.iter().any(|l| l.contains(bad)), "{bad}: {all:#?}");
        }
    }

    #[test]
    fn inside_a_team_lists_its_items_only() {
        let r = board(true);
        let i = inbox(&r);
        let (ls, at) = list_lines(&i, &Sel::Item("run:d1".into()), Some("relay"), 52, ts(6_000));
        let t = texts(&ls);
        assert!(t[0].starts_with(" ‹ relay"), "{t:#?}");
        assert_eq!(t.len(), 1 + 3 * 2, "a back line, then two lines an item: {t:#?}");
        assert!(t[1].contains("● clean worktrees"), "{t:#?}");
        assert!(t[2].contains("question") && t[2].contains("Removed 23"), "{t:#?}");
        assert!(t[3].contains("✓ report step 4 fixes"), "{t:#?}");
        assert!(t[4].contains("+412/−96 · 3 commits · All eight"), "{t:#?}");
        assert!(t[5].contains("◐ review-queue spike"), "{t:#?}");
        assert!(t[5].trim_end().ends_with("33m"), "worked for, not when: {t:#?}");
        for gone in ["nightly", "docs summary", "All teams", "NEEDS YOU", "TEAMS"] {
            assert!(!t.iter().any(|l| l.contains(gone)), "{gone}: {t:#?}");
        }
        assert_eq!(at, Some(Picked { head: 0, lines: 3..5 }));
    }

    #[test]
    fn the_list_keeps_the_whole_selection_and_its_heading_in_view() {
        let now = ts(6_000);
        let mut r = board(false);
        r.happened.extend((0..8).map(|n| {
            let scope = format!("/p/team{n}");
            queued(
                mk_lead("codex", &format!("run:t{n}"), Band::Happened, Some(&scope), ts(100 + n)),
                RestKind::Finished,
                &[],
                "Done.",
                None,
            )
        }));
        let i = inbox(&r);
        let last = i.teams.last().unwrap().name.clone();
        let (ls, at) = list_lines(&i, &Sel::Team(last), None, 52, now);
        let at = at.unwrap();
        let h = 8;
        assert!(ls.len() > h, "taller than the pane");
        // From the top, the last team's three lines come into view, all of them.
        let top = scroll(0, h, ls.len(), Some(&at));
        assert!(top <= at.lines.start && at.lines.end <= top + h, "{top} {at:?}");
        assert_eq!(top, at.lines.end - h, "only as far as needed");
        // Back up at the strip, its heading comes along where it fits.
        let (ls, at) = list_lines(&i, &Sel::Item("run:a1".into()), None, 52, now);
        let at = at.unwrap();
        let top = scroll(ls.len(), h, ls.len(), Some(&at));
        assert_eq!(top, at.head, "{at:?}");
        assert!(texts(&ls)[top].contains("NEEDS YOU"));
        // A block taller than the pane shows from its start.
        let tall = Picked { head: 0, lines: 10..20 };
        assert_eq!(scroll(0, 4, 30, Some(&tall)), 10);
    }

    #[test]
    fn all_teams_reads_a_block_per_team_in_the_lists_order() {
        let r = board(true);
        let i = inbox(&r);
        let t = texts(&reading_lines(&r, &i, &Sel::All, None, &HashMap::new(), 80, ts(6_000)));
        let relay = t.iter().position(|l| l.starts_with(" relay")).unwrap();
        let docs = t.iter().position(|l| l.starts_with(" docs")).unwrap();
        assert!(relay < docs, "{t:#?}");
        assert!(
            t[relay].contains("2 to review · 1 working · +412/−96 · 3 commits"),
            "{t:#?}"
        );
        assert!(t[relay + 1].contains("clean worktrees"), "the gist: {t:#?}");
        assert!(t[docs].contains("2 to review") && !t[docs].contains('+'), "{t:#?}");
    }

    #[test]
    fn a_team_reads_since_you_looked_then_its_groups() {
        let r = board(true);
        let i = inbox(&r);
        let sel = Sel::Team("relay".into());
        let t = texts(&reading_lines(&r, &i, &sel, None, &HashMap::new(), 80, ts(6_000)));
        let since = t.iter().position(|l| l.contains("SINCE YOU LOOKED")).unwrap();
        assert!(t[since + 1].contains("1 done, 1 ask, 1 working."), "{t:#?}");
        assert!(t[since + 2].contains("+412/−96 · 3 commits"), "{t:#?}");
        let at = |w: &str| t.iter().position(|l| l.contains(w)).unwrap();
        assert!(
            at("NEEDS YOU · 1") < at("DONE · 1") && at("DONE · 1") < at("WORKING · 1"),
            "{t:#?}"
        );
        assert!(t[at("DONE · 1") + 1].contains("+412/−96"), "{t:#?}");
    }

    #[test]
    fn an_ask_reads_boxed_with_its_question_picked_out() {
        let r = board(true);
        let i = inbox(&r);
        let v = view();
        let mut briefs = HashMap::new();
        let ls = reading_lines(&r, &i, &Sel::Item("run:a1".into()), Some(&v), &briefs, 80, ts(6_000));
        let t = texts(&ls);
        assert!(t[0].contains("relay · clean worktrees"), "{t:#?}");
        assert!(t[1].contains("claude") && t[1].contains("3 turns since"), "{t:#?}");
        let asks = t.iter().position(|l| l.contains("ASKS · ")).unwrap();
        assert!(t[asks + 1].contains('┌'), "{t:#?}");
        let q = t.iter().position(|l| l.contains("Should I prune")).unwrap();
        assert!(t[q].starts_with(" │ ") && t[q].ends_with(" │"), "{t:#?}");
        assert!(
            ls[q]
                .spans
                .iter()
                .any(|s| s.content.contains("Should") && s.style == theme::name())
        );
        assert!(!t.iter().any(|l| l.contains("CHANGED")), "no git on the ask: {t:#?}");
        assert!(t.iter().any(|l| l.contains("not made · p reads the log")), "{t:#?}");
        let log = t.iter().position(|l| l.contains("LOG SINCE YOU LOOKED")).unwrap();
        assert!(t[log + 1].contains("you") && t[log + 1].contains("clean up the worktrees"));
        assert!(!t[log + 1].contains("and more"), "first line only: {t:#?}");
        let mut deep = summary_of("a1", "It cleaned the worktrees and waits on the branches.");
        deep.deep = true;
        briefs.insert("a1".into(), deep);
        let t = texts(&reading_lines(
            &r,
            &i,
            &Sel::Item("run:a1".into()),
            Some(&v),
            &briefs,
            80,
            ts(6_000),
        ));
        assert!(t.iter().any(|l| l.contains("It cleaned the worktrees")), "{t:#?}");
        assert!(!t.iter().any(|l| l.contains("not made")), "{t:#?}");
    }

    #[test]
    fn a_finished_item_reads_its_last_and_what_changed() {
        let mut r = board(true);
        let long = (0..40).map(|n| format!("word{n}")).collect::<Vec<_>>().join(" ");
        r.happened[0].rest.as_mut().unwrap().last = Some(long);
        let i = inbox(&r);
        let t = texts(&reading_lines(
            &r,
            &i,
            &Sel::Item("run:d1".into()),
            None,
            &HashMap::new(),
            40,
            ts(6_000),
        ));
        assert!(t[1].contains("main"), "the branch: {t:#?}");
        let done = t.iter().position(|l| l.contains("DONE · ")).unwrap();
        let changed = t.iter().position(|l| l.contains("CHANGED SINCE YOU LOOKED")).unwrap();
        let block = &t[done + 1..changed];
        assert!(!block.iter().any(|l| l.contains('┌')), "no box: {t:#?}");
        assert_eq!(block.iter().filter(|l| l.contains("word")).count(), 6, "capped: {t:#?}");
        assert!(t[changed + 1].contains("+412/−96 · 3 commits"), "{t:#?}");
        assert!(t[changed + 1].contains("not pushed"), "{t:#?}");
        assert!(t[changed + 2].contains("fix the parser"), "{t:#?}");
        assert!(t.iter().any(|l| l.contains("reading…")), "no view yet: {t:#?}");
    }

    #[test]
    fn a_message_that_says_nothing_shows_what_changed_alone() {
        let mut r = board(true);
        r.happened[0].rest.as_mut().unwrap().last = Some("Done.".into());
        let read = |r: &Report, key: &str| {
            let i = inbox(r);
            texts(&reading_lines(
                r,
                &i,
                &Sel::Item(key.into()),
                None,
                &HashMap::new(),
                80,
                ts(6_000),
            ))
        };
        let t = read(&r, "run:d1");
        let done = t.iter().position(|l| l.contains("DONE · ")).unwrap();
        assert!(t[done + 1].is_empty(), "nothing said, straight to what changed: {t:#?}");
        assert!(t[done + 2].contains("CHANGED SINCE YOU LOOKED"), "{t:#?}");
        assert!(!t.iter().any(|l| l.trim() == "Done."), "{t:#?}");
        // With no repo, it is all there is: it stands.
        let t = read(&r, "run:d2");
        assert!(t.iter().any(|l| l.trim() == "Done."), "{t:#?}");
        assert!(!t.iter().any(|l| l.contains("CHANGED")), "{t:#?}");
    }
}
