//! What the wizard shows: the list's mark and name over each step, the
//! step itself, and its keys at the foot; and the summary it leaves behind.
//! The list's colours and glyphs: each harness's icon in its colour, the
//! accent for what is Relay's, quiet text dimmed.

use super::model::{Came, Ended, Harn, Job, Run, STEPS, Step, Wizard};
use crate::guide::{EXAMPLES, MESSAGING_GUIDE};
use crate::rows::fit;
use crate::tui::{help, mark, theme};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

/// The one line under the name.
pub const TAGLINE: &str = "One list for every agent. Let's connect your harnesses.";
/// Where the guide's link is, in words.
pub const LINK_TEXT: &str = "messaging guide ↗";
/// The mark stands beside the words this wide and up.
const WIDE: u16 = 80;
/// Text sits this far in.
const INDENT: u16 = 2;
/// The spinner's frames.
const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn w(s: &str) -> usize {
    s.width()
}

fn spans_w(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// A harness's icon, in its colour; quiet where it is not installed.
fn icon(h: &Harn) -> Span<'static> {
    let style = match (h.found, h.color) {
        (false, _) => theme::dim(),
        (true, Some((r, g, b))) => Style::default().fg(Color::Rgb(r, g, b)),
        (true, None) => Style::default(),
    };
    Span::styled(h.icon.clone(), style)
}

fn bad() -> Style {
    Style::default().fg(theme::BAD)
}

fn ok() -> Style {
    theme::accent()
}

/// `text` in lines at most `width` wide, broken between words (a word
/// longer than that, between its characters).
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if w(&candidate) <= width {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            out.push(std::mem::take(&mut line));
        }
        line = word.to_string();
        while w(&line) > width {
            let mut used = 0;
            let cut = line
                .char_indices()
                .find(|(_, c)| {
                    used += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
                    used > width
                })
                .map_or(line.len(), |(i, _)| i)
                .max(line.chars().next().map_or(0, char::len_utf8));
            let rest = line.split_off(cut);
            out.push(std::mem::replace(&mut line, rest));
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// The first of `hints` that fit `width`, ` · ` between.
fn hints(list: &[&str], width: usize) -> String {
    let mut out = String::new();
    for h in list {
        let next = if out.is_empty() {
            h.to_string()
        } else {
            format!("{out} · {h}")
        };
        if w(&next) > width {
            break;
        }
        out = next;
    }
    out
}

/// What the last `enter` does, by where the wizard came from.
fn last_enter(came: Came) -> &'static str {
    match came {
        Came::FirstRun => "enter to open the list",
        Came::Command => "enter to finish",
        Came::List => "enter to go back to the list",
    }
}

/// The step's keys, for the foot.
fn foot(wz: &Wizard) -> Vec<&'static str> {
    match wz.step {
        Step::Found => vec!["enter to walk through", "tab to just get me there", "q to quit"],
        Step::Changes => vec![
            "space or click to tick",
            "↑↓ to move",
            "enter to apply",
            "esc to go back",
            "q to quit",
        ],
        Step::Applying if wz.running() => vec!["setting up…"],
        Step::Applying => vec!["enter to go on"],
        Step::YourTurn => vec!["enter to go on", "esc to go back"],
        Step::Keys => vec!["ctrl+j or enter to go on", "esc to go back"],
        Step::Now => vec![last_enter(wz.came), "esc to go back"],
    }
}

/// Where things were drawn, for the terminal and the mouse.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Drawn {
    /// The guide's link, where it is on screen.
    pub link: Option<Rect>,
    /// Step 2's rows on screen: the screen row, and which row (`rows`).
    pub rows: Vec<(u16, usize)>,
}

impl Drawn {
    /// Step 2's row on the screen row `y`, if one is drawn there.
    pub fn row_at(&self, y: u16) -> Option<usize> {
        self.rows.iter().find(|(at, _)| *at == y).map(|(_, r)| *r)
    }
}

/// Draws the wizard into the frame: where the guide's link is, if it is on
/// screen, for the terminal to make it one, and where step 2's rows are.
pub fn draw(wz: &Wizard, f: &mut Frame) -> Drawn {
    let area = f.area();
    if area.height < 3 || area.width < 10 {
        return Drawn::default();
    }
    let wide = area.width >= WIDE && area.height >= 16;
    let text_w = area.width.saturating_sub(INDENT * 2) as usize;
    let number = if wz.fast {
        String::new()
    } else {
        format!("  {}/{STEPS}", wz.step.number())
    };
    let title = if wz.fast {
        "Setting up everything"
    } else {
        wz.step.title()
    };
    let name = vec![
        Span::styled("relay setup", theme::accent().add_modifier(Modifier::BOLD)),
        Span::styled(number, theme::dim()),
    ];
    let head_h: u16 = if wide {
        let m = Rect::new(area.x, area.y, mark::W, mark::SMALL.len() as u16);
        f.render_widget(Paragraph::new(mark::lines(&mark::SMALL)), m);
        let x = area.x + mark::W + 3;
        let text = Rect::new(x, area.y + 1, area.right().saturating_sub(x), 3);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(name),
                Line::styled(TAGLINE, theme::dim()),
                Line::styled(title, theme::name()),
            ]),
            text,
        );
        mark::SMALL.len() as u16 + 1
    } else {
        let text = Rect::new(area.x + INDENT, area.y, text_w as u16, 3);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(name),
                Line::styled(fit(TAGLINE, text_w).trim_end().to_string(), theme::dim()),
                Line::styled(title, theme::name()),
            ]),
            text,
        );
        4
    };
    let body_h = area.height.saturating_sub(head_h + 2);
    let body = Rect::new(area.x + INDENT, area.y + head_h, text_w as u16, body_h);
    let (lines, focus, link, hits) = body_lines(wz, text_w);
    // The window moves only to keep what has the focus in it.
    let skip = (focus + 1).saturating_sub(body_h as usize);
    let rows = hits
        .into_iter()
        .filter(|(line, _)| *line >= skip && *line < skip + body_h as usize)
        .map(|(line, row)| (body.y + (line - skip) as u16, row))
        .collect();
    let shown: Vec<Line> = lines.into_iter().skip(skip).take(body_h as usize).collect();
    f.render_widget(Paragraph::new(shown), body);
    let foot_at = Rect::new(area.x + INDENT, area.bottom() - 1, text_w as u16, 1);
    f.render_widget(
        Paragraph::new(Line::styled(hints(&foot(wz), text_w), theme::dim())),
        foot_at,
    );
    let link = link.and_then(|(row, col)| {
        let row = row.checked_sub(skip)?;
        (row < body_h as usize).then(|| Rect::new(body.x + col as u16, body.y + row as u16, w(LINK_TEXT) as u16, 1))
    });
    Drawn { link, rows }
}

/// Lines, the one with the focus, where the link is (line, column), and
/// where step 2's rows are (line, row).
type Body = (Vec<Line<'static>>, usize, Option<(usize, usize)>, Vec<(usize, usize)>);

/// The step's lines, `width` wide, and the rest of `Body`: the focus is
/// step 2's cursor, else the top.
fn body_lines(wz: &Wizard, width: usize) -> Body {
    let mut link = None;
    let mut hits = Vec::new();
    let (lines, focus) = match wz.step {
        Step::Found => (found(wz, width), 0),
        Step::Changes => {
            let (lines, focus, h) = changes(wz, width);
            hits = h;
            (lines, focus)
        }
        Step::Applying => (applying(wz, width), 0),
        Step::YourTurn => (your_turn(wz, width), 0),
        Step::Keys => (keys(wz, width), 0),
        Step::Now => {
            let (lines, at) = now(wz, width);
            link = Some(at);
            (lines, 0)
        }
    };
    (lines, focus, link, hits)
}

/// Step 1: every agent harness, found or not, and what setup does for it;
/// then the two ways on.
fn found(wz: &Wizard, width: usize) -> Vec<Line<'static>> {
    let name_w = wz.harns.iter().map(|h| w(&h.name)).max().unwrap_or(0) + 2;
    let mut out = Vec::new();
    for h in &wz.harns {
        let mut spans = vec![icon(h), Span::raw(" ")];
        let (name_style, state, state_style) = if h.found {
            (theme::name(), "found", ok())
        } else {
            (theme::dim(), "not installed", theme::dim())
        };
        spans.push(Span::styled(fit(&h.name, name_w), name_style));
        spans.push(Span::styled(fit(state, 15), state_style));
        let does = if h.does.is_empty() {
            "nothing to set up".to_string()
        } else {
            h.does.join(" · ")
        };
        let room = width.saturating_sub(spans_w(&spans));
        spans.push(Span::styled(fit(&does, room).trim_end().to_string(), theme::dim()));
        out.push(Line::from(spans));
    }
    out.push(Line::raw(""));
    out.push(Line::from(vec![
        Span::styled(" ▸ Walk me through it ", theme::selected().add_modifier(Modifier::BOLD)),
        Span::raw("   "),
        Span::styled("Yes, just get me there", theme::accent()),
        Span::styled(" (tab)", theme::dim()),
    ]));
    out
}

/// A tick box: `[x]`, `[ ]`, or `[-]` where some are ticked.
fn tick(on: Option<bool>) -> Span<'static> {
    match on {
        Some(true) => Span::styled("[x] ", ok()),
        Some(false) => Span::styled("[ ] ", theme::dim()),
        None => Span::styled("[-] ", ok()),
    }
}

/// Step 2: a card per harness found that setup has something for — its
/// header ticks all of it — and each change with a tick box of its own:
/// what, where, why. The line that has the focus; where each of `rows` is
/// (line, row).
fn changes(wz: &Wizard, width: usize) -> (Vec<Line<'static>>, usize, Vec<(usize, usize)>) {
    let rows = wz.rows();
    let at = rows.get(wz.cursor).copied();
    let cards: Vec<usize> = wz.listed();
    let all = cards.iter().flat_map(|i| wz.harns[*i].changes());
    let label_w = all.clone().map(|c| w(&c.label)).max().unwrap_or(0);
    let file_w = all.map(|c| w(&c.part.file)).max().unwrap_or(0);
    // `▸ ` `    [x] + ` label, file, why.
    let lead_w = 2 + 4 + 4 + 2;
    // In columns where they fit; else each file right after its label.
    let aligned = lead_w + label_w + 2 + file_w <= width;
    let why_room = width.saturating_sub(lead_w + label_w + 2 + file_w + 2);
    let inline = aligned;
    let mut out = Vec::new();
    let mut hits = Vec::new();
    let mut focus = 0;
    let mut row = 0;
    let pointer = |here: bool| Span::styled(if here { "▸ " } else { "  " }, ok());
    for i in cards {
        let h = &wz.harns[i];
        if !out.is_empty() {
            out.push(Line::raw(""));
        }
        let here = at == Some((i, None));
        let mut head = vec![pointer(here)];
        if h.needs() {
            let changes = h.changes();
            let on = changes.iter().filter(|c| c.on).count();
            head.push(tick(match on {
                0 => Some(false),
                n if n == changes.len() => Some(true),
                _ => None,
            }));
        }
        head.extend([
            icon(h),
            Span::raw(" "),
            Span::styled(h.name.clone(), theme::name()),
            Span::raw("  "),
        ]);
        match &h.need {
            Err(e) => {
                let room = width.saturating_sub(spans_w(&head) + 2);
                head.push(Span::styled(format!("✗ {}", fit(e, room).trim_end()), bad()));
                out.push(Line::from(head));
                continue;
            }
            Ok(c) if c.is_empty() => {
                head.push(Span::styled("✓ set up", ok()));
                out.push(Line::from(head));
                continue;
            }
            Ok(_) => {}
        }
        out.push(Line::from(head));
        hits.push((out.len() - 1, row));
        if here {
            focus = out.len() - 1;
        }
        row += 1;
        for (n, c) in h.changes().iter().enumerate() {
            let here = at == Some((i, Some(n)));
            let style = if c.on { Style::default() } else { theme::dim() };
            let mut l = vec![
                pointer(here),
                Span::raw("    "),
                tick(Some(c.on)),
                Span::styled(format!("{} ", c.sign), if c.on { ok() } else { theme::dim() }),
                Span::styled(
                    fit(&c.label, if aligned { label_w + 2 } else { w(&c.label) + 2 }),
                    style.add_modifier(Modifier::BOLD),
                ),
            ];
            let room = width.saturating_sub(spans_w(&l));
            l.push(Span::styled(
                fit(&c.part.file, (file_w + 2).min(room)).trim_end().to_string(),
                theme::dim(),
            ));
            if inline {
                l.push(Span::raw(" ".repeat((file_w + 2).saturating_sub(w(&c.part.file)))));
            }
            // Why, for what the cursor is on: the change, or all of its
            // harness's.
            let why = theme::dim().add_modifier(Modifier::ITALIC);
            let says = here || at == Some((i, None));
            let beside = inline && w(&c.why) + 2 <= why_room;
            if says && beside {
                l.push(Span::styled(format!("— {}", c.why), why));
            }
            out.push(Line::from(l));
            hits.push((out.len() - 1, row));
            if says && !beside {
                for wl in wrap(&c.why, width.saturating_sub(lead_w + 2)) {
                    out.push(Line::styled(format!("{}— {wl}", " ".repeat(lead_w)), why));
                }
            }
            if here {
                focus = out.len() - 1;
            }
            row += 1;
        }
    }
    if wz.needy().is_empty() {
        out.push(Line::raw(""));
        out.push(Line::styled("✓ already set up", ok()));
    }
    (out, focus, hits)
}

/// One job's line: how it stands, the harness, the change, its file; a
/// failure's words under it.
fn job_lines(wz: &Wizard, j: &Job, name_w: usize, label_w: usize, width: usize) -> Vec<Line<'static>> {
    let h = &wz.harns[j.harn];
    let c = wz.change(j);
    let mark = match &j.run {
        Run::Waiting => Span::styled("·", theme::dim()),
        Run::Running => Span::styled(SPIN[wz.tick % SPIN.len()], Style::default().fg(theme::LIVE)),
        Run::Done(Ok(_)) => Span::styled("✓", ok()),
        Run::Done(Err(_)) => Span::styled("✗", bad()),
    };
    let mut l = vec![
        mark,
        Span::raw(" "),
        icon(h),
        Span::raw(" "),
        Span::styled(fit(&h.name, name_w), theme::name()),
        Span::raw(fit(&c.label, label_w + 2)),
    ];
    let room = width.saturating_sub(spans_w(&l));
    l.push(Span::styled(
        fit(&c.part.file, room).trim_end().to_string(),
        theme::dim(),
    ));
    let mut out = vec![Line::from(l)];
    if let Run::Done(Err(e)) = &j.run {
        let e = e.strip_prefix(&format!("{}: ", h.name)).unwrap_or(e);
        for wl in wrap(e, width.saturating_sub(4)) {
            out.push(Line::styled(format!("    {wl}"), bad()));
        }
    }
    out
}

/// Step 3: a line per change, as it goes.
fn applying(wz: &Wizard, width: usize) -> Vec<Line<'static>> {
    let name_w = wz.jobs.iter().map(|j| w(&wz.harns[j.harn].name)).max().unwrap_or(0) + 2;
    let label_w = wz.jobs.iter().map(|j| w(&wz.change(j).label)).max().unwrap_or(0);
    let mut out: Vec<Line> = wz
        .jobs
        .iter()
        .flat_map(|j| job_lines(wz, j, name_w, label_w, width))
        .collect();
    if wz.all_done {
        out.push(Line::raw(""));
        out.push(match wz.failed() {
            0 => Line::styled("✓ all set up", ok()),
            n => Line::styled(
                format!("✗ {n} failed · the rest is set up · rl setup tries again"),
                bad(),
            ),
        });
    }
    out
}

/// Step 4: what only you can do, as a checklist.
fn your_turn(wz: &Wizard, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::styled("Relay can't do this part for you:", theme::dim()),
        Line::raw(""),
    ];
    for (i, note) in wz.notes() {
        let h = &wz.harns[i];
        out.push(Line::from(vec![
            Span::styled("[ ] ", ok()),
            icon(h),
            Span::raw(" "),
            Span::styled(h.name.clone(), theme::name()),
        ]));
        for wl in wrap(&note, width.saturating_sub(6)) {
            out.push(Line::raw(format!("      {wl}")));
        }
    }
    out
}

/// The essential keys as the help grid has them: the key plain, what it
/// does quiet; two columns where they fit, else one.
pub fn key_grid(width: usize) -> Vec<Line<'static>> {
    let entries: Vec<(&str, &str)> = help::ESSENTIAL
        .iter()
        .map(|(id, does)| (help::get(*id).keys, *does))
        .collect();
    let col = |items: &[(&str, &str)]| {
        (
            items.iter().map(|(k, _)| w(k)).max().unwrap_or(0) + 2,
            items.iter().map(|(_, d)| w(d)).max().unwrap_or(0),
        )
    };
    let left: Vec<_> = entries.iter().copied().step_by(2).collect();
    let right: Vec<_> = entries.iter().copied().skip(1).step_by(2).collect();
    let (lk, ld) = col(&left);
    let (rk, rd) = col(&right);
    if lk + ld + 4 + rk + rd <= width {
        return left
            .iter()
            .zip(right.iter())
            .map(|((k1, d1), (k2, d2))| {
                Line::from(vec![
                    Span::raw(fit(k1, lk)),
                    Span::styled(fit(d1, ld + 4), theme::dim()),
                    Span::raw(fit(k2, rk)),
                    Span::styled(d2.to_string(), theme::dim()),
                ])
            })
            .collect();
    }
    let (k, _) = col(&entries);
    entries
        .iter()
        .map(|(key, does)| {
            Line::from(vec![
                Span::raw(fit(key, k)),
                Span::styled(does.to_string(), theme::dim()),
            ])
        })
        .collect()
}

/// Step 5: the keys you will use, and a go at one.
fn keys(wz: &Wizard, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if wz.already {
        out.push(Line::styled("✓ already set up", ok()));
        out.push(Line::raw(""));
    }
    out.extend(key_grid(width));
    out.push(Line::raw(""));
    let detach = help::get(help::Id::Detach).keys;
    let words =
        format!("An attached session is full screen: {detach} gives the list back, and the session keeps running.");
    out.extend(wrap(&words, width).into_iter().map(|l| Line::styled(l, theme::dim())));
    out.push(Line::raw(""));
    out.push(Line::from(vec![
        Span::styled("try it: ", theme::dim()),
        Span::styled("press ctrl+j to go on", theme::accent().add_modifier(Modifier::BOLD)),
    ]));
    out
}

/// Two harnesses for the drawing: Claude's and Codex's looks where they
/// are in the registry, else the first two.
fn pair(wz: &Wizard) -> (Span<'static>, Span<'static>) {
    let by = |n: &str| wz.harns.iter().find(|h| h.name == n);
    let lit = |h: &Harn| Harn {
        found: true,
        ..h.clone()
    };
    let a = by("claude").or(wz.harns.first());
    let b = by("codex").or(wz.harns.get(1));
    let plain = || Span::raw(crate::rows::AGENT);
    (
        a.map(|h| icon(&lit(h))).unwrap_or_else(plain),
        b.map(|h| icon(&lit(h))).unwrap_or_else(plain),
    )
}

/// Step 6: two agents messaging, what to ask, and the guide.
fn now(wz: &Wizard, width: usize) -> (Vec<Line<'static>>, (usize, usize)) {
    let (a, b) = pair(wz);
    let mut out = vec![
        Line::from(vec![
            Span::raw("  "),
            a,
            Span::styled(" api ", theme::name()),
            Span::styled(" ──send──▶ ", theme::accent()),
            b,
            Span::styled(" web", theme::name()),
        ]),
        Line::styled("         ◀──inbox─", theme::dim()),
        Line::raw(""),
    ];
    let lead = "Your agents can message each other. Ask one in plain words:";
    out.extend(wrap(lead, width).into_iter().map(Line::raw));
    for e in EXAMPLES {
        let mut first = true;
        for wl in wrap(&format!("\"{e}\""), width.saturating_sub(4)) {
            let lead = if std::mem::take(&mut first) { "  › " } else { "    " };
            out.push(Line::from(vec![
                Span::styled(lead, theme::accent()),
                Span::styled(wl, theme::dim().add_modifier(Modifier::ITALIC)),
            ]));
        }
    }
    out.push(Line::raw(""));
    let at = (out.len(), w("More: "));
    out.push(Line::from(vec![
        Span::raw("More: "),
        Span::styled(LINK_TEXT, theme::accent().add_modifier(Modifier::UNDERLINED)),
    ]));
    out.extend(
        wrap(MESSAGING_GUIDE, width)
            .into_iter()
            .map(|l| Line::styled(l, theme::dim())),
    );
    (out, at)
}

/// What the wizard leaves in the scrollback once it is over: what changed,
/// ✓ or ✗, what is left for you, the keys, the guide.
pub fn summary(wz: &Wizard) -> Vec<Line<'static>> {
    let head = |rest: &str| {
        Line::from(vec![
            Span::styled("relay setup", theme::accent().add_modifier(Modifier::BOLD)),
            Span::styled(format!(" · {rest}"), theme::dim()),
        ])
    };
    let mut out = Vec::new();
    if wz.jobs.is_empty() {
        if wz.already {
            out.push(head("done"));
            out.push(Line::styled("  ✓ already set up", ok()));
        } else {
            let later = match wz.came {
                Came::Command => "rl setup any time",
                _ => "/setup in the list any time",
            };
            out.push(head(&format!("nothing changed · {later}")));
            if wz.ended != Some(Ended::Finished) {
                return out;
            }
        }
    } else {
        out.push(head(if wz.failed() == 0 { "done" } else { "done, with errors" }));
        for (i, jobs) in wz.applied() {
            let h = &wz.harns[i];
            let failed = jobs.iter().any(|j| matches!(j.run, Run::Done(Err(_))));
            let mut l = vec![
                Span::raw("  "),
                if failed {
                    Span::styled("✗ ", bad())
                } else {
                    Span::styled("✓ ", ok())
                },
                icon(h),
                Span::raw(" "),
                Span::styled(h.name.clone(), theme::name()),
                Span::raw("  "),
            ];
            for (n, j) in jobs.iter().enumerate() {
                if n > 0 {
                    l.push(Span::styled(" · ", theme::dim()));
                }
                let label = wz.change(j).label.clone();
                match &j.run {
                    Run::Done(Err(e)) => {
                        let e = e.strip_prefix(&format!("{}: ", h.name)).unwrap_or(e);
                        l.push(Span::styled(format!("{label} ✗ {e}"), bad()));
                    }
                    Run::Done(Ok(_)) => l.push(Span::raw(label)),
                    _ => l.push(Span::styled(format!("{label} (not run)"), theme::dim())),
                }
            }
            out.push(Line::from(l));
        }
        for (i, note) in wz.notes() {
            out.push(Line::from(vec![
                Span::styled("  → ", theme::needs()),
                Span::styled(format!("{}: ", wz.harns[i].name), theme::name()),
                Span::raw(note),
            ]));
        }
    }
    let keys: Vec<String> = help::ESSENTIAL_LINE
        .iter()
        .map(|(id, does)| format!("{} {does}", help::get(*id).keys))
        .collect();
    out.push(Line::from(vec![
        Span::styled("  keys  ", theme::dim()),
        Span::raw(keys.join(" · ")),
    ]));
    out.push(Line::from(vec![
        Span::styled("  more  ", theme::dim()),
        Span::styled(MESSAGING_GUIDE, theme::accent().add_modifier(Modifier::UNDERLINED)),
    ]));
    out
}

/// A line as the terminal takes it: each span in its colours (truecolor,
/// SGR), the guide's address a link (OSC 8) as well.
pub fn ansi(line: &Line) -> String {
    let mut s = String::new();
    for span in &line.spans {
        let st = span.style;
        let mut sgr: Vec<String> = Vec::new();
        if st.add_modifier.contains(Modifier::BOLD) {
            sgr.push("1".into());
        }
        if st.add_modifier.contains(Modifier::DIM) {
            sgr.push("2".into());
        }
        if st.add_modifier.contains(Modifier::ITALIC) {
            sgr.push("3".into());
        }
        if st.add_modifier.contains(Modifier::UNDERLINED) {
            sgr.push("4".into());
        }
        if let Some(Color::Rgb(r, g, b)) = st.fg {
            sgr.push(format!("38;2;{r};{g};{b}"));
        }
        let text = span.content.as_ref();
        let text = if text == MESSAGING_GUIDE {
            link(text, text)
        } else {
            text.to_string()
        };
        if sgr.is_empty() {
            s.push_str(&text);
        } else {
            s.push_str(&format!("\x1b[{}m{text}\x1b[0m", sgr.join(";")));
        }
    }
    s
}

/// `text`, a link to `url` where the terminal makes links (OSC 8); plain
/// text elsewhere.
pub fn link(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}
