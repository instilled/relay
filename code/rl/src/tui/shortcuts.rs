//! `^h`: the keys for where you are, in the footer in place of its few hints.
//! `^h` again or `esc` puts them away; so does `?` on an empty command line
//! (once the line has text, `?` is typed), and a search keeps its own keys
//! in the footer.
//!
//! One table per place, so what the footer says stays what the keys do.

use super::theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

/// Where the keyboard is, as far as the keys go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// The list.
    List,
}

/// One key and what it does there, in a word or two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub keys: String,
    pub what: &'static str,
}

fn key(keys: impl Into<String>, what: &'static str) -> Key {
    Key {
        keys: keys.into(),
        what,
    }
}

/// The keys in force at a place, the ones used most first, and last the one
/// that puts them away.
pub fn keys(at: Place) -> Vec<Key> {
    let mut v = place_keys(at);
    v.push(key("^h", ""));
    v
}

fn place_keys(at: Place) -> Vec<Key> {
    match at {
        Place::List => vec![
            key("enter", "attach"),
            key("type", "start an agent or a shell"),
            key("tab", "complete"),
            key("↑↓ ^j^k", "move"),
            key("pgup/pgdn home/end", "page"),
            key("/ ^f", "search"),
            key("^x ^x", "stop/remove/hide"),
            key("^r", "rename"),
            key("^t", "pin"),
            key("⇧↑↓", "reorder"),
            key("a", "agents"),
            key("s", "shells"),
            key("h", "hidden"),
            key("^s", "by project/state"),
            key("^c ^c", "quit"),
        ],
    }
}

/// What `^h`, `?` and `esc` do to the shortcuts before anything else sees the
/// key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    /// `^h`, or `?`: shown, or put away.
    Toggle,
    /// `esc` with them shown: put away, and nothing else — the search waits
    /// for the next `esc`.
    Close,
    /// Not the shortcuts': the key goes on as ever.
    Pass,
}

/// `^h` and a plain `?` toggle the shortcuts; `esc` closes them first,
/// before it does any of its other jobs.
pub fn take(open: bool, k: &KeyEvent) -> Take {
    let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL) && !k.modifiers.contains(KeyModifiers::ALT);
    match k.code {
        KeyCode::Char('h') if ctrl => Take::Toggle,
        KeyCode::Char('?') if plain => Take::Toggle,
        KeyCode::Esc if open => Take::Close,
        _ => Take::Pass,
    }
}

const SEP: &str = " · ";

fn text_of(k: &Key) -> String {
    if k.what.is_empty() {
        k.keys.clone()
    } else {
        format!("{} {}", k.keys, k.what)
    }
}

fn width_of(s: &str) -> usize {
    Span::raw(s).width()
}

/// The keys laid out in rows no wider than `width`, breaking only between
/// keys, so none is ever cut in two. A key too wide for any row has one to
/// itself.
pub fn wrap(keys: &[Key], width: usize) -> Vec<Vec<&Key>> {
    let mut rows: Vec<Vec<&Key>> = Vec::new();
    let mut used = 0;
    for k in keys {
        let w = width_of(&text_of(k));
        match rows.last_mut() {
            Some(row) if used + width_of(SEP) + w <= width => {
                row.push(k);
                used += width_of(SEP) + w;
            }
            _ => {
                rows.push(vec![k]);
                used = w;
            }
        }
    }
    rows
}

/// The rows as they are drawn: the key itself plain, what it does and the
/// dots between quiet.
pub fn lines(keys: &[Key], width: usize) -> Vec<Line<'static>> {
    wrap(keys, width)
        .into_iter()
        .map(|row| {
            let mut spans = Vec::new();
            for (i, k) in row.into_iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(SEP, theme::dim()));
                }
                spans.push(Span::raw(k.keys.clone()));
                if !k.what.is_empty() {
                    spans.push(Span::styled(format!(" {}", k.what), theme::dim()));
                }
            }
            Line::from(spans)
        })
        .collect()
}

/// The footer's pointer to all this.
pub const HINT: &str = "^h shortcuts";

/// A footer hint with `^h shortcuts` at its end, when there is room for it;
/// as it was when there is not.
pub fn with_hint(footer: String, width: usize) -> String {
    let more = if footer.is_empty() {
        HINT.to_string()
    } else {
        format!("{footer}{SEP}{HINT}")
    };
    if width_of(&more) <= width { more } else { footer }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_text(row: &[&Key]) -> String {
        row.iter().map(|k| text_of(k)).collect::<Vec<_>>().join(SEP)
    }

    fn one_line(at: Place) -> String {
        row_text(&keys(at).iter().collect::<Vec<_>>())
    }

    #[test]
    fn the_list_says_its_keys() {
        assert_eq!(
            one_line(Place::List),
            "enter attach · type start an agent or a shell · tab complete · ↑↓ ^j^k move · \
             pgup/pgdn home/end page · / ^f search · \
             ^x ^x stop/remove/hide · ^r rename · ^t pin · ⇧↑↓ reorder · a agents · s shells · \
             h hidden · ^s by project/state · ^c ^c quit · ^h"
        );
    }

    #[test]
    fn a_narrow_terminal_wraps_between_keys_and_cuts_none() {
        let ks = keys(Place::List);
        for width in [160, 120, 80, 60, 40, 30] {
            let rows = wrap(&ks, width);
            // Every key, whole and in order, once.
            let flat: Vec<&Key> = rows.iter().flatten().copied().collect();
            assert_eq!(flat.len(), ks.len(), "at {width}");
            for (a, b) in flat.iter().zip(&ks) {
                assert_eq!(*a, b);
            }
            for row in &rows {
                let t = row_text(row);
                assert!(width_of(&t) <= width, "at {width}: {t:?} is too wide");
            }
        }
        assert_eq!(wrap(&ks, 300).len(), 1);
    }

    #[test]
    fn the_drawn_rows_say_what_the_wrapped_ones_do() {
        let ks = keys(Place::List);
        let drawn: Vec<String> = lines(&ks, 60)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let wrapped: Vec<String> = wrap(&ks, 60).iter().map(|r| row_text(r)).collect();
        assert_eq!(drawn, wrapped);
    }

    #[test]
    fn a_question_mark_and_ctrl_h_toggle_and_esc_closes_first() {
        let q = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(take(false, &q), Take::Toggle);
        assert_eq!(take(true, &q), Take::Toggle);
        // A terminal that reports the shift of a `?` still means `?`.
        let shifted = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(take(false, &shifted), Take::Toggle);
        assert_eq!(
            take(false, &KeyEvent::new(KeyCode::Char('?'), KeyModifiers::CONTROL)),
            Take::Pass
        );
        let h = |m| KeyEvent::new(KeyCode::Char('h'), m);
        assert_eq!(take(false, &h(KeyModifiers::CONTROL)), Take::Toggle);
        assert_eq!(take(false, &h(KeyModifiers::NONE)), Take::Pass, "a plain h is hidden's");
        assert_eq!(take(false, &h(KeyModifiers::CONTROL | KeyModifiers::ALT)), Take::Pass);
        let esc = KeyEvent::from(KeyCode::Esc);
        assert_eq!(take(true, &esc), Take::Close);
        assert_eq!(take(false, &esc), Take::Pass);
    }

    #[test]
    fn the_footer_points_here_only_where_there_is_room() {
        assert_eq!(with_hint("^f search".into(), 80), "^f search · ^h shortcuts");
        assert_eq!(with_hint("^f search".into(), 20), "^f search");
        assert_eq!(with_hint(String::new(), 80), "^h shortcuts");
    }
}
