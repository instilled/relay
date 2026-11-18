//! The list's keys, in one table: `^h` shows them all as a grid under the
//! command line, in place of the footer's hints, the way `claude agents`
//! does (`ctrl+r to rename`), and the footer's hints are a few of the same
//! entries — so neither can say a key the other does not. `^h` again or
//! `F1` puts the grid away (`F1` is there for terminals that send `^h` as
//! backspace), and so does `esc`.

use super::theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};

/// Which key, for the footer to name one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Id {
    Enter,
    Peek,
    Detach,
    Start,
    Find,
    Commands,
    Complete,
    Newline,
    Move,
    Page,
    Reorder,
    Group,
    Rename,
    Pin,
    Colour,
    Remove,
    Agents,
    Shells,
    Hidden,
    Project,
    Setup,
    Report,
    Quit,
    Help,
}

/// One key and what it does: `keys to does`, as `claude agents` says it.
#[derive(Debug)]
pub struct Key {
    pub id: Id,
    pub keys: &'static str,
    pub does: &'static str,
}

const fn key(id: Id, keys: &'static str, does: &'static str) -> Key {
    Key { id, keys, does }
}

/// Every key the list has, the ones used most first; the grid fills its
/// rows in this order.
pub const KEYS: [Key; 24] = [
    key(Id::Enter, "enter", "to attach"),
    key(Id::Peek, "space", "to peek"),
    key(Id::Detach, "ctrl+\\", "to detach"),
    key(Id::Find, "ctrl+f", "to find"),
    key(Id::Commands, "/", "for commands"),
    key(Id::Complete, "tab", "to complete"),
    key(Id::Start, "type", "to start an agent"),
    key(Id::Newline, "shift+enter", "for a new line"),
    key(Id::Move, "↑↓ ctrl+j/k", "to move"),
    key(Id::Page, "pgup/pgdn/home/end", "to page"),
    key(Id::Reorder, "shift+↑↓", "to reorder"),
    key(Id::Group, "ctrl+s", "to switch views"),
    key(Id::Rename, "ctrl+r", "to rename"),
    key(Id::Pin, "ctrl+t", "to pin to top"),
    key(Id::Colour, "ctrl+g", "to colour"),
    key(Id::Remove, "ctrl+x twice", "to stop/rm/hide"),
    key(Id::Agents, "ctrl+a", "to show agents"),
    key(Id::Shells, "ctrl+l", "to show shells"),
    key(Id::Hidden, "ctrl+o", "to show hidden"),
    key(Id::Project, "/project", "to add a project"),
    key(Id::Setup, "/setup", "to set up relay"),
    key(Id::Report, "/report", "to report drift"),
    key(Id::Quit, "ctrl+c twice", "to quit"),
    key(Id::Help, "ctrl+h/f1", "to close"),
];

/// The few keys `rl setup` teaches (its "keys you'll use" card), each
/// with the words the card says beside it, in the card's order: two
/// columns, row by row. The keys themselves are the table's, so the card
/// cannot say a key the list does not have.
pub const ESSENTIAL: [(Id, &str); 6] = [
    (Id::Move, "move"),
    (Id::Enter, "attach"),
    (Id::Reorder, "reorder"),
    (Id::Detach, "detach, back to the list"),
    (Id::Start, "start an agent"),
    (Id::Help, "all keys"),
];

/// The keys `rl setup`'s quick way names in one line, with its words.
pub const ESSENTIAL_LINE: [(Id, &str); 4] = [
    (Id::Enter, "attach"),
    (Id::Detach, "detach"),
    (Id::Move, "move"),
    (Id::Help, "all keys"),
];

pub fn get(id: Id) -> &'static Key {
    KEYS.iter().find(|k| k.id == id).expect("every id is in the table")
}

/// A key in words: `ctrl+r to rename`.
pub fn text(k: &Key) -> String {
    format!("{} {}", k.keys, k.does)
}

/// A key in the footer's words: its keys, and what it does there and then
/// (`ctrl+x twice to remove`), or what the table says.
pub fn hint(id: Id, does: Option<&str>) -> String {
    let k = get(id);
    format!("{} {}", k.keys, does.unwrap_or(k.does))
}

/// What `^h`, `F1` and `esc` do to the grid before anything else sees the
/// key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    /// `^h` or `F1`: shown, or put away.
    Toggle,
    /// `esc` with it shown: put away, and nothing else — a line being typed
    /// waits for the next `esc`.
    Close,
    /// Not the grid's: the key goes on as ever.
    Pass,
}

/// `^h` and `F1` toggle the grid — `F1` for the terminals that send `^h`
/// as backspace; `esc` closes it first, before it does any of its other
/// jobs. `?` is a letter like any other: it types.
pub fn take(open: bool, k: &KeyEvent) -> Take {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL) && !k.modifiers.contains(KeyModifiers::ALT);
    match k.code {
        KeyCode::Char('h') if ctrl => Take::Toggle,
        KeyCode::F(1) => Take::Toggle,
        KeyCode::Esc if open => Take::Close,
        _ => Take::Pass,
    }
}

fn width_of(s: &str) -> usize {
    Span::raw(s).width()
}

/// At least this much air after the widest entry in its column.
const GAP: usize = 6;
/// No more columns than this, however wide the terminal.
const MOST: usize = 4;

/// How many columns the grid has `width` wide: as many as fit the widest
/// entry and its air, from one to four.
pub fn columns(width: usize) -> usize {
    let widest = KEYS.iter().map(|k| width_of(&text(k))).max().unwrap_or(0);
    (width / (widest + GAP)).clamp(1, MOST)
}

/// The grid's rows, `width` wide: the entries in even columns across the
/// width, row by row; each key plain and what it does quiet.
pub fn grid(width: usize) -> Vec<Line<'static>> {
    let cols = columns(width);
    let colw = width / cols;
    KEYS.chunks(cols)
        .map(|row| {
            let mut spans = Vec::new();
            for (i, k) in row.iter().enumerate() {
                spans.push(Span::raw(k.keys));
                spans.push(Span::styled(format!(" {}", k.does), theme::dim()));
                if i + 1 < row.len() {
                    spans.push(Span::raw(" ".repeat(colw.saturating_sub(width_of(&text(k))))));
                }
            }
            Line::from(spans)
        })
        .collect()
}

const SEP: &str = " · ";

/// The footer's one row: the hints, the most useful first, as many as fit
/// `width` with room kept for the pointer to the grid at the end; the
/// pointer too where it fits.
pub fn footer(hints: &[String], width: usize) -> String {
    let more = hint(Id::Help, Some("for help"));
    let fit = |room: usize| {
        let mut out = String::new();
        for h in hints {
            let next = if out.is_empty() {
                h.clone()
            } else {
                format!("{out}{SEP}{h}")
            };
            if width_of(&next) > room {
                break;
            }
            out = next;
        }
        out
    };
    let kept = width.saturating_sub(width_of(SEP) + width_of(&more));
    let out = fit(kept);
    if out.is_empty() && width_of(&more) <= width {
        return more;
    }
    if out.is_empty() {
        return fit(width);
    }
    format!("{out}{SEP}{more}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn every_entry_reads_keys_to_what_they_do_in_lowercase() {
        for k in &KEYS {
            let t = text(k);
            assert!(t.contains(" to ") || t.contains(" for "), "{t}");
            assert_eq!(k.keys, k.keys.to_lowercase(), "{t}");
        }
        assert_eq!(text(get(Id::Rename)), "ctrl+r to rename");
        assert_eq!(hint(Id::Remove, Some("to stop")), "ctrl+x twice to stop");
    }

    #[test]
    fn the_grid_has_four_columns_wide_and_one_narrow_and_cuts_no_entry() {
        assert_eq!(columns(138), 4);
        assert_eq!(columns(78), 2);
        assert_eq!(columns(58), 1);
        for width in [200, 138, 120, 100, 78, 58, 40] {
            let rows = grid(width);
            let cols = columns(width);
            assert_eq!(rows.len(), KEYS.len().div_ceil(cols), "at {width}");
            let all: String = rows.iter().map(drawn).collect::<Vec<_>>().join("\n");
            for k in &KEYS {
                assert!(all.contains(&text(k)), "at {width}: {} is cut", text(k));
            }
            for r in &rows {
                assert!(r.width() <= width, "at {width}: {}", drawn(r));
            }
        }
    }

    #[test]
    fn the_columns_start_evenly_across_the_width() {
        let rows = grid(138);
        let first = drawn(&rows[0]);
        let at: Vec<usize> = KEYS[..4]
            .iter()
            .map(|k| first[..first.find(&text(k)).unwrap()].chars().count())
            .collect();
        assert_eq!(at, [0, 34, 68, 102]);
    }

    #[test]
    fn every_essential_key_is_in_the_table() {
        for (id, _) in ESSENTIAL.iter().chain(ESSENTIAL_LINE.iter()) {
            assert!(KEYS.iter().any(|k| k.id == *id), "{id:?}");
        }
        assert_eq!(get(Id::Detach).keys, "ctrl+\\");
        assert_eq!(get(Id::Move).keys, "↑↓ ctrl+j/k");
    }

    #[test]
    fn ctrl_h_and_f1_toggle_a_question_mark_types_and_esc_closes_first() {
        let q = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(take(false, &q), Take::Pass, "a ? is typed");
        assert_eq!(
            take(true, &KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT)),
            Take::Pass
        );
        let f1 = KeyEvent::from(KeyCode::F(1));
        assert_eq!(take(false, &f1), Take::Toggle);
        assert_eq!(take(true, &f1), Take::Toggle);
        let h = |m| KeyEvent::new(KeyCode::Char('h'), m);
        assert_eq!(take(false, &h(KeyModifiers::CONTROL)), Take::Toggle);
        assert_eq!(take(false, &h(KeyModifiers::NONE)), Take::Pass, "a plain h is typed");
        assert_eq!(take(false, &h(KeyModifiers::CONTROL | KeyModifiers::ALT)), Take::Pass);
        let esc = KeyEvent::from(KeyCode::Esc);
        assert_eq!(take(true, &esc), Take::Close);
        assert_eq!(take(false, &esc), Take::Pass);
    }

    #[test]
    fn the_footer_keeps_the_first_hints_that_fit_and_points_to_the_grid() {
        let hints: Vec<String> = vec![hint(Id::Enter, None), hint(Id::Find, None), hint(Id::Commands, None)];
        assert_eq!(
            footer(&hints, 120),
            "enter to attach · ctrl+f to find · / for commands · ctrl+h/f1 for help"
        );
        assert_eq!(
            footer(&hints, 60),
            "enter to attach · ctrl+f to find · ctrl+h/f1 for help"
        );
        assert_eq!(footer(&hints, 30), "ctrl+h/f1 for help");
        assert_eq!(footer(&hints, 16), "enter to attach");
    }
}
