//! Drawing the list: the header, the rows under their headers, the toggles
//! and the search on their line, and the footer. Colour carries meaning — a
//! saturated accent per state, on the terminal's own foreground so it reads
//! on either theme (see `theme`). Nothing on screen is an id.

use super::app::{App, Mode};
use super::{mark, shortcuts, theme};
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

/// Where things go on screen: the header, the list, the toggles and search
/// line, the footer, and the separators between them.
pub struct Areas {
    pub head: Rect,
    pub seps: [Rect; 3],
    pub list: Rect,
    pub toggles: Rect,
    pub footer: Rect,
}

pub fn areas(area: Rect, foot_h: u16) -> Areas {
    let [head, sep_a, list, sep_b, toggles, sep_c, footer] = Layout::vertical([
        Constraint::Length(head_h(area)),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(foot_h.max(1)),
    ])
    .areas(area);
    Areas {
        head,
        seps: [sep_a, sep_b, sep_c],
        list,
        toggles,
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
        let a = areas(f.area(), foot.len() as u16);
        let lines = self.lines();
        let title = rows::headline(&lines, &self.filter);
        self.draw_head(f, a.head, &title);
        for sep in a.seps {
            f.render_widget(Paragraph::new("─".repeat(sep.width as usize)).dim(), sep);
        }
        self.draw_list(f, a.list, &lines);
        f.render_widget(Paragraph::new(self.toggles_line(a.toggles.width)), a.toggles);
        let footer = Rect {
            x: a.footer.x + TEXT_INDENT,
            width: a.footer.width.saturating_sub(TEXT_INDENT),
            ..a.footer
        };
        f.render_widget(Paragraph::new(foot), footer);
    }

    /// The header: where there is room the mark stands at the left, under a
    /// row of air, with the name and the tally beside its middle rows;
    /// otherwise the headline alone, on one line.
    fn draw_head(&self, f: &mut Frame, head: Rect, title: &str) {
        if head.height <= 1 {
            let text = Rect {
                x: head.x + TEXT_INDENT,
                width: head.width.saturating_sub(TEXT_INDENT),
                ..head
            };
            f.render_widget(Paragraph::new(Line::from(headline_spans(title))), text);
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
        let mut first = vec![
            Span::styled(NAME, theme::accent().add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {}", env!("CARGO_PKG_VERSION")), theme::dim()),
        ];
        let user = std::env::var_os("HOME");
        if let Some(home) = home_note(self.home.path(), user.as_deref().map(Path::new)) {
            first.push(Span::styled(format!(" · {home}"), theme::dim()));
        }
        // The headline leads with the name, which the first line says already.
        let tally = title.strip_prefix(NAME).unwrap_or(title).trim_start_matches(" · ");
        f.render_widget(
            Paragraph::new(vec![Line::from(first), Line::raw(tally.to_string())]),
            text,
        );
    }

    /// The toggles line: the search being typed, or the filter in force, at
    /// the left; at the right the toggles — a glyph and its key for each kind,
    /// `✦a` agents, `❯s` shells, then `◌h` hidden and the grouping, each set
    /// apart. On is accented and bold, off is quiet.
    fn toggles_line(&self, width: u16) -> Line<'static> {
        let on = theme::accent().add_modifier(Modifier::BOLD);
        let off = theme::dim();
        let lit = |b: bool| if b { on } else { off };
        let p = self.prefs();
        let grouping = match p.grouping {
            Grouping::Project => "by project",
            Grouping::State => "by state",
        };
        let mut right = Vec::new();
        if self.search.is_none()
            && let Some(d) = self.detected()
        {
            right.push(Span::styled(d.label(), theme::accent()));
            right.push(Span::styled("  │  ", off));
        }
        right.extend([
            Span::styled("✦a", lit(p.show_agents)),
            Span::styled("  ", off),
            Span::styled("❯s", lit(p.show_shells)),
            // Hidden is a different axis — what you hid, not a kind — and the
            // grouping another again, so a divider sets each apart.
            Span::styled("  │  ", off),
            Span::styled("◌h", lit(p.show_hidden)),
            Span::styled("  │  ", off),
            Span::styled(grouping, theme::accent()),
            Span::styled(" ^s", off),
        ]);
        let mut left = match &self.search {
            Some(s) => {
                let mut spans = vec![Span::raw(" "), Span::styled("⌕ ", theme::accent())];
                spans.extend(theme::with_caret(s.line.text(), s.line.cursor(), Style::default()));
                if s.line.is_empty() {
                    spans.push(Span::styled(" words · in: is: kind: on:", theme::dim()));
                }
                spans
            }
            None => self.command_spans(),
        };
        let width = width as usize;
        let rw: usize = right.iter().map(Span::width).sum::<usize>() + 1;
        let left_room = width.saturating_sub(rw + 1);
        left = cap_spans(left, left_room);
        let lw: usize = left.iter().map(Span::width).sum();
        left.push(Span::raw(" ".repeat(width.saturating_sub(lw + rw))));
        left.extend(right);
        Line::from(left)
    }

    /// The command line: the folder it starts in, short, then what is
    /// typed, the caret, and the best completion dim after it. Empty, it
    /// says what it is for, or the filter in force.
    fn command_spans(&self) -> Vec<Span<'static>> {
        let folder = self.line_folder();
        let place = match folder.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => rows::short_path(&folder),
        };
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(place, theme::dim()),
            Span::styled(" ❯ ", theme::accent()),
        ];
        let l = &self.cmd.line;
        match self.cmd.ghost.as_deref() {
            Some(g) if l.cursor() == l.text().len() && !g.is_empty() => {
                let mut chars = g.chars();
                let first = chars.next().map(String::from).unwrap_or_default();
                spans.push(Span::raw(l.text().to_string()));
                spans.push(Span::styled(first, theme::dim().add_modifier(Modifier::REVERSED)));
                spans.push(Span::styled(chars.as_str().to_string(), theme::dim()));
            }
            _ => spans.extend(theme::with_caret(l.text(), l.cursor(), Style::default())),
        }
        if l.is_empty() {
            let hint = if self.filter.is_empty() {
                " a command, or claude/codex/agy and a prompt · / searches".to_string()
            } else {
                format!(" ⌕ {} · esc clears", self.filter)
            };
            spans.push(Span::styled(hint, theme::dim()));
        }
        spans
    }

    /// The footer's rows at this width: the shortcuts where they are up,
    /// under a flash if there is one, else the one row of hints.
    pub fn foot_lines(&self, width: u16) -> Vec<Line<'static>> {
        let room = width.saturating_sub(TEXT_INDENT).max(1) as usize;
        if !self.shortcuts || self.search.is_some() {
            return vec![Line::from(self.footer(room)).dim()];
        }
        let keys = shortcuts::lines(&shortcuts::keys(shortcuts::Place::List), room);
        let flash = self.flashing().map(|m| Line::from(m.to_string()).dim());
        flash.into_iter().chain(keys).collect()
    }

    /// The footer's one row: a word that stands for a moment, or the search's
    /// keys while it is typed, or the hints, with a pointer to the shortcuts
    /// where there is room.
    fn footer(&self, width: usize) -> String {
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
        if !self.cmd.is_empty() {
            return "enter starts · tab completes · ↑↓ picks the folder · esc clears".into();
        }
        if let Some(why) = self.native_failing() {
            return why;
        }
        let by = match self.prefs().grouping {
            Grouping::Project => "^s by state",
            Grouping::State => "^s by project",
        };
        let mut hints = vec!["enter attaches", "type to start", self.x_hint(), "/ search"];
        if !self.filter.is_empty() {
            hints.push("esc clears the search");
        }
        hints.push(by);
        shortcuts::with_hint(hints.join(" · "), width)
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
    fn x_hint(&self) -> &'static str {
        match self.selected() {
            Some(ListLine::Session(r)) => match r.source {
                Source::Held if r.state.is_terminal() || r.state == RunState::Stale => "^x remove",
                Source::Held => "^x stop",
                Source::Native if r.stop.is_some() => "^x stop",
                _ => "^x hide",
            },
            _ => "^x hide",
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
                " nothing here · a agents · s shells · h hidden".into()
            } else if p.show_hidden {
                " nothing hidden".into()
            } else {
                " nothing running · type to start one".into()
            };
            let nothing = self.filter.is_empty() && p.show_agents && p.show_shells && !p.show_hidden;
            f.render_widget(empty_list(&msg, area, nothing), area);
            return;
        }
        let (shown, keys, sel_line) = self.row_lines(lines, area.width as usize);
        self.draw_window(f, area, shown, keys, sel_line);
    }

    /// The list's lines `w` wide, with a blank line above every group but the
    /// first: the lines, the key each line belongs to (for the mouse), and
    /// the selected line's place.
    fn row_lines(&self, lines: &[ListLine], w: usize) -> (Vec<Line<'static>>, Vec<Option<String>>, usize) {
        let now = Timestamp::now();
        // Columns: icon, title, state, the summary to the edge, the pull
        // request and age at the right — the title column is the screen's
        // share, bounded.
        let tw = (w * 35 / 100).clamp(14, 44);
        let mut out: Vec<Line> = Vec::with_capacity(lines.len());
        let mut keys: Vec<Option<String>> = Vec::with_capacity(lines.len());
        let mut sel_line = 0usize;
        // What the search's words hit is lit up where it shows.
        let words = query::Query::parse(&self.filter).words;
        let names = Names::new(&self.prefs().names);
        for (n, line) in lines.iter().enumerate() {
            let key = line.key();
            let mut drawn = match line {
                ListLine::Group(h) => {
                    if n > 0 {
                        out.push(Line::raw(""));
                        keys.push(None);
                    }
                    header_line(h, w, tw)
                }
                ListLine::Session(r) => self.session_line(r, &names, w, tw, now),
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
                    theme::with_caret(rn.line.text(), rn.line.cursor(), Style::default()),
                    room,
                ));
                drawn = Line::from(spans);
            }
            if !words.is_empty() {
                drawn = light(drawn, &words);
            }
            if self.sel.as_deref() == Some(key.as_str()) {
                // The highlight runs to the edge, however little the row says.
                let short = w.saturating_sub(drawn.width());
                if short > 0 {
                    drawn.spans.push(Span::raw(" ".repeat(short)));
                }
                drawn = drawn.style(theme::selected());
                sel_line = out.len();
            }
            out.push(drawn);
            keys.push(Some(key));
        }
        (out, keys, sel_line)
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
        // The colour it picked in its harness, on its name and its icon —
        // not under the cursor, whose band wins.
        let picked = r
            .color
            .as_deref()
            .and_then(theme::picked)
            .filter(|_| self.sel.as_deref() != Some(r.key.as_str()));
        let mut icon = icon_span(rows::icon(r), Some(&r.harness), r.state.is_terminal());
        let mut name = Style::default();
        if let Some(c) = picked {
            icon[1].style = icon[1].style.fg(c);
            name = name.fg(c);
        }
        list_line(
            w,
            tw,
            icon,
            vec![
                Span::styled(fit(&title, tw - harness), name),
                Span::styled(format!(" {}", fit(&r.harness, harness - 1)), theme::dim()),
            ],
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
    /// when there is more either side, the edge line says how much. `keys` is
    /// in step with `lines`: the key each belongs to, for the mouse.
    fn draw_window(
        &self,
        f: &mut Frame,
        area: Rect,
        lines: Vec<Line<'static>>,
        mut keys: Vec<Option<String>>,
        sel_line: usize,
    ) {
        let h = area.height as usize;
        let n = lines.len();
        if h == 0 {
            return;
        }
        let top = viewport(n, h, sel_line, self.scroll.get());
        self.scroll.set(top);
        let more_below = top + h < n;
        let mut shown: Vec<Line> = lines[top..(top + h).min(n)].to_vec();
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
        // and the toggles stand between it and the footer.
        let a = areas(screen(100, 30), 1);
        assert_eq!((a.list.y, a.toggles.y, a.footer.y), (7, 27, 29));
        let a = areas(screen(80, 20), 1);
        assert_eq!((a.list.y, a.toggles.y, a.footer.y), (2, 17, 19));
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
