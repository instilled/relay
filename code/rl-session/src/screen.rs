//! A headless terminal: the run's screen as a viewer would see it.
//!
//! Replaying a tail of output bytes to someone attaching is not handing them
//! a screen. The tail starts in the middle of a redraw, it carries every
//! redraw the app ever made, and an inline app (Gemini CLI, Claude Code) that
//! reprints its history on a resize leaves each reprint in it. Played into a
//! terminal of another size, relative cursor moves land on the wrong lines.
//! So the holder keeps the screen itself, fed every byte, and an attach is
//! handed that screen drawn afresh: the history above it once, then what is
//! on it now.

/// Escape-parsing state carried across chunks so a sequence split across two
/// PTY reads is still recognised. Shared with rl-detect's stripper so the two
/// escape machines cannot drift.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EscState {
    #[default]
    None,
    /// Saw ESC; waiting for the next byte to say what kind of escape this is.
    Escape,
    /// Inside `ESC [ ... final`, waiting for a final byte in 0x40..=0x7E.
    Csi,
    /// Inside `ESC ] ... BEL` or `... ESC \`, waiting for the terminator.
    Osc { prev_was_esc: bool },
    /// Saw `ESC (` or `ESC )`; waiting for the one byte saying which set.
    /// `g0` is true for `(`, the set ordinary text is drawn from.
    Charset { g0: bool },
}

/// An escape sequence still unterminated after this many bytes is treated as
/// malformed and abandoned, so a stray, never-terminated ESC doesn't buffer
/// forever. Shared with rl-detect's stripper.
pub const MAX_PENDING_ESCAPE: usize = 64;
/// An OSC carries text (a window title, a folder, a link), so it is given
/// far longer before it is taken for a stray. Shared with rl-detect's stripper.
pub const MAX_PENDING_OSC: usize = 4096;

/// Whether an escape has run on past what its kind is allowed. Shared with
/// rl-detect's stripper.
pub fn overrun(state: &EscState, pending: usize) -> bool {
    match state {
        EscState::None => false,
        EscState::Osc { .. } => pending > MAX_PENDING_OSC,
        _ => pending > MAX_PENDING_ESCAPE,
    }
}

/// One character of the DEC special graphics set, which a terminal draws in
/// place of the letter while that set is chosen. What the set leaves alone
/// comes back unchanged.
pub fn dec_graphic(c: char) -> char {
    match c {
        '_' => ' ',
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        c => c,
    }
}

/// Rewrites a byte stream so the box-drawing set is drawn rather than spelled,
/// leaving everything else exactly as it stood — colours, cursor moves and all.
///
/// `rl_detect::ansi::Stripper` answers what a run's output *said*; this answers what a
/// terminal would have *shown*, for a screen that draws the bytes itself.
/// `vt100` hands any escape carrying an intermediate byte to its unhandled
/// hook, and `ESC ( 0` is one, so without this a ruled-off prompt arrives as
/// `qqqqqq`.
#[derive(Debug, Clone, Default)]
pub struct Graphics {
    state: EscState,
    pending: usize,
    /// Whether each designated set is the box-drawing one: G0, then G1.
    sets: [bool; 2],
    /// Which of them letters are drawn from; shift-out and shift-in move it.
    shifted: usize,
}

impl Graphics {
    pub fn new() -> Self {
        Self::default()
    }

    /// One chunk, rewritten. An escape left open at the end carries over, so
    /// a sequence split across reads is still recognised.
    pub fn rewrite(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &b in bytes {
            match std::mem::take(&mut self.state) {
                EscState::None => {
                    if b == 0x1b {
                        // Held back: only the next byte says whether this
                        // begins a charset escape, which goes no further.
                        self.state = EscState::Escape;
                        self.pending = 1;
                    } else if b == 0x0e || b == 0x0f {
                        // The shift is acted on here, so it goes no further
                        // either: the screen is handed what it chose.
                        self.shifted = usize::from(b == 0x0e);
                    } else if self.sets[self.shifted] && (0x5f..=0x7e).contains(&b) {
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(dec_graphic(b as char).encode_utf8(&mut buf).as_bytes());
                    } else {
                        out.push(b);
                    }
                }
                EscState::Escape => {
                    self.pending += 1;
                    match b {
                        b'(' | b')' => self.state = EscState::Charset { g0: b == b'(' },
                        _ => {
                            out.push(0x1b);
                            out.push(b);
                            self.state = match b {
                                b'[' => EscState::Csi,
                                b']' => EscState::Osc { prev_was_esc: false },
                                _ => EscState::None,
                            };
                        }
                    }
                }
                EscState::Csi => {
                    self.pending += 1;
                    out.push(b);
                    self.state = if (0x40..=0x7e).contains(&b) {
                        EscState::None
                    } else {
                        EscState::Csi
                    };
                }
                EscState::Osc { prev_was_esc } => {
                    self.pending += 1;
                    out.push(b);
                    self.state = if b == 0x07 || (prev_was_esc && b == b'\\') {
                        EscState::None
                    } else {
                        EscState::Osc {
                            prev_was_esc: b == 0x1b,
                        }
                    };
                }
                EscState::Charset { g0 } => {
                    self.pending += 1;
                    self.sets[usize::from(!g0)] = b == b'0';
                    self.state = EscState::None;
                }
            }
            if overrun(&self.state, self.pending) {
                // Not a well-formed escape after all.
                self.state = EscState::None;
            }
        }
        out
    }
}

/// Rows of history kept above the screen. 32 bytes a cell: 2000 rows of 200
/// columns is about 12 MiB, the most one run holds; tmux keeps as many.
pub const HISTORY_ROWS: usize = 2000;

/// One run's screen, fed every byte it writes.
pub struct Screen {
    parser: vt100::Parser,
    graphics: Graphics,
}

impl Screen {
    /// A zero size is clamped to 1: a terminal with no cells cannot be drawn
    /// on, and the holder may be told one before the window has a size.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows.max(1), cols.max(1), HISTORY_ROWS),
            graphics: Graphics::new(),
        }
    }

    /// Every byte the run writes goes in, in order; an escape split across
    /// two calls carries over, so reads can be cut anywhere.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(&self.graphics.rewrite(bytes));
    }

    /// The window changed size: the snapshot is drawn at the new one, so a
    /// viewer joining after it is handed a screen that fits the app's own.
    /// `vt100` does not reflow (a shrink drops the bottom rows, a widening
    /// leaves history at its old width), so this relies on the app repainting
    /// after a resize, as inline agents do.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.parser.screen_mut().set_size(rows.max(1), cols.max(1));
    }

    /// Bytes that draw this screen on a terminal of its size: the history
    /// first, a line at a time, pushed up out of sight; then the screen
    /// itself, cleared and drawn, on the alternate screen if the app is on
    /// it; then the cursor, colours and input modes the app left set.
    /// History rows that were soft-wrapped go out as hard line breaks, so
    /// the viewer's scrollback does not rewrap them.
    ///
    /// What `vt100` does not model is not carried: the scroll region, origin
    /// mode, charset designations, kitty keyboard and focus reporting. An app
    /// that relies on one draws wrong until its next full redraw, which is
    /// the accepted cost.
    ///
    /// An app on the alternate screen has its main screen and history drawn
    /// first as well, so they are there when it leaves. `vt100` shows only
    /// the alternate grid while it is on, so it is switched off (`?47l` is a
    /// bare mode toggle: no clear, no cursor save) and back on around them.
    pub fn snapshot(&mut self) -> Vec<u8> {
        let alternate = self.parser.screen().alternate_screen();
        if alternate {
            self.parser.process(b"\x1b[?47l");
        }
        let mut out = Vec::new();
        let (rows, cols) = self.parser.screen().size();
        let screen = self.parser.screen_mut();
        screen.set_scrollback(usize::MAX);
        let mut at = screen.scrollback();
        // Oldest first: a viewed page starts `at` rows back.
        while at > 0 {
            screen.set_scrollback(at);
            let take = at.min(rows as usize);
            for row in screen.rows_formatted(0, cols).take(take) {
                out.extend_from_slice(&row);
                out.extend_from_slice(b"\x1b[m\r\n");
            }
            at -= take;
        }
        screen.set_scrollback(0);
        // Up and out of the way, into the viewer's own scrollback, before the
        // screen is cleared and drawn where they stood. With no history too:
        // what the viewer had on their screen is kept, not cleared away.
        out.extend(std::iter::repeat_n(b'\n', rows.saturating_sub(1) as usize));
        if alternate {
            out.extend_from_slice(&screen.contents_formatted());
            self.parser.process(b"\x1b[?47h");
            out.extend_from_slice(b"\x1b[?1049h");
            out.extend_from_slice(&self.parser.screen().state_formatted());
        } else {
            out.extend_from_slice(&screen.state_formatted());
        }
        out
    }

    /// An ended run's screen, from as much of its output as there is.
    pub fn render(bytes: &[u8], cols: u16, rows: u16) -> Vec<u8> {
        let mut s = Self::new(cols, rows);
        s.feed(bytes);
        s.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screen_is_handed_the_box_drawing_rather_than_the_letters() {
        let mut g = Graphics::default();
        assert_eq!(g.rewrite(b"\x1b(0qqq\x1b(Bok"), "───ok".as_bytes());
        // Everything else passes through byte for byte — including a `q`
        // inside a parameter, which is no part of the text at all.
        let mut g = Graphics::default();
        assert_eq!(
            g.rewrite(b"\x1b[31mred\x1b[0m\x1b]0;q\x07"),
            b"\x1b[31mred\x1b[0m\x1b]0;q\x07"
        );
        // The set and a half-read escape both carry across reads.
        let mut g = Graphics::default();
        assert_eq!(g.rewrite(b"\x1b(0l"), "┌".as_bytes());
        assert_eq!(g.rewrite(b"q\x1b("), "─".as_bytes());
        assert_eq!(g.rewrite(b"Bx"), b"x");
        // The way a prompt actually rules its line, taken from a session's own
        // journal: G1 designated, shifted out for the run of `q`s, shifted
        // back. The shifts are acted on here, so the screen never sees them.
        let mut g = Graphics::default();
        assert_eq!(g.rewrite(b"\x1b)0\x0eqq\x0fx"), "──x".as_bytes());
    }

    /// What a terminal shows after being handed `bytes`: its history, then its
    /// screen, as plain text rows with trailing blanks trimmed.
    fn shown(bytes: &[u8], cols: u16, rows: u16) -> Vec<String> {
        let mut p = vt100::Parser::new(rows, cols, 10_000);
        p.process(bytes);
        shown_parser(&mut p, cols, rows)
    }

    fn shown_parser(p: &mut vt100::Parser, cols: u16, rows: u16) -> Vec<String> {
        let s = p.screen_mut();
        s.set_scrollback(usize::MAX);
        let back = s.scrollback();
        let mut out = Vec::new();
        // Oldest first: page down through the history a screenful at a time.
        let mut at = back;
        while at > 0 {
            s.set_scrollback(at);
            let take = at.min(rows as usize);
            out.extend(s.rows(0, cols).take(take).map(|r| r.trim_end().to_string()));
            at -= take;
        }
        s.set_scrollback(0);
        out.extend(s.rows(0, cols).map(|r| r.trim_end().to_string()));
        while out.last().is_some_and(String::is_empty) {
            out.pop();
        }
        out
    }

    #[test]
    fn snapshot_round_trips_history_and_screen() {
        let mut s = Screen::new(20, 5);
        for i in 0..30 {
            s.feed(format!("line {i:02}\r\n").as_bytes());
        }
        s.feed(b"\x1b[1;31mred\x1b[m prompt");
        let want = shown(
            &{
                let mut all = Vec::new();
                for i in 0..30 {
                    all.extend_from_slice(format!("line {i:02}\r\n").as_bytes());
                }
                all.extend_from_slice(b"\x1b[1;31mred\x1b[m prompt");
                all
            },
            20,
            5,
        );
        assert_eq!(shown(&s.snapshot(), 20, 5), want);
        assert_eq!(want.first().unwrap(), "line 00");
        assert_eq!(want.last().unwrap(), "red prompt");
    }

    #[test]
    fn a_line_redrawn_in_place_is_drawn_once() {
        let mut s = Screen::new(40, 6);
        // The blank line is what the first redraw climbs onto; without it the
        // first "up a line" would erase the header.
        s.feed(b"header\r\n\r\n");
        for i in 0..200 {
            // Ink's way: up a line, erase it, write it again.
            s.feed(format!("\x1b[1A\x1b[2K\rframe {i:03}\r\n").as_bytes());
        }
        let snap = String::from_utf8_lossy(&s.snapshot()).into_owned();
        assert!(snap.contains("frame 199"), "{snap:?}");
        assert!(!snap.contains("frame 198"), "{snap:?}");
        assert_eq!(snap.matches("header").count(), 1, "{snap:?}");
    }

    #[test]
    fn an_alternate_screen_app_is_drawn_on_the_alternate_screen() {
        let mut s = Screen::new(20, 4);
        for i in 0..10 {
            s.feed(format!("old {i}\r\n").as_bytes());
        }
        s.feed(b"shell\r\n\x1b[?1049h\x1b[Hfull screen");
        let snap = s.snapshot();
        let text = String::from_utf8_lossy(&snap);
        let alt = text.rfind("\x1b[?1049h").expect("switches to the alternate screen");
        assert!(alt < text.find("full screen").unwrap());
        let mut p = vt100::Parser::new(4, 20, 100);
        p.process(&snap);
        assert!(p.screen().alternate_screen());
        assert!(p.screen().contents().contains("full screen"));
        // Leaving it brings back what was under it: the shell, and the
        // history above the shell.
        p.process(b"\x1b[?1049l");
        assert!(!p.screen().alternate_screen());
        assert!(p.screen().contents().contains("shell"));
        let back = shown_parser(&mut p, 20, 4);
        assert_eq!(back.first().unwrap(), "old 0");
    }

    #[test]
    fn the_box_drawing_set_survives_the_snapshot() {
        let mut s = Screen::new(10, 2);
        s.feed(b"\x1b(0qqq\x1b(Bok");
        let snap = String::from_utf8_lossy(&s.snapshot()).into_owned();
        assert!(snap.contains("───ok"), "{snap:?}");
    }

    #[test]
    fn a_resize_is_the_size_the_snapshot_draws() {
        let mut s = Screen::new(10, 3);
        s.feed(b"abc");
        s.resize(30, 8);
        let mut p = vt100::Parser::new(8, 30, 0);
        p.process(&s.snapshot());
        assert_eq!(p.screen().size(), (8, 30));
        assert!(p.screen().contents().starts_with("abc"));
        assert_eq!(p.screen().cursor_position(), (0, 3));
    }

    /// A run with no history yet still pushes the viewer's own lines up into
    /// their scrollback rather than clearing them away.
    #[test]
    fn what_the_viewer_had_on_screen_is_kept_above() {
        let mut s = Screen::new(20, 4);
        s.feed(b"abc");
        let mut p = vt100::Parser::new(4, 20, 100);
        p.process(b"mine 1\r\nmine 2\r\nmine 3\r\n");
        p.process(&s.snapshot());
        assert_eq!(shown_parser(&mut p, 20, 4), ["mine 1", "mine 2", "mine 3", "abc"]);
    }

    #[test]
    fn render_is_a_fresh_screen_fed_and_drawn() {
        let bytes = b"one\r\ntwo\r\n\x1b[1A\x1b[2Ktwo again";
        let mut s = Screen::new(20, 4);
        s.feed(bytes);
        assert_eq!(Screen::render(bytes, 20, 4), s.snapshot());
    }
}
