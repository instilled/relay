//! `impl App` — `space`: a peek at the session under the cursor, in a pane
//! under the list, without attaching to it. What it shows is its harness's
//! to say (`Harness::peek`): asked off the list's thread, so a harness slow
//! to answer never holds a key up; asked again as the cursor settles on
//! another row, and every `AGAIN` while it stays. Nothing is sent to the
//! session.

use super::app::App;
use super::theme;
use crate::rows::{Line as ListLine, Names, fit};
use crate::sources::{self, Row};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use rl_detect::harness::Peek;
use std::time::{Duration, Instant};

/// How long the cursor rests on a row before it is peeked at: a key held
/// down asks nothing of the rows it passes.
pub const SETTLE: Duration = Duration::from_millis(150);
/// How often the row it rests on is asked again.
pub const AGAIN: Duration = Duration::from_millis(1500);
/// How many lines are asked for before the pane has been drawn.
const ROWS: usize = 12;
/// The most rows the pane takes, and the fewest the list keeps beside it.
const PANE_MOST: u16 = 16;
const LIST_LEAST: u16 = 4;

/// The pane, while it is open.
#[derive(Debug, Default)]
pub struct Peeking {
    /// What it shows: the row's key, and what its harness said of it.
    pub shown: Option<(String, Peek)>,
    /// When that was said.
    pub at: Option<Instant>,
    /// The row last asked of, while its answer is on its way.
    pub asked: Option<String>,
}

/// What a peek came to: the row's key, and what its harness said.
pub type Peeked = (String, Peek);

/// How tall the pane is under a list area `h` tall, its title line too:
/// none where the list would have too little left.
pub fn pane_h(h: u16) -> u16 {
    let pane = (h * 2 / 5).min(PANE_MOST);
    if pane < 3 || h.saturating_sub(pane) < LIST_LEAST {
        return 0;
    }
    pane
}

impl App {
    /// `space`: the pane opened on the session under the cursor, or closed.
    /// On a header, or on nothing, it stays shut and the footer says why.
    pub fn toggle_peek(&mut self) {
        if self.peek.take().is_some() {
            return;
        }
        if self.peek_row().is_none() {
            return self.say("space peeks at a session: put the cursor on one");
        }
        self.peek = Some(Peeking::default());
        self.ask_peek();
    }

    /// The session row the pane is for: the one under the cursor.
    pub fn peek_row(&self) -> Option<&Row> {
        match self.selected()? {
            ListLine::Session(r) => Some(r),
            ListLine::Group(_) => None,
        }
    }

    /// When the row under the cursor is next to be asked: once the cursor
    /// has settled on a row not shown yet, `AGAIN` after the last answer for
    /// the one shown. None while the pane is shut, it is on no session, or
    /// that row's answer is on its way.
    pub fn peek_due(&self) -> Option<Instant> {
        let p = self.peek.as_ref()?;
        let key = &self.peek_row()?.key;
        if p.asked.as_ref() == Some(key) {
            return None;
        }
        match (&p.shown, p.at) {
            (Some((k, _)), Some(at)) if k == key => Some(at + AGAIN),
            _ => Some(self.moved + SETTLE),
        }
    }

    /// Asks the harness of the row under the cursor what it is doing, on a
    /// blocking thread of its own; the answer comes to `peeked`.
    pub fn ask_peek(&mut self) {
        let Some(r) = self.peek_row() else { return };
        let (key, session) = (r.key.clone(), r.session.clone());
        let harness = sources::harness_of(&r.harness);
        let rows = match self.peek_h.get() {
            0 => ROWS,
            h => h as usize,
        };
        let Some(p) = self.peek.as_mut() else { return };
        p.asked = Some(key.clone());
        self.peeks
            .spawn_blocking(move || (key.clone(), harness.peek(&session, rows)));
    }

    /// What a peek came to: shown, where it is the row under the cursor's;
    /// else let go, the row it was for left behind.
    pub fn peeked(&mut self, (key, said): Peeked) {
        let here = self.peek_row().map(|r| r.key.clone());
        let Some(p) = self.peek.as_mut() else { return };
        if p.asked.as_ref() == Some(&key) {
            p.asked = None;
        }
        if here.as_ref() == Some(&key) {
            p.shown = Some((key, said));
            p.at = Some(Instant::now());
        }
    }

    /// The pane's rows under a list area `h` tall: none while it is shut or
    /// the cursor is on no session.
    pub fn peek_rows(&self, h: u16) -> u16 {
        match (&self.peek, self.peek_row()) {
            (Some(_), Some(_)) => pane_h(h),
            _ => 0,
        }
    }

    /// The pane in `area`: a rule with the session's name, harness and how
    /// it stands, then its last lines as its harness gave them, or why there
    /// are none — or, while the first answer is on its way, an ellipsis.
    pub fn draw_peek(&self, f: &mut Frame, area: Rect) {
        let (Some(p), Some(r)) = (&self.peek, self.peek_row()) else {
            return;
        };
        let w = area.width as usize;
        let body_h = area.height.saturating_sub(1);
        self.peek_h.set(body_h);
        let names = Names::new(&self.config.list.names);
        let mut head = vec![
            Span::styled("─ ", theme::dim()),
            Span::styled(names.title(r).to_string(), theme::name()),
        ];
        let about: Vec<&str> = [r.harness.as_str(), r.words.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        if !about.is_empty() {
            head.push(Span::styled(format!(" · {}", about.join(" · ")), theme::dim()));
        }
        head.push(Span::raw(" "));
        let used: usize = head.iter().map(Span::width).sum();
        if used > w {
            head = vec![Span::styled(
                fit(names.title(r), w).trim_end().to_string(),
                theme::name(),
            )];
        } else {
            head.push(Span::styled("─".repeat(w - used), theme::dim()));
        }
        let quiet = |s: &str| {
            Line::from(Span::styled(
                fit(&format!("  {s}"), w).trim_end().to_string(),
                theme::dim(),
            ))
        };
        let body: Vec<Line<'static>> = match &p.shown {
            Some((k, said)) if *k == r.key => match said {
                Peek::Lines(l) if l.is_empty() => vec![quiet("nothing on its screen yet")],
                Peek::Lines(l) => {
                    let from = l.len().saturating_sub(body_h as usize);
                    l[from..]
                        .iter()
                        .map(|l| Line::raw(fit(&format!("  {}", plain(l)), w).trim_end().to_string()))
                        .collect()
                }
                Peek::Nothing(why) => why.lines().take(body_h as usize).map(quiet).collect(),
            },
            _ => vec![quiet("…")],
        };
        let mut lines = vec![Line::from(head)];
        lines.extend(body);
        f.render_widget(Paragraph::new(lines), area);
    }
}

/// A line as it may be drawn: a tab or any other control character a
/// harness left in it as a space.
fn plain(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pane_takes_two_fifths_and_leaves_the_list_its_rows() {
        assert_eq!(pane_h(30), 12);
        assert_eq!(pane_h(60), PANE_MOST, "no taller than this");
        assert_eq!(pane_h(10), 4);
        assert_eq!(pane_h(7), 0, "too little left for the list");
    }

    #[test]
    fn a_control_character_is_drawn_as_a_space() {
        assert_eq!(plain("a\tb\u{7}c"), "a b c");
    }
}
