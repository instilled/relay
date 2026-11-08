//! The search's line (`^f`): one line of text, edited where it stands. The
//! usual editing keys work: `^a` `^e` to the ends, `^u` `^k` to cut to
//! them, `^w` the word behind (to the last space, as a shell does), and by
//! word — alt or ctrl with `←`/`→`, `alt-b`/`alt-f`, `alt-backspace`,
//! `alt-delete`/`alt-d` — where a word ends at anything that is not a letter
//! or a digit, so a path stops at its slashes. `↑`/`↓` and `home`/`end` are
//! left alone, because the list above wants them. Alt only ever arrives when
//! the terminal sends it (iTerm2: the option key set to `Esc+`); the ctrl
//! arrows work either way.
//!
//! `enter` (with or without shift) and `^j` never add a line: the search is
//! one line, and the list takes them.
//!
//! Pure: what is typed and where the cursor is. No I/O, and nothing about the
//! rest of the list.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What a keystroke did to the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Typing,
    Submit,
    Cancel,
    Complete,
    Nothing,
}

/// What is typed, and where the cursor is.
#[derive(Debug, Default, Clone)]
pub struct Line {
    text: String,
    /// A byte index into `text`, always on a char boundary.
    cursor: usize,
}

impl Line {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// A byte index into the text, always on a char boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether nothing at all is typed.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The text in, and the cursor put at a byte index within it. Past the
    /// end, or mid-character, it falls back to the nearest boundary before.
    pub fn set_with_cursor(&mut self, text: impl Into<String>, cursor: usize) {
        self.text = text.into();
        let mut at = cursor.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        self.cursor = at;
    }

    /// Text put in at the cursor, which ends up after it.
    pub fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn key(&mut self, k: &KeyEvent) -> Act {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        // By word: alt is the Mac habit, ctrl the one everywhere else.
        let by_word = alt || ctrl;
        match k.code {
            KeyCode::Enter => return Act::Submit,
            KeyCode::Esc => return Act::Cancel,
            KeyCode::Tab => return Act::Complete,
            KeyCode::Backspace if by_word => {
                let start = self.word_left();
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Backspace => {
                if let Some(i) = self.before() {
                    self.text.remove(i);
                    self.cursor = i;
                }
            }
            KeyCode::Delete if by_word => {
                let end = self.word_right();
                self.text.drain(self.cursor..end);
            }
            KeyCode::Char('d') if alt => {
                let end = self.word_right();
                self.text.drain(self.cursor..end);
            }
            KeyCode::Delete => {
                if self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                }
            }
            KeyCode::Left if by_word => self.cursor = self.word_left(),
            KeyCode::Right if by_word => self.cursor = self.word_right(),
            KeyCode::Char('b') if alt => self.cursor = self.word_left(),
            KeyCode::Char('f') if alt => self.cursor = self.word_right(),
            KeyCode::Left => self.cursor = self.before().unwrap_or(0),
            KeyCode::Right => self.cursor = self.after().unwrap_or(self.text.len()),
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.text.len(),
            KeyCode::Char('u') if ctrl => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('k') if ctrl => self.text.truncate(self.cursor),
            KeyCode::Char('w') if ctrl => {
                let start = self.word_start();
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char(c) if !ctrl => {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            _ => return Act::Nothing,
        }
        Act::Typing
    }

    /// Where the char before the cursor starts, if there is one.
    fn before(&self) -> Option<usize> {
        self.text[..self.cursor].char_indices().next_back().map(|(i, _)| i)
    }

    /// Where the char at the cursor ends, if there is one.
    fn after(&self) -> Option<usize> {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }

    /// Where ctrl-w cuts back to: over any trailing spaces, then the word.
    fn word_start(&self) -> usize {
        let head = self.text[..self.cursor].trim_end_matches(' ');
        head.rfind(' ').map(|i| i + 1).unwrap_or(0)
    }

    /// The word before the cursor: over anything between words, then the word
    /// itself. A word is letters and digits, so `~/Projects/flint` is three of
    /// them and the slashes are between.
    fn word_left(&self) -> usize {
        let head = self.text[..self.cursor].trim_end_matches(|c: char| !c.is_alphanumeric());
        head.trim_end_matches(char::is_alphanumeric).len()
    }

    /// The end of the word after the cursor, the same way round.
    fn word_right(&self) -> usize {
        let tail = self.text[self.cursor..].trim_start_matches(|c: char| !c.is_alphanumeric());
        let tail = tail.trim_start_matches(char::is_alphanumeric);
        self.text.len() - tail.len()
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_paste_goes_in_at_the_cursor() {
        let mut l = Line::default();
        l.set_with_cursor("ab", 1);
        l.insert("xy");
        assert_eq!((l.text(), l.cursor()), ("axyb", 3));
    }

    use super::*;

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn press(l: &mut Line, text: &str) -> Act {
        let mut act = Act::Nothing;
        for c in text.chars() {
            act = l.key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        act
    }

    #[test]
    fn typing_fills_the_line_and_moves_the_cursor() {
        let mut l = Line::default();
        assert!(l.is_empty());
        assert_eq!(press(&mut l, "make test"), Act::Typing);
        assert_eq!(l.text(), "make test");
        assert_eq!(l.cursor(), 9);
        assert!(!l.is_empty());
    }

    #[test]
    fn backspace_and_delete_take_one_char_each() {
        let mut l = Line::default();
        press(&mut l, "abc");
        assert_eq!(l.key(&KeyEvent::from(KeyCode::Backspace)), Act::Typing);
        assert_eq!((l.text(), l.cursor()), ("ab", 2));
        l.key(&KeyEvent::from(KeyCode::Left));
        assert_eq!(l.key(&KeyEvent::from(KeyCode::Delete)), Act::Typing);
        assert_eq!((l.text(), l.cursor()), ("a", 1));
        // At the ends, both are quiet.
        l.key(&KeyEvent::from(KeyCode::Delete));
        assert_eq!(l.text(), "a");
        l.key(&KeyEvent::from(KeyCode::Left));
        l.key(&KeyEvent::from(KeyCode::Backspace));
        assert_eq!((l.text(), l.cursor()), ("a", 0));
    }

    #[test]
    fn left_and_right_stop_at_the_ends() {
        let mut l = Line::default();
        press(&mut l, "ab");
        for _ in 0..5 {
            assert_eq!(l.key(&KeyEvent::from(KeyCode::Left)), Act::Typing);
        }
        assert_eq!(l.cursor(), 0);
        for _ in 0..5 {
            l.key(&KeyEvent::from(KeyCode::Right));
        }
        assert_eq!(l.cursor(), 2);
        press(&mut l, "!");
        assert_eq!(l.text(), "ab!");
    }

    #[test]
    fn ctrl_a_and_ctrl_e_go_to_the_ends() {
        let mut l = Line::default();
        press(&mut l, "make test");
        assert_eq!(l.key(&ctrl('a')), Act::Typing);
        assert_eq!(l.cursor(), 0);
        press(&mut l, "sudo ");
        assert_eq!(l.text(), "sudo make test");
        assert_eq!(l.key(&ctrl('e')), Act::Typing);
        assert_eq!(l.cursor(), 14);
    }

    #[test]
    fn ctrl_u_and_ctrl_k_cut_to_the_ends() {
        let mut l = Line::default();
        press(&mut l, "make test");
        l.key(&ctrl('a'));
        for _ in 0..5 {
            l.key(&KeyEvent::from(KeyCode::Right));
        }
        assert_eq!(l.key(&ctrl('u')), Act::Typing);
        assert_eq!((l.text(), l.cursor()), ("test", 0));

        let mut l = Line::default();
        press(&mut l, "make test");
        l.key(&ctrl('a'));
        for _ in 0..5 {
            l.key(&KeyEvent::from(KeyCode::Right));
        }
        assert_eq!(l.key(&ctrl('k')), Act::Typing);
        assert_eq!((l.text(), l.cursor()), ("make ", 5));
    }

    #[test]
    fn ctrl_w_takes_the_word_before_the_cursor() {
        let mut l = Line::default();
        press(&mut l, "claude fix the build");
        assert_eq!(l.key(&ctrl('w')), Act::Typing);
        assert_eq!((l.text(), l.cursor()), ("claude fix the ", 15));
        l.key(&ctrl('w'));
        assert_eq!(l.text(), "claude fix ");
        // Trailing spaces go with the word.
        let mut l = Line::default();
        press(&mut l, "make test   ");
        l.key(&ctrl('w'));
        assert_eq!((l.text(), l.cursor()), ("make ", 5));
        l.key(&ctrl('w'));
        assert_eq!((l.text(), l.cursor()), ("", 0));
        // On empty, it stays quiet.
        assert_eq!(l.key(&ctrl('w')), Act::Typing);
        assert_eq!(l.text(), "");
        // Mid-line it cuts only back to the start of the word.
        let mut l = Line::default();
        press(&mut l, "make test");
        l.key(&KeyEvent::from(KeyCode::Left));
        l.key(&ctrl('w'));
        assert_eq!((l.text(), l.cursor()), ("make t", 5));
    }

    #[test]
    fn multi_byte_text_survives_every_editing_key() {
        let mut l = Line::default();
        press(&mut l, "héllo 🌍 wörd");
        assert_eq!(l.cursor(), l.text().len());
        assert!(l.text().is_char_boundary(l.cursor()));
        // Back over the emoji word and retype it.
        l.key(&ctrl('w'));
        assert_eq!(l.text(), "héllo 🌍 ");
        l.key(&ctrl('w'));
        assert_eq!(l.text(), "héllo ");
        press(&mut l, "wörd");
        assert_eq!(l.text(), "héllo wörd");

        // Left over a two-byte char, then insert before it.
        let mut l = Line::default();
        press(&mut l, "héllo");
        for _ in 0..4 {
            l.key(&KeyEvent::from(KeyCode::Left));
        }
        assert_eq!(l.cursor(), 1, "before the é, which is two bytes wide");
        press(&mut l, "X");
        assert_eq!(l.text(), "hXéllo");
        l.key(&KeyEvent::from(KeyCode::Delete));
        assert_eq!(l.text(), "hXllo");
        l.key(&KeyEvent::from(KeyCode::Backspace));
        assert_eq!((l.text(), l.cursor()), ("hllo", 1));

        // Backspace over an emoji takes the whole of it.
        let mut l = Line::default();
        press(&mut l, "a🌍");
        l.key(&KeyEvent::from(KeyCode::Backspace));
        assert_eq!((l.text(), l.cursor()), ("a", 1));

        // Walking right across the emoji lands on boundaries only.
        let mut l = Line::default();
        press(&mut l, "é🌍é");
        l.key(&ctrl('a'));
        for _ in 0..4 {
            l.key(&KeyEvent::from(KeyCode::Right));
            assert!(l.text().is_char_boundary(l.cursor()));
        }
        assert_eq!(l.cursor(), l.text().len());
    }

    #[test]
    fn set_puts_the_text_in_and_the_cursor_where_asked() {
        let mut l = Line::default();
        l.set_with_cursor("make test", 9);
        assert_eq!((l.text(), l.cursor()), ("make test", 9));
        l.set_with_cursor("make test", 4);
        assert_eq!(l.cursor(), 4);
        press(&mut l, "s");
        assert_eq!(l.text(), "makes test");
        // Past the end, and mid-character, fall back to a boundary.
        l.set_with_cursor("héllo", 99);
        assert_eq!(l.cursor(), 6);
        l.set_with_cursor("héllo", 3);
        assert_eq!(l.cursor(), 3, "just past the é");
        l.set_with_cursor("héllo", 2);
        assert_eq!(l.cursor(), 1, "mid-é, so the boundary before it");
    }

    #[test]
    fn words_are_walked_and_cut_by_alt_or_ctrl() {
        let key = |code, m| KeyEvent::new(code, m);
        let alt = KeyModifiers::ALT;
        let ctrl = KeyModifiers::CONTROL;
        let mut l = Line::default();
        press(&mut l, "make test ~/Projects/flint");
        // Back a word at a time: the path's slashes are edges too.
        l.key(&key(KeyCode::Left, alt));
        assert_eq!(l.cursor(), 21, "before `flint`");
        l.key(&key(KeyCode::Left, ctrl));
        assert_eq!(l.cursor(), 12, "before `Projects`");
        l.key(&key(KeyCode::Char('b'), alt));
        assert_eq!(l.cursor(), 5, "before `test`");
        // Forward again.
        l.key(&key(KeyCode::Right, alt));
        assert_eq!(l.cursor(), 9, "past `test`");
        l.key(&key(KeyCode::Char('f'), alt));
        assert_eq!(l.cursor(), 20, "past `Projects`");
        // At either end there is nowhere further to go.
        for _ in 0..4 {
            l.key(&key(KeyCode::Right, alt));
        }
        assert_eq!(l.cursor(), l.text().len());
        for _ in 0..9 {
            l.key(&key(KeyCode::Left, alt));
        }
        assert_eq!(l.cursor(), 0);

        // Cutting: the word after, then the words before.
        l.key(&key(KeyCode::Delete, alt));
        assert_eq!(l.text(), " test ~/Projects/flint");
        l.key(&key(KeyCode::Char('d'), alt));
        assert_eq!(l.text(), " ~/Projects/flint");
        l.key(&key(KeyCode::Char('e'), ctrl));
        l.key(&key(KeyCode::Backspace, alt));
        assert_eq!(l.text(), " ~/Projects/");
        l.key(&key(KeyCode::Backspace, ctrl));
        assert_eq!(l.text(), " ~/");
        l.key(&key(KeyCode::Backspace, alt));
        assert_eq!(l.text(), "", "nothing left but the edges");
        assert_eq!(l.cursor(), 0);

        // Alt with any other letter is still that letter.
        let mut l = Line::default();
        assert_eq!(l.key(&key(KeyCode::Char('x'), alt)), Act::Typing);
        assert_eq!(l.text(), "x");
    }

    #[test]
    fn enter_submits_esc_cancels_tab_completes() {
        let mut l = Line::default();
        press(&mut l, "make test");
        assert_eq!(l.key(&KeyEvent::from(KeyCode::Enter)), Act::Submit);
        assert_eq!(l.key(&KeyEvent::from(KeyCode::Esc)), Act::Cancel);
        assert_eq!(l.key(&KeyEvent::from(KeyCode::Tab)), Act::Complete);
        assert_eq!(l.text(), "make test", "none of them touch the text");
    }

    #[test]
    fn the_list_keeps_the_keys_the_line_does_not_want() {
        let mut l = Line::default();
        press(&mut l, "abc");
        for code in [KeyCode::Up, KeyCode::Down, KeyCode::Home, KeyCode::End] {
            assert_eq!(l.key(&KeyEvent::from(code)), Act::Nothing, "{code:?}");
        }
        assert_eq!(l.key(&KeyEvent::from(KeyCode::PageUp)), Act::Nothing);
        assert_eq!(l.key(&KeyEvent::from(KeyCode::BackTab)), Act::Nothing);
        // A ctrl chord the line has no use for is the panel's, not ours.
        assert_eq!(l.key(&ctrl('c')), Act::Nothing);
        assert_eq!((l.text(), l.cursor()), ("abc", 3));
    }
}
