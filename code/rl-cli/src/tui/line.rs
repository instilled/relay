//! The line at the foot of the panel: one place that is always taking text.
//! Typing is the panel's resting state, so the line is never opened and never
//! closed — what you type sits there until `enter` sends it or `esc` clears
//! it. The usual editing keys work: `^a` `^e` to the ends, `^u` `^k` to cut to
//! them, `^w` the word behind (to the last space, as a shell does), and by
//! word — alt or ctrl with `←`/`→`, `alt-b`/`alt-f`, `alt-backspace`,
//! `alt-delete`/`alt-d` — where a word ends at anything that is not a letter
//! or a digit, so a path stops at its slashes. `↑`/`↓` and `home`/`end` are
//! left alone, because the list above wants them. Alt only ever arrives when
//! the terminal sends it (iTerm2: the option key set to `Esc+`); the ctrl
//! arrows work either way.
//!
//! Pure: what is typed, where the cursor is, and what a finished line means.
//! No I/O, and nothing about the rest of the panel.

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

/// What a line says to do, once it is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    /// Nothing typed.
    Empty,
    /// A `/` command, and whatever followed it.
    Slash(String),
    /// A shell line, run as it stands.
    Shell(String),
    /// An agent by name, the rest its prompt.
    Agent { agent: String, prompt: Option<String> },
    /// Plain words: the assistant reads them and drafts what they mean.
    Ask(String),
}

/// The line at the foot of the panel: what is typed, and where the cursor is.
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

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
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

    /// How many rows the text takes, splitting on newlines (one when empty).
    pub fn rows(&self) -> usize {
        self.text.bytes().filter(|&b| b == b'\n').count() + 1
    }

    pub fn key(&mut self, k: &KeyEvent) -> Act {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        // By word: alt is the Mac habit, ctrl the one everywhere else.
        let by_word = alt || ctrl;
        match k.code {
            // Shift-enter (and alt-enter, where a terminal sends that instead)
            // adds a line rather than sending; plain enter still sends. It only
            // arrives distinct from enter on a terminal that reports it — see
            // the keyboard-enhancement flags the panel asks for.
            KeyCode::Enter if k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {
                self.text.insert(self.cursor, '\n');
                self.cursor += 1;
            }
            // A line feed is shift-enter too, where the terminal is set to send
            // one for it (iTerm2's shift-return mapping) — it passes through
            // tmux untouched, where a reported modifier would not. Raw mode
            // reads the byte as ctrl-j, which is the same key.
            KeyCode::Char('j') if ctrl => {
                self.text.insert(self.cursor, '\n');
                self.cursor += 1;
            }
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

/// What a line means. `runnable` answers whether a first word names something
/// this machine can actually run — a binary, a target here — which is what
/// separates a shell line from words meant for the assistant.
pub fn what(line: &str, agents: &[String], runnable: &dyn Fn(&str) -> bool) -> What {
    let line = line.trim();
    if line.is_empty() {
        return What::Empty;
    }
    if let Some(rest) = line.strip_prefix('/') {
        return What::Slash(rest.trim().to_string());
    }
    // A bang forces a shell line when the first word is not one we know.
    if let Some(rest) = line.strip_prefix('!') {
        return What::Shell(rest.trim().to_string());
    }
    let (head, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    if agents.iter().any(|a| a == head) {
        let prompt = unquote(rest.trim());
        return What::Agent {
            agent: head.to_string(),
            prompt: (!prompt.is_empty()).then(|| prompt.to_string()),
        };
    }
    if runnable(head) {
        return What::Shell(line.to_string());
    }
    What::Ask(line.to_string())
}

/// One wrapping pair of double or single quotes off a prompt.
fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')))
        .unwrap_or(s)
}

#[cfg(test)]
mod tests {
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

    fn agents() -> Vec<String> {
        ["claude", "codex"].iter().map(|s| s.to_string()).collect()
    }

    fn nothing_runs(_: &str) -> bool {
        false
    }

    #[test]
    fn typing_fills_the_line_and_moves_the_cursor() {
        let mut l = Line::default();
        assert!(l.is_empty());
        assert_eq!(press(&mut l, "make test"), Act::Typing);
        assert_eq!(l.text(), "make test");
        assert_eq!(l.cursor(), 9);
        assert!(!l.is_empty());
        l.clear();
        assert!(l.is_empty());
        assert_eq!(l.cursor(), 0);
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
        press(&mut l, "héllo 🌍 wörld");
        assert_eq!(l.cursor(), l.text().len());
        assert!(l.text().is_char_boundary(l.cursor()));
        // Back over the emoji word and retype it.
        l.key(&ctrl('w'));
        assert_eq!(l.text(), "héllo 🌍 ");
        l.key(&ctrl('w'));
        assert_eq!(l.text(), "héllo ");
        press(&mut l, "wörld");
        assert_eq!(l.text(), "héllo wörld");

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
    fn shift_enter_adds_a_line_rather_than_sending() {
        let mut l = Line::default();
        press(&mut l, "first");
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(l.key(&shift_enter), Act::Typing);
        press(&mut l, "second");
        assert_eq!(l.text(), "first\nsecond");
        assert_eq!(l.rows(), 2);
        // Alt-enter does the same, for terminals that send that instead.
        assert_eq!(l.key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)), Act::Typing);
        assert_eq!(l.rows(), 3);
        // A terminal that sends a line feed for shift-enter: raw mode reads it
        // as ctrl-j, and it adds a line as well.
        let line_feed = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(l.key(&line_feed), Act::Typing);
        assert_eq!(l.rows(), 4);
        // Plain enter still sends.
        assert_eq!(l.key(&KeyEvent::from(KeyCode::Enter)), Act::Submit);
        // Backspace joins the lines back.
        let mut l = Line::default();
        press(&mut l, "a");
        l.key(&shift_enter);
        assert_eq!((l.text(), l.rows()), ("a\n", 2));
        l.key(&KeyEvent::from(KeyCode::Backspace));
        assert_eq!((l.text(), l.rows()), ("a", 1));
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

    #[test]
    fn an_empty_line_means_nothing() {
        assert_eq!(what("", &agents(), &nothing_runs), What::Empty);
        assert_eq!(what("   ", &agents(), &nothing_runs), What::Empty);
    }

    #[test]
    fn a_slash_is_a_command() {
        assert_eq!(what("/help", &agents(), &nothing_runs), What::Slash("help".into()));
        assert_eq!(
            what("  /new  a run  ", &agents(), &nothing_runs),
            What::Slash("new  a run".into())
        );
        assert_eq!(what("/", &agents(), &nothing_runs), What::Slash(String::new()));
    }

    #[test]
    fn a_bang_forces_a_shell_line() {
        // `hello` is not a binary and not an agent, so only the bang saves it.
        assert_eq!(
            what("hello world", &agents(), &nothing_runs),
            What::Ask("hello world".into())
        );
        assert_eq!(
            what("!hello world", &agents(), &nothing_runs),
            What::Shell("hello world".into())
        );
        // Even an agent's name, behind a bang, is a shell line.
        assert_eq!(
            what("! claude x", &agents(), &nothing_runs),
            What::Shell("claude x".into())
        );
    }

    #[test]
    fn an_agents_name_makes_the_rest_a_prompt() {
        assert_eq!(
            what("claude fix the build", &agents(), &nothing_runs),
            What::Agent {
                agent: "claude".into(),
                prompt: Some("fix the build".into()),
            }
        );
        assert_eq!(
            what("codex", &agents(), &nothing_runs),
            What::Agent {
                agent: "codex".into(),
                prompt: None,
            },
            "nothing after the name is no prompt"
        );
        assert_eq!(
            what("claude   ", &agents(), &nothing_runs),
            What::Agent {
                agent: "claude".into(),
                prompt: None,
            }
        );
        // An agent wins over a binary of the same name.
        assert_eq!(
            what("claude hi", &agents(), &|_| true),
            What::Agent {
                agent: "claude".into(),
                prompt: Some("hi".into()),
            }
        );
    }

    #[test]
    fn a_quoted_prompt_loses_one_wrapping_pair() {
        let quoted = |s: &str| match what(s, &agents(), &nothing_runs) {
            What::Agent { prompt, .. } => prompt,
            other => panic!("not an agent: {other:?}"),
        };
        assert_eq!(quoted(r#"claude "fix the build""#), Some("fix the build".into()));
        assert_eq!(quoted("claude 'fix the build'"), Some("fix the build".into()));
        assert_eq!(
            quoted(r#"claude ""fix it"""#),
            Some(r#""fix it""#.into()),
            "only one pair comes off"
        );
        assert_eq!(
            quoted(r#"claude say "hi" now"#),
            Some(r#"say "hi" now"#.into()),
            "quotes inside stay"
        );
        assert_eq!(
            quoted(r#"claude ""#),
            Some(r#"""#.into()),
            "a lone quote is not a pair, so it stays"
        );
        assert_eq!(quoted("claude    "), None, "only spaces after the name is no prompt");
        assert_eq!(
            quoted(r#"claude "fix' "#),
            Some(r#""fix'"#.into()),
            "mismatched, so kept"
        );
    }

    #[test]
    fn a_word_this_machine_can_run_is_a_shell_line() {
        let runs_make = |w: &str| w == "make";
        assert_eq!(
            what("make test", &agents(), &runs_make),
            What::Shell("make test".into())
        );
        assert_eq!(what("  make  ", &agents(), &runs_make), What::Shell("make".into()));
    }

    #[test]
    fn anything_else_is_words_for_the_assistant() {
        assert_eq!(
            what("run the tests and tell me", &agents(), &nothing_runs),
            What::Ask("run the tests and tell me".into())
        );
        assert_eq!(
            what("  why did nightly fail?  ", &agents(), &nothing_runs),
            What::Ask("why did nightly fail?".into()),
            "trimmed"
        );
        assert_eq!(what("héllo 🌍", &agents(), &nothing_runs), What::Ask("héllo 🌍".into()));
    }
}
