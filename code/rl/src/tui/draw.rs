//! Drawing the list: the header with its toggles, the rows under their
//! headers, the command line (or the search, while one is open), and the
//! footer. Colour carries meaning — a
//! saturated accent per state, on the terminal's own foreground so it reads
//! on either theme (see `theme`). Nothing on screen is an id.

use super::app::{App, Mode};
use super::{cmdline, help, mark, theme};
use crate::config::Grouping;
use crate::git::checkout_words;
use crate::query;
use crate::rows::{self, COMMAND, Head, Line as ListLine, Names, SHELL, fit};
use crate::sources::{Row, Source};
use jiff::Timestamp;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use rl_core::state::RunState;
use rl_core::time::fmt_age;
use std::path::Path;

/// The panel's name, as the header says it.
const NAME: &str = "relay";
/// Text sits this far in from the left edge: the header's and the footer's.
const TEXT_INDENT: u16 = 2;
/// The tall header (the mark beside the words) wants a screen this big.
const TALL_H: u16 = 24;
const TALL_W: u16 = 80;
/// Between the mark and the words beside it.
const MARK_GAP: u16 = 3;
/// What the one-line header keeps for its headline before the toggles give
/// up their keys.
const HEADLINE_ROOM: usize = 24;
/// A row of air above the mark, so it does not sit on the screen's top edge.
const HEAD_AIR: u16 = 1;
/// The icon slot: a space, a one-column icon, a space. Every row has one, so
/// every title starts in the same column.
pub const ICON_W: usize = 3;
/// The state column's width: `needs you~`, and a harness's `needs input`, fit.
pub const WORD_W: usize = 11;
/// The room a row's harness has, at the end of the title's column.
pub const HARNESS_W: usize = 7;

/// The first of `n` lines to show in a window `h` tall so that line `sel` is
/// visible and not under an edge marker: when the window starts past the
/// top, its first line says how much is above; when it ends before the end,
/// its last line says how much is below. `prev` is where the window was, so
/// it only moves when it must.
pub fn viewport(n: usize, h: usize, sel: usize, prev: usize) -> usize {
    if n <= h || h < 3 {
        return 0;
    }
    let mut top = prev.min(n - h);
    if sel < top + usize::from(top > 0) {
        top = sel.saturating_sub(1);
    }
    loop {
        let more = top + h < n;
        let last = top + h - 1 - usize::from(more);
        if sel <= last || top + h >= n {
            break;
        }
        top += 1;
    }
    top
}

/// Where the list last drew, so a click can find its line: the area it
/// filled, how far it was scrolled, and the key each of its lines carries —
/// `None` for the blank line a group leaves above itself.
#[derive(Default)]
pub struct ListHit {
    pub area: Rect,
    pub scroll: usize,
    pub keys: Vec<Option<String>>,
}

impl ListHit {
    pub fn has(&self, col: u16, row: u16) -> bool {
        col >= self.area.x
            && col < self.area.x + self.area.width
            && row >= self.area.y
            && row < self.area.y + self.area.height
    }
}

/// The key on the line under a pointer, if the list drew one there.
pub fn hit_row(hit: &ListHit, col: u16, row: u16) -> Option<String> {
    if !hit.has(col, row) {
        return None;
    }
    let line = hit.scroll + (row - hit.area.y) as usize;
    hit.keys.get(line).cloned().flatten()
}

/// Where the state column starts, for a title column `tw` wide — where a
/// header's checkout starts too, so the two line up.
pub fn state_col(tw: usize) -> usize {
    ICON_W + tw + 1
}

/// An icon's colour: a shell's the prompt's, a command quiet, an agent its
/// harness's own (`cli`); `ended`, quiet whatever it is.
pub fn icon_style(icon: &str, cli: Option<&str>, ended: bool) -> Style {
    if ended {
        return theme::dim();
    }
    match icon {
        SHELL => theme::accent(),
        COMMAND => theme::dim(),
        _ => match cli.and_then(|c| rows::looks().get(c)).and_then(|l| l.color) {
            Some((r, g, b)) => Style::default().fg(Color::Rgb(r, g, b)),
            None => Style::default(),
        },
    }
}

/// A row's icon, in its slot and its colour.
pub fn icon_span(icon: &str, cli: Option<&str>, ended: bool) -> Vec<Span<'static>> {
    vec![
        Span::raw(" "),
        Span::styled(icon.to_string(), icon_style(icon, cli, ended)),
        Span::raw(" "),
    ]
}

/// `spans` in at most `room` columns, the last cut with `…` where it does
/// not fit whole, and what comes after it dropped.
fn cap_spans(spans: Vec<Span<'static>>, room: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut left = room;
    for s in spans {
        let n = s.width();
        if n <= left {
            left -= n;
            out.push(s);
        } else {
            if left > 0 {
                out.push(Span::styled(fit(&s.content, left), s.style));
            }
            break;
        }
    }
    out
}

/// One row of the list, to the width: icon, title, state, whatever goes
/// before the summary (that it is hidden), the summary filling the middle,
/// then its pull request and the age at the right.
#[allow(clippy::too_many_arguments)]
pub fn list_line(
    w: usize,
    tw: usize,
    icon: Vec<Span<'static>>,
    title: Vec<Span<'static>>,
    word: Vec<Span<'static>>,
    summary: &str,
    lead: Vec<Span<'static>>,
    aside: Option<Span<'static>>,
    age: String,
) -> Line<'static> {
    let age = format!("{age:>4}");
    let aside_w = aside.as_ref().map_or(0, |a| a.width() + 2);
    let right_w = aside_w + age.chars().count();
    // What goes before the summary has the summary's room at most, so a
    // narrow list still ends in its age.
    let lead = cap_spans(lead, w.saturating_sub(state_col(tw) + WORD_W + 1 + right_w + 1));
    let before: usize = lead.iter().map(Span::width).sum();
    let fixed = state_col(tw) + WORD_W + 1 + before + right_w + 1;
    // The word, padded out to its column whatever it is made of.
    let used: usize = word.iter().map(Span::width).sum();
    let mut spans = icon;
    spans.extend(title);
    spans.push(Span::raw(" "));
    spans.extend(word);
    spans.push(Span::raw(" ".repeat(WORD_W.saturating_sub(used) + 1)));
    spans.extend(lead);
    spans.push(Span::raw(fit(summary, w.saturating_sub(fixed))).dim());
    spans.push(Span::raw(" "));
    if let Some(a) = aside {
        spans.push(a);
        spans.push(Span::raw("  "));
    }
    spans.push(Span::raw(age).dim());
    Line::from(spans)
}

/// A group's header, to the width: a project's place in the title column —
/// the folder bold, what it is in dim, the name it goes by after — or a
/// state heading's words; then the project's git checkout from the state
/// column on, and at the right edge, in amber, how many of its rows need you
/// but are off the list.
pub fn header_line(h: &Head, w: usize, tw: usize) -> Line<'static> {
    let mut l = if h.is_project() {
        let (parent, last, own) = rows::place_parts(&h.place, h.own.as_deref(), place_w(tw));
        vec![
            Span::styled(parent, theme::dim()),
            Span::styled(last, theme::name()),
            Span::styled(own, theme::dim()),
        ]
    } else {
        vec![Span::styled(
            fit(&h.name, place_w(tw)).trim_end().to_string(),
            theme::name(),
        )]
    };
    let col = state_col(tw);
    let width = |l: &[Span]| l.iter().map(Span::width).sum::<usize>();
    let right: Vec<Span<'static>> = rows::needs_hint(h.attention)
        .map(|hint| vec![Span::styled(hint, theme::needs())])
        .unwrap_or_default();
    if let Some(c) = &h.checkout {
        let at = col.max(width(&l) + 2);
        l.push(Span::raw(" ".repeat(at - width(&l))));
        let room = w.saturating_sub(at + width(&right) + usize::from(!right.is_empty()) * 2);
        let git = fit(&checkout_words(c), room);
        l.push(Span::styled(git.trim_end().to_string(), theme::dim()));
    }
    if !right.is_empty() {
        let pad = w.saturating_sub(width(&l) + width(&right)).max(1);
        l.push(Span::raw(" ".repeat(pad)));
        l.extend(right);
    }
    Line::from(l)
}

/// The room a header's place has: up to the checkout's column, and a gap.
fn place_w(tw: usize) -> usize {
    state_col(tw) - 2
}

/// A line with the search's words lit up, wherever a span holds them.
fn light(line: Line<'static>, words: &[String]) -> Line<'static> {
    let mut out = Vec::with_capacity(line.spans.len());
    for span in line.spans {
        let marks = query::marks(&span.content, words);
        if marks.is_empty() {
            out.push(span);
            continue;
        }
        let text = span.content.to_string();
        let lit = span.style.add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
        let mut at = 0;
        for (a, b) in marks {
            if a > at {
                out.push(Span::styled(text[at..a].to_string(), span.style));
            }
            out.push(Span::styled(text[a..b].to_string(), lit));
            at = b;
        }
        if at < text.len() {
            out.push(Span::styled(text[at..].to_string(), span.style));
        }
    }
    Line { spans: out, ..line }
}

/// The Relay home, for the header, when it is not the usual `~/.relay` — a
/// list on a test home should say so — with `~` for home.
pub fn home_note(home: &Path, user: Option<&Path>) -> Option<String> {
    if user.is_some_and(|u| home == u.join(".relay")) {
        return None;
    }
    Some(rl_core::display_path(home))
}

/// What the list says when it has nothing to show. When nothing is running
/// at all — no search, no toggle — the mark stands full size in the middle of
/// it with the words under it, where it fits; otherwise the words alone.
pub fn empty_list(msg: &str, area: Rect, nothing: bool) -> Paragraph<'static> {
    let words = msg.trim_start();
    let need = mark::FULL.len() as u16 + 2;
    let fits = area.height >= need && area.width as usize >= words.chars().count().max(mark::W as usize);
    if !nothing || !fits {
        return Paragraph::new(msg.to_string()).dim();
    }
    let mut lines = vec![Line::raw(""); ((area.height - need) / 2) as usize];
    lines.extend(mark::lines(&mark::FULL));
    lines.push(Line::raw(""));
    lines.push(Line::styled(words.to_string(), theme::dim()));
    Paragraph::new(lines).centered()
}

/// Where things go on screen: the header, the list, the command line (the
/// search, while one is open), the footer, and the separators between them.
pub struct Areas {
    pub head: Rect,
    pub seps: [Rect; 3],
    pub list: Rect,
    pub line: Rect,
    pub footer: Rect,
}

/// The most rows the command line stands on; a line typed with more scrolls
/// within them.
pub const LINE_MOST: usize = 5;

/// `line_h` rows for the command line (`App::line_h`), `foot_h` for the
/// footer.
pub fn areas(area: Rect, line_h: u16, foot_h: u16) -> Areas {
    let [head, sep_a, list, sep_b, line, sep_c, footer] = Layout::vertical([
        Constraint::Length(head_h(area)),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(line_h.max(1)),
        Constraint::Length(1),
        Constraint::Length(foot_h.max(1)),
    ])
    .areas(area);
    Areas {
        head,
        seps: [sep_a, sep_b, sep_c],
        list,
        line,
        footer,
    }
}

/// How tall the header is: a row of air and the small mark's rows, with the
/// words beside it, where the screen has room; one line elsewhere.
pub fn head_h(area: Rect) -> u16 {
    if area.height < TALL_H || area.width < TALL_W {
        1
    } else {
        HEAD_AIR + mark::SMALL.len() as u16
    }
}

/// A row `width` wide: `left`, cut to leave room for `right` (in the accent)
/// at its right end.
fn with_right(left: Vec<Span<'static>>, right: Option<String>, width: usize) -> Line<'static> {
    let right: Vec<Span<'static>> = right.map(|r| Span::styled(r, theme::accent())).into_iter().collect();
    let rw: usize = right.iter().map(Span::width).sum::<usize>() + 1;
    let mut left = cap_spans(left, width.saturating_sub(rw + 1));
    let lw: usize = left.iter().map(Span::width).sum();
    left.push(Span::raw(" ".repeat(width.saturating_sub(lw + rw))));
    left.extend(right);
    Line::from(left)
}

/// The headline's words, styled: the lead word in the accent, the tally plain.
fn headline_spans(title: &str) -> Vec<Span<'static>> {
    let (lead, rest) = match title.split_once(" · ") {
        Some((lead, rest)) => (lead, Some(rest)),
        None => (title, None),
    };
    let mut spans = vec![Span::styled(
        lead.to_string(),
        theme::accent().add_modifier(Modifier::BOLD),
    )];
    if let Some(rest) = rest {
        spans.push(Span::raw(format!(" · {rest}")));
    }
    spans
}

impl App {
    pub fn draw(&self, f: &mut Frame) {
        let foot = self.foot_lines(f.area().width);
        let a = areas(f.area(), self.line_h(), foot.len() as u16);
        let lines = self.lines();
        let title = rows::headline(&lines, &self.filter);
        self.draw_head(f, a.head, &title);
        for sep in a.seps {
            f.render_widget(Paragraph::new("─".repeat(sep.width as usize)).dim(), sep);
        }
        self.draw_list(f, a.list, &lines);
        f.render_widget(Paragraph::new(self.bottom_lines(a.line.width)), a.line);
        let footer = Rect {
            x: a.footer.x + TEXT_INDENT,
            width: a.footer.width.saturating_sub(TEXT_INDENT),
            ..a.footer
        };
        f.render_widget(Paragraph::new(foot), footer);
    }

    /// The header: where there is room the mark stands at the left, under a
    /// row of air, with the name, the tally and the toggles beside its middle
    /// rows; otherwise the headline on one line, the toggles at its right
    /// end, and the headline cut to leave them room.
    fn draw_head(&self, f: &mut Frame, head: Rect, title: &str) {
        let width = |t: &[Span]| t.iter().map(Span::width).sum::<usize>();
        if head.height <= 1 {
            let text = Rect {
                x: head.x + TEXT_INDENT,
                width: head.width.saturating_sub(TEXT_INDENT + 1),
                ..head
            };
            // The keys where the headline still has its room, else the icons
            // alone: the grid (`ctrl+h`) has the keys.
            let mut toggles = self.toggle_spans(true);
            if width(&toggles) + HEADLINE_ROOM > text.width as usize {
                toggles = self.toggle_spans(false);
            }
            let tw = width(&toggles);
            let room = (text.width as usize).saturating_sub(tw + 2);
            let mut spans = cap_spans(headline_spans(title), room);
            let used: usize = spans.iter().map(Span::width).sum();
            spans.push(Span::raw(" ".repeat((text.width as usize).saturating_sub(used + tw))));
            spans.extend(toggles);
            f.render_widget(Paragraph::new(Line::from(spans)), text);
            return;
        }
        let mark = Rect {
            x: head.x,
            y: head.y + HEAD_AIR,
            width: mark::W,
            height: head.height - HEAD_AIR,
        };
        f.render_widget(Paragraph::new(mark::lines(&mark::SMALL)), mark);
        let x = mark.x + mark::W + MARK_GAP;
        let text = Rect::new(x, mark.y + 1, head.right().saturating_sub(x), mark.height - 2);
        let mut toggles = self.toggle_spans(true);
        if width(&toggles) > text.width as usize {
            toggles = self.toggle_spans(false);
        }
        let mut first = vec![
            Span::styled(NAME, theme::accent().add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {}", env!("CARGO_PKG_VERSION")), theme::dim()),
        ];
        let user = std::env::var_os("HOME");
        if let Some(home) = home_note(self.home.path(), user.as_deref().map(Path::new)) {
            // Where there is no room for it, the home goes first.
            let note = Span::styled(format!(" · {home}"), theme::dim());
            let used: usize = first.iter().map(Span::width).sum();
            if used + note.width() <= text.width as usize {
                first.push(note);
            }
        }
        // The headline leads with the name, which the first line says already.
        let tally = title.strip_prefix(NAME).unwrap_or(title).trim_start_matches(" · ");
        f.render_widget(
            Paragraph::new(vec![
                Line::from(first),
                Line::raw(tally.to_string()),
                Line::from(cap_spans(toggles, text.width as usize)),
            ]),
            text,
        );
    }

    /// The toggles, for the header: a glyph and, with room (`keys`), what it
    /// is — `✦ agents`, `❯ shells`, then `◌ hidden` and the grouping, each set
    /// apart. The keys (ctrl+a, ctrl+l, ctrl+o, ctrl+s) are in the help grid.
    /// On is accented and bold, off is quiet.
    fn toggle_spans(&self, keys: bool) -> Vec<Span<'static>> {
        let on = theme::accent().add_modifier(Modifier::BOLD);
        let off = theme::dim();
        let lit = |b: bool| if b { on } else { off };
        let p = self.prefs();
        let grouping = match p.grouping {
            Grouping::Project => "by project",
            Grouping::State => "by state",
        };
        let k = |icon: &str, key: &str| {
            if keys {
                format!("{icon} {key}")
            } else {
                icon.to_string()
            }
        };
        vec![
            Span::styled(k("✦", "agents"), lit(p.show_agents)),
            Span::styled("  ", off),
            Span::styled(k("❯", "shells"), lit(p.show_shells)),
            // Hidden is a different axis — what you hid, not a kind — and the
            // grouping another again, so a divider sets each apart.
            Span::styled("  │  ", off),
            Span::styled(k("◌", "hidden"), lit(p.show_hidden)),
            Span::styled("  │  ", off),
            Span::styled(grouping, theme::accent()),
        ]
    }

    /// How many rows the command line stands on: one for each line typed
    /// (`shift+enter`), up to `LINE_MOST`; one for the search.
    pub fn line_h(&self) -> u16 {
        if self.search.is_some() {
            return 1;
        }
        self.cmd.line.text().split('\n').count().clamp(1, LINE_MOST) as u16
    }

    /// The rows over the footer: the command line, with at the right of its
    /// first row what it would start; or, while one is open, the search in
    /// its place.
    fn bottom_lines(&self, width: u16) -> Vec<Line<'static>> {
        let width = width as usize;
        let Some(s) = &self.search else {
            let label = self.detected().map(|d| d.label());
            return self
                .command_rows()
                .into_iter()
                .enumerate()
                .map(|(i, row)| {
                    let right = label.clone().filter(|_| i == 0);
                    with_right(row, right, width)
                })
                .collect();
        };
        let mut spans = vec![Span::raw(" "), Span::styled("⌕ ", theme::accent())];
        spans.extend(theme::with_caret(s.line.text(), s.line.cursor(), Style::default()));
        if s.line.is_empty() {
            spans.push(Span::styled(" words · in: is: kind: on:", theme::dim()));
        }
        vec![with_right(spans, None, width)]
    }

    /// The command line's rows: the prompt, then what is typed, the caret,
    /// and the best completion dim after it — a row for each line typed,
    /// the prompt on the first only (`⋮` where lines above are out of
    /// sight: past `LINE_MOST` the rows follow the cursor). Empty, it says
    /// what it is for, or the filter in force.
    fn command_rows(&self) -> Vec<Vec<Span<'static>>> {
        let l = &self.cmd.line;
        let (text, cur) = (l.text(), l.cursor());
        let starts: Vec<usize> = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let at = starts.iter().rposition(|&s| s <= cur).unwrap_or(0);
        let top = (at + 1).saturating_sub(LINE_MOST);
        let mut out = Vec::new();
        for (i, &from) in starts.iter().enumerate().skip(top).take(LINE_MOST) {
            let to = starts.get(i + 1).map_or(text.len(), |&n| n - 1);
            let seg = &text[from..to];
            let mut spans = match i {
                0 => vec![Span::raw(" "), Span::styled("❯ ", theme::accent())],
                _ if i == top => vec![Span::styled(" ⋮ ", theme::dim())],
                _ => vec![Span::raw("   ")],
            };
            if i != at {
                spans.push(Span::raw(seg.to_string()));
            } else {
                match self.cmd.ghost.as_deref() {
                    Some(g) if cur == text.len() && !g.is_empty() => {
                        let mut chars = g.chars();
                        let first = chars.next().map(String::from).unwrap_or_default();
                        spans.push(Span::raw(seg.to_string()));
                        spans.push(Span::styled(first, theme::dim().add_modifier(Modifier::REVERSED)));
                        spans.push(Span::styled(chars.as_str().to_string(), theme::dim()));
                    }
                    _ => spans.extend(theme::with_caret(seg, cur - from, Style::default())),
                }
            }
            out.push(spans);
        }
        if l.is_empty() {
            let hint = if !self.filter.is_empty() {
                format!(" ⌕ {} · esc clears", self.filter)
            } else if cmdline::default_agent(rows::registry(), &self.prefs().default).is_some() {
                format!(
                    " a prompt for {}, or claude/codex/agy and a prompt, ! for a shell · / for commands",
                    self.prefs().default
                )
            } else {
                " a command, or claude/codex/agy and a prompt · / for commands · ctrl+f to find".to_string()
            };
            out[0].push(Span::styled(hint, theme::dim()));
        }
        out
    }

    /// The footer's rows at this width: the grid of keys where it is up,
    /// under a flash if there is one, else the one row of hints.
    pub fn foot_lines(&self, width: u16) -> Vec<Line<'static>> {
        let room = width.saturating_sub(TEXT_INDENT).max(1) as usize;
        // A word of several lines (what `/setup` did) stands on as many rows.
        let flash = || -> Vec<Line<'static>> {
            self.flashing()
                .into_iter()
                .flat_map(str::lines)
                .map(|l| Line::from(l.to_string()).dim())
                .collect()
        };
        if !self.help || self.search.is_some() {
            let f = flash();
            if f.len() > 1 {
                return f;
            }
            return vec![Line::from(self.footer(room)).dim()];
        }
        flash().into_iter().chain(help::grid(room)).collect()
    }

    /// The footer's one row: a word that stands for a moment, or the search's
    /// keys while it is typed, or the hints — entries of the grid's, so they
    /// say what it does — with a pointer to the grid where there is room.
    fn footer(&self, width: usize) -> String {
        // A choice asked stands over any word of the moment: it has the keys.
        if let Mode::Ask(a) = &self.mode {
            return a.offer();
        }
        if let Some(m) = self.flashing() {
            return m.to_string();
        }
        if matches!(self.mode, Mode::Rename(_)) {
            return "type a name · enter keeps it · empty goes back to its own · esc cancels".into();
        }
        if self.search.is_some() {
            return "type to narrow · tab completes in: is: kind: on:, or next match · ↑↓ move · \
                    enter keeps · esc puts back"
                .into();
        }
        if super::slash::is_command(self.cmd.line.text()) {
            return "enter runs it on the row · tab completes · ↑↓ picks the row · esc clears".into();
        }
        if !self.cmd.is_empty() {
            return "enter starts · shift+enter adds a line · tab completes · ↑↓ picks the row, and so the folder · esc clears"
                .into();
        }
        if let Some(why) = self.native_failing() {
            return why;
        }
        let by = match self.prefs().grouping {
            Grouping::Project => "to group by state",
            Grouping::State => "to group by project",
        };
        let mut hints = vec![
            help::hint(help::Id::Enter, None),
            help::hint(help::Id::Remove, Some(&self.x_hint())),
            help::hint(help::Id::Find, None),
            help::hint(help::Id::Commands, None),
        ];
        if !self.filter.is_empty() {
            hints.insert(1, "esc to clear the search".into());
        }
        hints.push(help::hint(help::Id::Group, Some(by)));
        if let Some(n) = self.setup_notice() {
            hints.insert(0, n);
        }
        if let Some(n) = self.drift_notice() {
            hints.insert(0, n);
        }
        help::footer(&hints, width)
    }

    /// A harness's lister that did not answer, while that is its last word:
    /// what went wrong, and from when what its rows show is.
    fn native_failing(&self) -> Option<String> {
        let f = self.native.failing.first()?;
        let shown = match f.since {
            Some(at) => format!(
                "showing {}",
                at.to_zoned(jiff::tz::TimeZone::system()).strftime("%H:%M")
            ),
            None => format!("nothing of {}'s listed yet", f.harness),
        };
        Some(format!("{} · {shown}", f.error.replace('`', "")))
    }

    /// What `^x` does to the line under the cursor, in the hints' words.
    fn x_hint(&self) -> String {
        let hidden = |key: &str| self.prefs().hidden.iter().any(|h| h.key == key);
        match self.selected() {
            Some(ListLine::Session(r)) => match r.source {
                Source::Held if r.state.is_terminal() || r.state == RunState::Stale => "to remove".into(),
                Source::Held => "to stop".into(),
                Source::Native if hidden(&r.key) => "to bring back".into(),
                Source::Native => match (&r.stop, &r.close) {
                    (Some(_), Some(c)) => format!("to stop or {}", c.words),
                    (None, Some(c)) => format!("to {}", c.words),
                    (Some(_), None) => "to stop".into(),
                    (None, None) => "to hide".into(),
                },
                _ => "to hide".into(),
            },
            _ => "to hide".into(),
        }
    }

    fn draw_list(&self, f: &mut Frame, area: Rect, lines: &[ListLine]) {
        self.list_h.set(area.height);
        // Cleared here and filled in once the window is known, so a click can
        // never land on a map left behind by a draw that returned early.
        *self.hit.borrow_mut() = ListHit::default();
        if lines.is_empty() {
            let p = self.prefs();
            let msg: String = if !self.filter.is_empty() {
                " nothing matches".into()
            } else if !p.show_agents || !p.show_shells {
                " nothing here · ctrl+a agents · ctrl+l shells · ctrl+o hidden".into()
            } else if p.show_hidden {
                " nothing hidden".into()
            } else {
                " nothing running · type to start one".into()
            };
            let nothing = self.filter.is_empty() && p.show_agents && p.show_shells && !p.show_hidden;
            f.render_widget(empty_list(&msg, area, nothing), area);
            return;
        }
        self.draw_window(f, area, lines);
    }

    /// The list's lines `w` wide, with a blank line above every group but the
    /// first: which line each place shows (`None`, a blank), the key each
    /// carries (for the mouse), and the selected line's place.
    fn places(&self, lines: &[ListLine]) -> (Vec<Option<usize>>, Vec<Option<String>>, usize) {
        let mut places = Vec::with_capacity(lines.len());
        let mut keys = Vec::with_capacity(lines.len());
        let mut sel_line = 0usize;
        for (n, line) in lines.iter().enumerate() {
            if n > 0 && matches!(line, ListLine::Group(_)) {
                places.push(None);
                keys.push(None);
            }
            let key = line.key();
            if self.sel.as_deref() == Some(key.as_str()) {
                sel_line = places.len();
            }
            places.push(Some(n));
            keys.push(Some(key));
        }
        (places, keys, sel_line)
    }

    /// One line of the list, drawn `w` wide.
    fn row_line(&self, line: &ListLine, w: usize, cx: &RowCx) -> Line<'static> {
        let tw = cx.tw;
        let key = line.key();
        let mut drawn = match line {
            ListLine::Group(h) => header_line(h, w, tw),
            ListLine::Session(r) => self.session_line(r, &cx.names, w, tw, cx.now),
        };
        // A name being typed stands where the name goes, the caret in it.
        if let Mode::Rename(rn) = &self.mode
            && rn.key == key
        {
            let mut spans = match line {
                ListLine::Session(r) => icon_span(rows::icon(r), Some(&r.harness), r.state.is_terminal()),
                ListLine::Group(_) => vec![],
            };
            let room = w.saturating_sub(spans.iter().map(Span::width).sum());
            spans.extend(cap_spans(
                theme::with_selection(rn.line.text(), rn.line.cursor(), rn.line.selection(), Style::default()),
                room,
            ));
            drawn = Line::from(spans);
        }
        if !cx.words.is_empty() {
            drawn = light(drawn, &cx.words);
        }
        if self.sel.as_deref() == Some(key.as_str()) {
            // The highlight runs to the edge, however little the row says.
            let short = w.saturating_sub(drawn.width());
            if short > 0 {
                drawn.spans.push(Span::raw(" ".repeat(short)));
            }
            drawn = drawn.style(theme::selected());
        }
        drawn
    }

    /// A session's row. One standing outside its project — pinned, come
    /// through, or grouped by state — says which project it is from; one
    /// that came through says it is hidden. A shell at rest says where it is
    /// in its project, where it says nothing else.
    fn session_line(&self, r: &Row, names: &Names, w: usize, tw: usize, now: Timestamp) -> Line<'static> {
        let p = self.prefs();
        let project = rows::project_of(r, names);
        let through = !p.show_hidden && rows::hidden_since(r, &project, &p.hidden).is_some();
        let away = through || p.pins.contains(&r.key) || p.grouping == Grouping::State;
        let title = if away {
            format!("{} › {}", names.workspace(&project), names.title(r))
        } else {
            names.title(r).to_string()
        };
        let place = (rows::live_shell(r) && r.summary.is_none()).then(|| rows::shell_place(&r.cwd, &project));
        let summary = place.as_deref().or(r.summary.as_deref()).unwrap_or("");
        // `#N`, coloured by how it stands where `gh` said.
        let pr = r.pr.as_ref().map(|pr| {
            Span::styled(
                format!("#{}", pr.number),
                theme::pr(self.prs.get(&pr.href).map(String::as_str)),
            )
        });
        let word = fit(&r.words, WORD_W).trim_end().to_string();
        // The harness at the end of the title's column, quiet: the state
        // column stays where a header's checkout starts.
        let harness = HARNESS_W.min(tw / 3);
        // The colour it picked in its harness, as a swatch behind the name
        // alone, as `claude agents` has it; the icon keeps its state's colour.
        // Under the cursor the band stays, and the swatch stays on the name.
        let icon = icon_span(rows::icon(r), Some(&r.harness), r.state.is_terminal());
        let fitted = fit(&title, tw - harness);
        let text = fitted.trim_end();
        let mut name = vec![Span::raw(text.to_string())];
        if let Some(c) = theme::effective(r.color.as_deref(), p.colors.get(&r.key).map(String::as_str)) {
            name[0].style = theme::swatch(c);
        }
        let mut room = fitted.chars().count() - text.chars().count();
        // Your name for one that is not Relay's: the harness's own beside it,
        // dim, cut before your name is. Where it does not fit, left out.
        let own = (r.source != Source::Held && !r.name.is_empty() && r.name != names.title(r)).then_some(&r.name);
        if let Some(own) = own
            && text.chars().count() == title.chars().count()
            && room >= 6
        {
            let beside = fit(&format!("  ({own})"), room);
            let beside = beside.trim_end();
            room -= beside.chars().count();
            name.push(Span::styled(beside.to_string(), theme::dim()));
        }
        name.push(Span::raw(" ".repeat(room)));
        name.push(Span::styled(format!(" {}", fit(&r.harness, harness - 1)), theme::dim()));
        list_line(
            w,
            tw,
            icon,
            name,
            vec![Span::styled(word, theme::state(r.state))],
            summary,
            if through {
                vec![Span::raw("hidden  ").dim()]
            } else {
                vec![]
            },
            pr,
            r.started
                .or(r.state_at)
                .map(|t| fmt_age(t, r.ended.unwrap_or(now)))
                .unwrap_or_default(),
        )
    }

    /// Lines in the list's place, a window of them following the selection;
    /// when there is more either side, the edge line says how much. Only
    /// what the window shows is drawn.
    fn draw_window(&self, f: &mut Frame, area: Rect, lines: &[ListLine]) {
        let h = area.height as usize;
        if h == 0 {
            return;
        }
        let (places, mut keys, sel_line) = self.places(lines);
        let n = places.len();
        let top = viewport(n, h, sel_line, self.scroll.get());
        self.scroll.set(top);
        let more_below = top + h < n;
        let w = area.width as usize;
        let cx = RowCx {
            // Columns: icon, title, state, the summary to the edge, the pull
            // request and age at the right — the title column is the
            // screen's share, bounded.
            tw: (w * 35 / 100).clamp(14, 44),
            now: Timestamp::now(),
            // What the search's words hit is lit up where it shows.
            words: query::Query::parse(&self.filter).words,
            names: Names::new(&self.prefs().names),
        };
        let mut shown: Vec<Line> = places[top..(top + h).min(n)]
            .iter()
            .map(|p| match p {
                Some(i) => self.row_line(&lines[*i], w, &cx),
                None => Line::raw(""),
            })
            .collect();
        if top > 0 {
            shown[0] = Line::from(Span::raw(format!(" ▲ {top} more")).dim());
            // An edge line gave its place to a count, so it carries no line.
            keys[top] = None;
        }
        if more_below {
            let left = n - (top + h);
            let i = shown.len() - 1;
            shown[i] = Line::from(Span::raw(format!(" ▼ {left} more")).dim());
            keys[top + i] = None;
        }
        f.render_widget(Paragraph::new(shown), area);
        *self.hit.borrow_mut() = ListHit {
            area,
            scroll: top,
            keys,
        };
    }
}

/// What every row drawn at once shares.
struct RowCx<'a> {
    tw: usize,
    now: Timestamp,
    words: Vec<String>,
    names: Names<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::Checkout;

    #[test]
    fn a_click_finds_the_line_it_fell_on() {
        let hit = ListHit {
            area: Rect::new(2, 3, 20, 4),
            scroll: 1,
            keys: vec![
                Some("r:one".into()), // scrolled out of sight
                Some("r:two".into()), // the top line drawn
                None,                 // the blank a group leaves above itself
                Some("g:three".into()),
            ],
        };
        assert_eq!(hit_row(&hit, 5, 3), Some("r:two".into()));
        assert_eq!(hit_row(&hit, 5, 4), None, "the blank above a group owns no line");
        assert_eq!(hit_row(&hit, 5, 5), Some("g:three".into()));
        // Off the list on every side.
        assert_eq!(hit_row(&hit, 1, 3), None);
        assert_eq!(hit_row(&hit, 22, 3), None);
        assert_eq!(hit_row(&hit, 5, 2), None);
        assert_eq!(hit_row(&hit, 5, 7), None);
        // Inside the area, but past the last line the list drew.
        assert_eq!(hit_row(&hit, 5, 6), None);
    }

    #[test]
    fn the_header_stands_tall_where_there_is_room_and_is_one_line_elsewhere() {
        let screen = |w, h| Rect::new(0, 0, w, h);
        assert_eq!(head_h(screen(160, 45)), 6);
        assert_eq!(head_h(screen(80, 24)), 6);
        assert_eq!(head_h(screen(160, 23)), 1);
        assert_eq!(head_h(screen(79, 45)), 1);
        // The list starts under the header and its separator, whichever it is,
        // and the command line stands between it and the footer.
        let a = areas(screen(100, 30), 1, 1);
        assert_eq!((a.list.y, a.line.y, a.footer.y), (7, 27, 29));
        let a = areas(screen(80, 20), 1, 1);
        assert_eq!((a.list.y, a.line.y, a.footer.y), (2, 17, 19));
    }

    #[test]
    fn the_home_is_said_only_when_it_is_not_the_usual_one() {
        let user = Some(Path::new("/Users/me"));
        assert_eq!(home_note(Path::new("/Users/me/.relay"), user), None);
        assert_eq!(home_note(Path::new("/tmp/x"), user), Some("/tmp/x".into()));
        assert_eq!(home_note(Path::new("/tmp/x"), None), Some("/tmp/x".into()));
    }

    #[test]
    fn an_empty_list_shows_the_mark_where_it_fits() {
        use ratatui::buffer::Buffer;
        use ratatui::widgets::Widget;
        let shown = |nothing: bool, w: u16, h: u16| {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            empty_list(" nothing running · rl new", area, nothing).render(area, &mut buf);
            (0..h)
                .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let big = shown(true, 60, 20);
        assert!(big.contains("██▀▀████▀▀██"), "{big}");
        assert!(big.contains("nothing running"));
        // Too small for it, or a toggle or a search that came up empty: words.
        for s in [shown(true, 60, 9), shown(true, 22, 20), shown(false, 60, 20)] {
            assert!(!s.contains('█'), "{s}");
            assert!(s.starts_with(" nothing running"), "{s}");
        }
    }

    fn col_of(line: &Line, text: &str) -> usize {
        let s: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let at = s.find(text).unwrap_or_else(|| panic!("{text:?} not in {s:?}"));
        s[..at].chars().map(|c| Span::raw(c.to_string()).width()).sum()
    }

    #[test]
    fn headers_and_rows_keep_their_columns_whatever_the_lengths() {
        let (w, tw) = (100, 30);
        let co = |b: &str, n| Checkout {
            path: "/w".into(),
            branch: Some(b.into()),
            detached: None,
            worktrees: n,
            badges: vec![],
        };
        let head = |place: &str, own: Option<&str>, checkout: Option<Checkout>| Head {
            key: "g:/w".into(),
            place: place.into(),
            workspace: "/w".into(),
            name: "w".into(),
            own: own.map(Into::into),
            attention: 0,
            heading: None,
            checkout: checkout.map(Box::new),
        };
        let a = header_line(&head("~/Projects/flint-platform", None, Some(co("main", 24))), w, tw);
        let b = header_line(&head("~/tmp/foodai", Some("food"), Some(co("a-long-branch", 4))), w, tw);
        let c = header_line(
            &head(
                "~/Downloads/some/very/deep/folder/Finance & Real Estate",
                None,
                Some(co("main", 0)),
            ),
            w,
            tw,
        );
        // The checkout starts in the state column on every header.
        for l in [&a, &b, &c] {
            assert_eq!(col_of(l, "⎇"), state_col(tw), "{l}");
        }
        assert_eq!(col_of(&b, "tmp/"), 2, "the parent, then the folder");
        assert!(b.to_string().contains("foodai · food"));
        assert!(a.to_string().contains("⎇ main · 24 worktrees"), "{a}");
        // A long path lost its start and kept its folder.
        assert!(c.to_string().starts_with("…/"), "{c}");
        assert!(c.to_string().contains("Finance & Real Estate"));

        // Something that needs you is off the list: the header hints in amber,
        // at the edge.
        let mut hinted = head("~/tmp/foodai", None, Some(co("main", 0)));
        hinted.attention = 2;
        let line = header_line(&hinted, w, tw);
        assert!(line.to_string().ends_with("! 2 need you"), "{line}");
        assert_eq!(line.width(), w);
        let amber = line.spans.iter().find(|s| s.content == "! 2 need you").unwrap();
        assert_eq!(amber.style, theme::needs());

        // A heading, grouped by state, says its words where a folder goes.
        let heading = header_line(&Head::heading(rows::Heading::NeedsInput, 1), w, tw);
        assert!(heading.to_string().starts_with("Needs input"), "{heading}");
        assert!(heading.to_string().ends_with("! 1 needs you"), "{heading}");

        // A row's state word starts where a header's checkout does, and every
        // title in the same column, whatever icon it wears.
        let row = |icon: &str, word: &str| {
            list_line(
                w,
                tw,
                icon_span(icon, None, false),
                vec![Span::raw(fit("title", tw))],
                vec![Span::raw(word.to_string())],
                "summary",
                vec![],
                None,
                "2m".into(),
            )
        };
        for (icon, word) in [(rows::AGENT, "needs you"), (rows::SHELL, ""), (rows::COMMAND, "done")] {
            let l = row(icon, word);
            assert_eq!(col_of(&l, "title"), 3, "{l}");
            assert_eq!(col_of(&l, "summary"), state_col(tw) + WORD_W + 1, "{l}");
            assert_eq!(l.width(), w, "{l}");
        }
        assert_eq!(col_of(&row(rows::AGENT, "needs you"), "needs"), state_col(tw));
    }

    #[test]
    fn the_window_keeps_the_selection_clear_of_the_edge_markers() {
        // Everything fits: no window.
        assert_eq!(viewport(10, 20, 9, 0), 0);
        // Selection at the top: no marker above, so line 0 shows.
        assert_eq!(viewport(50, 10, 0, 0), 0);
        // Down to the last usable line (the last of the window is the marker).
        assert_eq!(viewport(50, 10, 8, 0), 0);
        assert_eq!(viewport(50, 10, 9, 0), 1, "top 1 shows 1..=10; 1 and 10 are markers");
        assert_eq!(viewport(50, 10, 10, 1), 2);
        // The window stays put while the selection is inside it.
        assert_eq!(viewport(50, 10, 5, 2), 2);
        // Up under the top marker pulls the window up.
        assert_eq!(viewport(50, 10, 2, 2), 1);
        assert_eq!(viewport(50, 10, 1, 1), 0);
        // At the very end there is no bottom marker, so the last line is usable.
        assert_eq!(viewport(50, 10, 49, 0), 40);
        assert_eq!(viewport(50, 10, 41, 40), 40);
        // Too short for markers: just the lines.
        assert_eq!(viewport(50, 2, 30, 0), 0);
    }
}
