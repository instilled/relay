//! A `vt100` screen drawn into a ratatui buffer. The run is drawn at the size
//! it ran at; when the pane is shorter, the rows around the cursor win
//! (a full-screen app keeps it at the bottom, a fresh shell at the top), and
//! the right edge is clipped. Scrolled back, the cursor stops deciding: the
//! top of the window is what was asked for, and the cursor is not drawn
//! because it is not where you are looking. A row cut at the right edge ends
//! in a dim `›`, so a clipped line never reads as the run's own.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

pub fn draw(screen: &vt100::Screen, area: Rect, buf: &mut Buffer) {
    let (rows, cols) = screen.size();
    let back = screen.scrollback();
    let (cursor_row, _) = screen.cursor_position();
    let row_off = if back > 0 {
        0
    } else {
        cursor_row
            .saturating_sub(area.height.saturating_sub(1))
            .min(rows.saturating_sub(area.height))
    };
    for y in 0..area.height.min(rows) {
        for x in 0..area.width.min(cols) {
            let Some(cell) = screen.cell(y + row_off, x) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let contents = cell.contents();
            let symbol = if contents.is_empty() { " " } else { contents };
            let mut style = Style::default().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.underline() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            if cell.inverse() {
                style = style.add_modifier(Modifier::REVERSED);
            }
            if let Some(c) = buf.cell_mut((area.x + x, area.y + y)) {
                c.set_symbol(symbol);
                c.set_style(style);
            }
        }
    }
    // Rows that go on past the edge say so in the last column.
    if cols > area.width && area.width > 0 {
        for y in 0..area.height.min(rows) {
            let row = y + row_off;
            let cut = (area.width..cols).any(|x| screen.cell(row, x).is_some_and(|c| !c.contents().trim().is_empty()));
            if cut && let Some(c) = buf.cell_mut((area.x + area.width - 1, area.y + y)) {
                c.set_symbol("›");
                c.set_style(Style::default().add_modifier(Modifier::DIM));
            }
        }
    }
    if back == 0 && !screen.hide_cursor() {
        let (cr, cc) = screen.cursor_position();
        if cr >= row_off
            && cr - row_off < area.height
            && cc < area.width
            && let Some(c) = buf.cell_mut((area.x + cc, area.y + cr - row_off))
        {
            c.set_style(c.style().add_modifier(Modifier::REVERSED));
        }
    }
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_cut_at_the_edge_says_so() {
        let mut p = vt100::Parser::new(3, 12, 0);
        p.process(b"short\r\nlonger one\r\n\x1b[?25l");
        let area = Rect::new(0, 0, 6, 3);
        let mut buf = Buffer::empty(area);
        draw(p.screen(), area, &mut buf);
        let row = |y: u16| {
            (0..6)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect::<String>()
        };
        assert_eq!(row(0), "short ", "fits: nothing added");
        assert_eq!(row(1), "longe›");
        assert!(buf.cell((5, 1)).unwrap().modifier.contains(Modifier::DIM));
    }

    #[test]
    fn draws_text_colour_and_cursor_bottom_aligned() {
        let mut p = vt100::Parser::new(4, 10, 0);
        p.process(b"one\r\ntwo\r\n\x1b[31mred\x1b[0m\r\nlast");
        let area = Rect::new(0, 0, 10, 2);
        let mut buf = Buffer::empty(area);
        draw(p.screen(), area, &mut buf);
        // Rows 2 and 3 of the 4-row screen fit a 2-row pane.
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "r");
        assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::Indexed(1));
        assert_eq!(buf.cell((3, 1)).unwrap().symbol(), "t");
        assert!(
            buf.cell((4, 1)).unwrap().modifier.contains(Modifier::REVERSED),
            "cursor after `last`"
        );

        // Scrolled back, the top of the window shows and the cursor does not.
        let mut p = vt100::Parser::new(2, 10, 100);
        p.process(b"one\r\ntwo\r\nthree\r\nfour");
        p.screen_mut().set_scrollback(2);
        let mut buf = Buffer::empty(area);
        draw(p.screen(), area, &mut buf);
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "o", "two rows back is `one`");
        assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "t");
        assert!(
            !buf.cell((4, 1)).unwrap().modifier.contains(Modifier::REVERSED),
            "no cursor while scrolled back"
        );

        // Content at the top of a tall screen: the cursor decides what shows.
        let mut p = vt100::Parser::new(40, 10, 0);
        p.process(b"ready\r\n> ");
        let mut buf = Buffer::empty(area);
        draw(p.screen(), area, &mut buf);
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "r");
        assert_eq!(buf.cell((0, 1)).unwrap().symbol(), ">");
    }
}
