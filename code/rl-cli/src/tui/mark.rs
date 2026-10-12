//! Relay's mark: a radio tower sending signals. It is drawn from one grid of
//! pixels, and comes in two sizes — a small one that rides the header, and
//! the full one the empty list and the setup guide have room for. Only the
//! mark wears the accent; nothing round it does.

use super::theme;
use ratatui::text::Line;

/// The mark as designed, one character a pixel: `#` is ink.
#[cfg(test)]
pub const PIXELS: [&str; 12] = [
    "........##........",
    ".#.....####.....#.",
    "..................",
    "#..#....##....#..#",
    "#..#....##....#..#",
    "..................",
    ".#.....####.....#.",
    "......##..##......",
    "..................",
    ".....##....##.....",
    "...############...",
    "...##..####..##...",
];

/// The header's mark: two pixels a cell in half-blocks, its blank rows
/// dropped, with a column of air at its left.
pub const SMALL: [&str; 5] = [
    "  ▄     ▄██▄     ▄ ",
    " █  █    ██    █  █",
    "  ▀    ▄█▀▀█▄    ▀ ",
    "    ▄▄██▄▄▄▄██▄▄   ",
    "    ▀▀  ▀▀▀▀  ▀▀   ",
];

/// The mark full size: a cell a pixel, its blank rows dropped and its last
/// two rows sharing a row of half-blocks, with the same column of air.
pub const FULL: [&str; 8] = [
    "         ██        ",
    "  █     ████     █ ",
    " █  █    ██    █  █",
    " █  █    ██    █  █",
    "  █     ████     █ ",
    "       ██  ██      ",
    "      ██    ██     ",
    "    ██▀▀████▀▀██   ",
];

/// How wide either mark is, in cells.
pub const W: u16 = 19;

/// A mark's rows as lines, in the accent.
pub fn lines(rows: &[&'static str]) -> Vec<Line<'static>> {
    rows.iter().map(|r| Line::styled(*r, theme::accent())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    /// Two rows of pixels as one row of half-blocks, behind a column of air.
    fn halves(top: &str, bottom: &str) -> String {
        let bottom: Vec<char> = bottom.chars().collect();
        let cells = top
            .chars()
            .enumerate()
            .map(|(i, t)| match (t == '#', bottom.get(i) == Some(&'#')) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        std::iter::once(' ').chain(cells).collect()
    }

    fn inked() -> Vec<&'static str> {
        PIXELS.iter().copied().filter(|r| r.contains('#')).collect()
    }

    #[test]
    fn the_small_mark_is_the_pixels_two_rows_a_cell() {
        let blank = ".".repeat(PIXELS[0].len());
        let rows = inked();
        let small: Vec<String> = rows
            .chunks(2)
            .map(|p| halves(p[0], p.get(1).copied().unwrap_or(&blank)))
            .collect();
        assert_eq!(small, SMALL);
    }

    #[test]
    fn the_full_mark_is_the_pixels_a_cell_each_but_the_foot() {
        let rows = inked();
        let (body, foot) = rows.split_at(rows.len() - 2);
        let mut full: Vec<String> = body
            .iter()
            .map(|r| {
                std::iter::once(' ')
                    .chain(r.chars().map(|c| if c == '#' { '█' } else { ' ' }))
                    .collect()
            })
            .collect();
        full.push(halves(foot[0], foot[1]));
        assert_eq!(full, FULL);
    }

    #[test]
    fn every_row_is_as_wide_as_the_mark() {
        for row in SMALL.iter().chain(FULL.iter()) {
            assert_eq!(Span::raw(*row).width(), W as usize, "{row:?}");
        }
    }
}
