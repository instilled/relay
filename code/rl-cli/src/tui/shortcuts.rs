//! `?` on an empty line: the keys for where you are, in a quiet line under
//! the command input — the list's, the graph's, the events view's, a
//! question's. `?` again or `esc` puts it away, and so does typing, so it
//! never takes room while you work. With words on the line `?` is only a
//! character: a question, a note and a search all take it.
//!
//! One table per place, so what the line says stays what the keys do. A key
//! that stands for a `/` command is read from `palette::key`, the same table
//! the commands list shows beside each command.

use super::palette::{self, Cmd};
use super::theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

/// Where the keyboard is, as far as the keys go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// The list.
    List,
    /// The list in the `hidden` lens, where `/hide` brings a row back.
    Hidden,
    /// The list with a question under the cursor.
    Question,
    /// A question taken out of the list, its choices walked in the pane:
    /// how many of them have a number, and whether several may be picked.
    Answer { n: usize, multi: bool },
    /// A run's terminal, watched: beside the list, or the whole screen;
    /// still going, so there is a keyboard to take, or ended.
    Term { wide: bool, live: bool },
    /// The wiring graph.
    Graph,
    /// `/events`.
    Events,
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

/// A key that stands for a `/` command: the key is the palette's.
fn cmd(c: Cmd, what: &'static str) -> Key {
    key(palette::key(c), what)
}

/// The keys in force at a place, the ones used most first.
pub fn keys(at: Place) -> Vec<Key> {
    let moves = key("↑↓ ^j^k", "move");
    let help = key("/help", "");
    match at {
        Place::List => vec![
            moves,
            key("⇧↑↓", "reorder"),
            key("enter", "join/open"),
            cmd(Cmd::Stop, "stop/archive/remove"),
            cmd(Cmd::Pin, "pin"),
            cmd(Cmd::Rename, "rename"),
            cmd(Cmd::Pane, "details"),
            cmd(Cmd::Filter, "search"),
            cmd(Cmd::Graph, "graph"),
            key("tab", "lens"),
            key("/hide", ""),
            help,
        ],
        // The list's own keys, but for the one that brings a row back; the
        // order is the list's to keep, not this lens's.
        Place::Hidden => {
            let mut v = keys(Place::List);
            v.retain(|k| k.what != "reorder" && k.keys != "/hide");
            v.insert(1, key("/hide", "brings it back"));
            v
        }
        Place::Question => vec![
            moves,
            key("enter", "its choices"),
            key("1-9 ↵", "answers, a note after it"),
            cmd(Cmd::Stop, "not this"),
            cmd(Cmd::Pane, "details"),
            cmd(Cmd::Filter, "search"),
            cmd(Cmd::Graph, "graph"),
            key("tab", "lens"),
            help,
        ],
        Place::Answer { n, multi } => {
            let mut v = vec![key("↑↓", "choose"), key("enter", "takes it")];
            match n {
                0 => {}
                1 => v.push(key("1", if multi { "picks" } else { "answers" })),
                n => v.push(key(format!("1-{n}"), if multi { "picks" } else { "answers" })),
            }
            if multi {
                v.push(key("space", "picks"));
            }
            v.push(key("esc", "back"));
            v
        }
        Place::Term { wide, live } => {
            let mut v = Vec::new();
            if live {
                v.push(key("enter", "takes the terminal"));
            }
            v.push(key("pgup/pgdn", "reads back"));
            if wide {
                v.push(key("↑↓", "a line back"));
            } else {
                v.push(moves);
            }
            v.push(cmd(Cmd::Pane, "hides it"));
            v.push(key("esc", "back"));
            v.push(help);
            v
        }
        Place::Graph => vec![
            moves,
            key("←→", "follow the wiring"),
            key("enter", "answer/run/details"),
            cmd(Cmd::Pane, "details"),
            cmd(Cmd::Filter, "search"),
            key("/approve", ""),
            key("/pause", ""),
            key("/edit", ""),
            cmd(Cmd::Graph, "back"),
            key("esc", "back"),
        ],
        Place::Events => vec![
            moves,
            key("pgup/pgdn", "page"),
            key("enter", "its run or rule"),
            cmd(Cmd::Filter, "kind: in: by:"),
            cmd(Cmd::Pane, "details"),
            cmd(Cmd::Graph, "its rule's wiring"),
            key("esc", "back"),
        ],
    }
}

/// What `?` and `esc` do to the shortcuts line before anything else sees the
/// key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    /// `?` on an empty line: shown, or put away.
    Toggle,
    /// `esc` with the line shown: put away, and nothing else — the pane, the
    /// search and the view wait for the next `esc`.
    Close,
    /// Not the shortcuts line's: the key goes on as ever.
    Pass,
}

/// `?` on an empty line toggles the shortcuts; with anything typed it is a
/// character like any other. `esc` closes them first, before it does any of
/// its other jobs.
pub fn take(open: bool, line_empty: bool, k: &KeyEvent) -> Take {
    let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match k.code {
        KeyCode::Char('?') if plain && line_empty => Take::Toggle,
        KeyCode::Esc if open => Take::Close,
        _ => Take::Pass,
    }
}

/// Whether the shortcuts stay up once a key has gone to the line: typing
/// puts them away, so they never take room while you work.
pub fn still_open(open: bool, line_empty: bool) -> bool {
    open && line_empty
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
pub const HINT: &str = "? shortcuts";

/// A footer hint with `? shortcuts` at its end, when there is room for it;
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

    fn q() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)
    }

    #[test]
    fn the_list_says_its_keys() {
        assert_eq!(
            one_line(Place::List),
            "↑↓ ^j^k move · ⇧↑↓ reorder · enter join/open · ^x stop/archive/remove · ^t pin · \
             ^r rename · ^o details · ^f search · ^g graph · tab lens · /hide · /help"
        );
    }

    #[test]
    fn each_place_has_its_own_keys() {
        let graph = one_line(Place::Graph);
        assert!(graph.contains("←→ follow the wiring"), "{graph}");
        assert!(graph.contains("^g back") && graph.contains("esc back"), "{graph}");
        assert!(!graph.contains("tab lens"), "the graph has no lens: {graph}");
        assert!(!graph.contains("reorder"), "{graph}");

        let events = one_line(Place::Events);
        assert!(events.contains("enter its run or rule"), "{events}");
        assert!(events.contains("^f kind: in: by:"), "{events}");
        assert!(!events.contains("←→"), "{events}");

        assert_eq!(
            one_line(Place::Hidden),
            "↑↓ ^j^k move · /hide brings it back · enter join/open · ^x stop/archive/remove · \
             ^t pin · ^r rename · ^o details · ^f search · ^g graph · tab lens · /help"
        );

        let question = one_line(Place::Question);
        assert!(question.contains("enter its choices"), "{question}");
        assert!(question.contains("1-9 ↵ answers"), "{question}");
        assert!(question.contains("^x not this"), "{question}");

        assert_eq!(
            one_line(Place::Answer { n: 3, multi: false }),
            "↑↓ choose · enter takes it · 1-3 answers · esc back"
        );
        assert_eq!(
            one_line(Place::Answer { n: 2, multi: true }),
            "↑↓ choose · enter takes it · 1-2 picks · space picks · esc back"
        );

        let wide = one_line(Place::Term { wide: true, live: true });
        assert!(
            wide.contains("enter takes the terminal") && wide.contains("↑↓ a line back"),
            "{wide}"
        );
        let beside = one_line(Place::Term {
            wide: false,
            live: true,
        });
        assert!(beside.contains("↑↓ ^j^k move"), "{beside}");
        let ended = one_line(Place::Term {
            wide: false,
            live: false,
        });
        assert!(
            !ended.contains("takes the terminal"),
            "an ended run has no keyboard: {ended}"
        );
    }

    #[test]
    fn a_command_key_is_the_one_the_commands_list_shows() {
        let all = [
            Place::List,
            Place::Hidden,
            Place::Question,
            Place::Graph,
            Place::Events,
            Place::Term {
                wide: false,
                live: true,
            },
        ];
        let ours = [
            (Cmd::Stop, "^x"),
            (Cmd::Pin, "^t"),
            (Cmd::Rename, "^r"),
            (Cmd::Pane, "^o"),
            (Cmd::Filter, "^f"),
            (Cmd::Graph, "^g"),
        ];
        for (c, k) in ours {
            assert_eq!(palette::key(c), k, "the palette moved {c:?}");
        }
        for at in all {
            for k in keys(at) {
                if k.keys.starts_with('^') && !k.keys.contains(' ') {
                    let known = ours.iter().any(|(_, s)| *s == k.keys);
                    assert!(known, "{at:?}: {} is not a key the palette has", k.keys);
                }
            }
        }
    }

    #[test]
    fn a_narrow_terminal_wraps_between_keys_and_cuts_none() {
        for at in [Place::List, Place::Graph, Place::Events, Place::Question, Place::Hidden] {
            let ks = keys(at);
            for width in [160, 120, 80, 60, 40, 30] {
                let rows = wrap(&ks, width);
                // Every key, whole and in order, once.
                let flat: Vec<&Key> = rows.iter().flatten().copied().collect();
                assert_eq!(flat.len(), ks.len(), "{at:?} at {width}");
                for (a, b) in flat.iter().zip(&ks) {
                    assert_eq!(*a, b);
                }
                for row in &rows {
                    let t = row_text(row);
                    assert!(width_of(&t) <= width, "{at:?} at {width}: {t:?} is too wide");
                }
            }
        }
        // The list's keys take one row at 160 columns and two at 80.
        assert_eq!(wrap(&keys(Place::List), 160).len(), 1);
        assert_eq!(wrap(&keys(Place::List), 78).len(), 2);
    }

    #[test]
    fn the_drawn_rows_say_what_the_wrapped_ones_do() {
        let ks = keys(Place::List);
        let drawn: Vec<String> = lines(&ks, 78)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let wrapped: Vec<String> = wrap(&ks, 78).iter().map(|r| row_text(r)).collect();
        assert_eq!(drawn, wrapped);
    }

    #[test]
    fn a_question_mark_toggles_only_on_an_empty_line() {
        assert_eq!(take(false, true, &q()), Take::Toggle);
        assert_eq!(take(true, true, &q()), Take::Toggle);
        assert_eq!(
            take(false, false, &q()),
            Take::Pass,
            "with words typed it is a character"
        );
        // A terminal that reports the shift of a `?` still means `?`.
        let shifted = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(take(false, true, &shifted), Take::Toggle);
        let ctrl = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::CONTROL);
        assert_eq!(take(false, true, &ctrl), Take::Pass);
        let other = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        assert_eq!(take(false, true, &other), Take::Pass);
    }

    #[test]
    fn esc_closes_the_shortcuts_before_anything_else() {
        let esc = KeyEvent::from(KeyCode::Esc);
        // Shown: `esc` is taken here, and goes no further — the pane, the
        // filter and the view are the next `esc`'s.
        assert_eq!(take(true, true, &esc), Take::Close);
        // Not shown: `esc` goes on to its other jobs.
        assert_eq!(take(false, true, &esc), Take::Pass);
        assert_eq!(take(false, false, &esc), Take::Pass);
    }

    #[test]
    fn typing_puts_the_shortcuts_away() {
        assert!(!still_open(true, false), "a character on the line closes them");
        assert!(still_open(true, true), "moving about keeps them");
        assert!(!still_open(false, true));
    }

    #[test]
    fn the_footer_points_here_only_where_there_is_room() {
        assert_eq!(with_hint("^o details".into(), 80), "^o details · ? shortcuts");
        assert_eq!(with_hint("^o details".into(), 20), "^o details");
        assert_eq!(with_hint(String::new(), 80), "? shortcuts");
    }
}
