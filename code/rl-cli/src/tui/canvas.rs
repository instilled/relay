//! A grid of characters to draw boxes and lines on: each line marks the sides of a cell it
//! leaves by, and `join` turns the marks into the box-drawing character they make. Pure.

use super::theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Which sides of a cell a line leaves by.
pub(super) const UP: u8 = 1;
pub(super) const DOWN: u8 = 2;
pub(super) const LEFT: u8 = 4;
pub(super) const RIGHT: u8 = 8;

/// What the cell after a wide character holds: it is the character's second column, and
/// nothing is drawn there.
const WIDE: char = '\0';

#[derive(Clone)]
struct Cell {
    ch: char,
    lines: u8,
    style: Style,
}

/// A drawing in characters, its lines joined where they meet. Read out a window of it with
/// [`Canvas::lines`].
pub struct Canvas {
    w: usize,
    h: usize,
    cells: Vec<Cell>,
}

impl Canvas {
    pub(super) fn new(w: usize, h: usize) -> Self {
        let blank = Cell {
            ch: ' ',
            lines: 0,
            style: Style::default(),
        };
        Self {
            w,
            h,
            cells: vec![blank; w * h],
        }
    }

    /// How wide and how tall it is.
    pub(super) fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    fn at(&mut self, x: usize, y: usize) -> Option<&mut Cell> {
        (x < self.w && y < self.h).then(|| &mut self.cells[y * self.w + x])
    }

    pub(super) fn put(&mut self, x: usize, y: usize, ch: char, style: Style) {
        if let Some(c) = self.at(x, y) {
            c.ch = ch;
            c.lines = 0;
            c.style = style;
        }
    }

    /// `s` from `(x, y)`, a cell to each column it takes: a wide character (CJK, most emoji)
    /// two.
    pub(super) fn text(&mut self, x: usize, y: usize, s: &str, style: Style) {
        let mut x = x;
        for ch in s.chars() {
            self.put(x, y, ch, style);
            if ch.width().unwrap_or(0) > 1 {
                x += 1;
                self.put(x, y, WIDE, style);
            }
            x += 1;
        }
    }

    pub(super) fn mark(&mut self, x: usize, y: usize, m: u8, style: Style) {
        if let Some(c) = self.at(x, y) {
            c.lines |= m;
            // A coloured line shows through a dim one.
            if c.style == Style::default() || style != theme::dim() {
                c.style = style;
            }
        }
    }

    /// Joins every line mark into the box-drawing character it makes.
    pub(super) fn join(&mut self) {
        for c in &mut self.cells {
            if c.lines == 0 {
                continue;
            }
            c.ch = match c.lines {
                x if x == LEFT | RIGHT || x == LEFT || x == RIGHT => '─',
                x if x == UP | DOWN || x == UP || x == DOWN => '│',
                x if x == DOWN | RIGHT => '╭',
                x if x == DOWN | LEFT => '╮',
                x if x == UP | RIGHT => '╰',
                x if x == UP | LEFT => '╯',
                x if x == UP | DOWN | RIGHT => '├',
                x if x == UP | DOWN | LEFT => '┤',
                x if x == LEFT | RIGHT | DOWN => '┬',
                x if x == LEFT | RIGHT | UP => '┴',
                _ => '┼',
            };
        }
    }

    /// Rows `y0..y0+h`, columns `x0..x0+w`, as ratatui lines.
    pub(super) fn lines(&self, x0: usize, y0: usize, w: usize, h: usize) -> Vec<Line<'static>> {
        (y0..y0 + h)
            .map(|y| {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut run = String::new();
                let mut style = Style::default();
                for x in x0..x0 + w {
                    let (ch, st) = match (x < self.w && y < self.h).then(|| &self.cells[y * self.w + x]) {
                        Some(c) => (c.ch, c.style),
                        None => (' ', Style::default()),
                    };
                    if st != style && !run.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut run), style));
                    }
                    style = st;
                    if ch != WIDE {
                        run.push(ch);
                    }
                }
                if !run.is_empty() {
                    spans.push(Span::styled(run, style));
                }
                Line::from(spans)
            })
            .collect()
    }

    /// The whole canvas as plain text, trailing space trimmed; for tests.
    #[cfg(test)]
    pub(super) fn plain(&self) -> String {
        (0..self.h)
            .map(|y| {
                let row: String = (0..self.w)
                    .map(|x| self.cells[y * self.w + x].ch)
                    .filter(|ch| *ch != WIDE)
                    .collect();
                row.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
