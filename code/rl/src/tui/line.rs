//! The search's line (`^f`): one line of text, edited where it stands. The
//! usual editing keys work: `^e` to the end (`^a` is the agents' filter;
//! the command line takes `home` and `end`), `^u` `^k` to cut to them, `^w` the word behind (to the last space, as a shell does), and by
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
//! A selection (`select_all`, which a name being renamed starts with) stands
//! until a key does something to it: what is typed or pasted takes its place,
//! `backspace` and `delete` take it away, and a move lets it go — `←` to its
//! start, `→` and `^e` to its end, `home` and `end` as ever. `enter`, `esc`
//! and `tab` leave it be.
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

/// What is typed, where the cursor is, and what of it is selected.
#[derive(Debug, Default, Clone)]
pub struct Line {
    text: String,
    /// A byte index into `text`, always on a char boundary.
    cursor: usize,
    /// What is selected, by byte indices on char boundaries; none, most of
    /// the time.
    selected: Option<std::ops::Range<usize>>,
}

impl Line {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// A byte index into the text, always on a char boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// What is selected, if anything.
    pub fn selection(&self) -> Option<std::ops::Range<usize>> {
        self.selected.clone()
    }

    /// All of the text selected, the cursor at its end: what is typed next
    /// takes its place. Nothing typed, nothing selected.
    pub fn select_all(&mut self) {
        self.cursor = self.text.len();
        self.selected = (!self.text.is_empty()).then(|| 0..self.text.len());
    }

    /// The selection taken out, the cursor where it was; whether there was one.
    fn cut_selection(&mut self) -> bool {
        match self.selected.take() {
            Some(r) => {
                self.text.drain(r.clone());
                self.cursor = r.start;
                true
            }
            None => false,
        }
    }

    /// Whether nothing at all is typed.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The text in, and the cursor put at a byte index within it. Past the
    /// end, or mid-character, it falls back to the nearest boundary before.
    pub fn set_with_cursor(&mut self, text: impl Into<String>, cursor: usize) {
        self.text = text.into();
        self.selected = None;
        let mut at = cursor.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        self.cursor = at;
    }

    /// The cursor to the start.
    pub fn home(&mut self) {
        self.selected = None;
        self.cursor = 0;
    }

    /// The cursor to the end.
    pub fn end(&mut self) {
        self.selected = None;
        self.cursor = self.text.len();
    }

    /// Text put in at the cursor, which ends up after it; in place of the
    /// selection, where there is one.
    pub fn insert(&mut self, s: &str) {
        self.cut_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn key(&mut self, k: &KeyEvent) -> Act {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        // By word: alt is the Mac habit, ctrl the one everywhere else.
        let by_word = alt || ctrl;
        if self.selected.is_some()
            && let Some(act) = self.key_on_selection(k, ctrl, alt)
        {
            return act;
        }
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

    /// A key while something is selected, where the selection decides what
    /// it does: typing and pasting replace it, `backspace` and `delete` take
    /// it, a move lets it go and goes to its edge. `None`: the key acts as
    /// ever (the selection let go first, unless it is `enter`, `esc`, `tab`
    /// or a key the line has no use for).
    fn key_on_selection(&mut self, k: &KeyEvent, ctrl: bool, alt: bool) -> Option<Act> {
        let r = self.selected.clone()?;
        match k.code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Tab => return None,
            KeyCode::Backspace | KeyCode::Delete => {
                self.cut_selection();
            }
            KeyCode::Char('b') if alt => self.go(r.start),
            KeyCode::Char('f') if alt => self.go(r.end),
            KeyCode::Char('e') if ctrl => self.go(r.end),
            KeyCode::Left => self.go(r.start),
            KeyCode::Right => self.go(r.end),
            // Another edit: on the text as it stands, the selection let go.
            KeyCode::Char('u' | 'k' | 'w') if ctrl => {
                self.selected = None;
                return None;
            }
            KeyCode::Char('d') if alt => {
                self.selected = None;
                return None;
            }
            KeyCode::Char(c) if !ctrl => self.insert(&c.to_string()),
            _ => return None,
        }
        Some(Act::Typing)
    }

    /// The cursor to `at`, the selection let go.
    fn go(&mut self, at: usize) {
        self.selected = None;
        self.cursor = at;
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
    fn home_and_ctrl_e_go_to_the_ends() {
        let mut l = Line::default();
        press(&mut l, "make test");
        l.home();
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
        l.home();
        for _ in 0..5 {
            l.key(&KeyEvent::from(KeyCode::Right));
        }
        assert_eq!(l.key(&ctrl('u')), Act::Typing);
        assert_eq!((l.text(), l.cursor()), ("test", 0));

        let mut l = Line::default();
        press(&mut l, "make test");
        l.home();
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
        l.home();
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

    fn selected(text: &str) -> Line {
        let mut l = Line::default();
        l.set_with_cursor(text, text.len());
        l.select_all();
        l
    }

    #[test]
    fn select_all_selects_the_text_with_the_cursor_at_its_end() {
        let l = selected("héllo");
        assert_eq!((l.selection(), l.cursor()), (Some(0..6), 6));
        assert_eq!(selected("").selection(), None, "nothing to select");
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut l = selected("shop-sh");
        assert_eq!(press(&mut l, "b"), Act::Typing);
        assert_eq!((l.text(), l.cursor(), l.selection()), ("b", 1, None));
        press(&mut l, "ox");
        assert_eq!(l.text(), "box", "only the first key replaces");
        // Alt with a letter that is no word key types, and replaces too.
        let mut l = selected("shop");
        l.key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
        assert_eq!(l.text(), "x");
    }

    #[test]
    fn a_paste_over_a_selection_replaces_it() {
        let mut l = selected("shop-sh");
        l.insert("build box");
        assert_eq!((l.text(), l.cursor(), l.selection()), ("build box", 9, None));
    }

    #[test]
    fn backspace_or_delete_on_a_selection_clears_it() {
        for code in [KeyCode::Backspace, KeyCode::Delete] {
            let mut l = selected("shop-sh");
            assert_eq!(l.key(&KeyEvent::from(code)), Act::Typing);
            assert_eq!((l.text(), l.cursor(), l.selection()), ("", 0, None), "{code:?}");
        }
        let mut l = selected("make test");
        l.key(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(l.text(), "", "by word, the selection only");
    }

    #[test]
    fn a_move_lets_a_selection_go_and_keeps_the_text() {
        let mut l = selected("shop");
        assert_eq!(l.key(&KeyEvent::from(KeyCode::Left)), Act::Typing);
        assert_eq!((l.text(), l.cursor(), l.selection()), ("shop", 0, None), "to its start");
        press(&mut l, "my ");
        assert_eq!(l.text(), "my shop", "then typing edits");
        let mut l = selected("shop");
        l.key(&KeyEvent::from(KeyCode::Right));
        assert_eq!((l.cursor(), l.selection()), (4, None), "to its end");
        press(&mut l, "-sh");
        assert_eq!(l.text(), "shop-sh");
        let mut l = selected("shop");
        l.home();
        assert_eq!((l.cursor(), l.selection()), (0, None));
        let mut l = selected("shop");
        l.end();
        assert_eq!((l.cursor(), l.selection()), (4, None));
        let mut l = selected("shop");
        l.key(&ctrl('e'));
        assert_eq!((l.text(), l.cursor(), l.selection()), ("shop", 4, None));
        let mut l = selected("make test");
        l.key(&ctrl('w'));
        assert_eq!((l.text(), l.selection()), ("make ", None), "another edit on the text");
    }

    #[test]
    fn enter_esc_tab_and_keys_the_line_has_no_use_for_leave_a_selection_be() {
        let mut l = selected("shop");
        assert_eq!(l.key(&KeyEvent::from(KeyCode::Enter)), Act::Submit);
        assert_eq!(l.key(&KeyEvent::from(KeyCode::Esc)), Act::Cancel);
        assert_eq!(l.key(&KeyEvent::from(KeyCode::Tab)), Act::Complete);
        assert_eq!(l.key(&KeyEvent::from(KeyCode::Up)), Act::Nothing);
        assert_eq!(l.key(&ctrl('c')), Act::Nothing);
        assert_eq!((l.text(), l.selection()), ("shop", Some(0..4)));
        l.set_with_cursor("other", 5);
        assert_eq!(l.selection(), None, "new text, no selection");
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
