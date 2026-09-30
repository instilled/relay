//! The panel's colours in one place. Accents are saturated mid-tones in
//! truecolor, so they read on a light terminal and a dark one alike; body and
//! quiet text stay on the terminal's own foreground (`Reset`, dimmed), so the
//! panel borrows the user's theme instead of fighting it. Colour still means
//! attention — it is not decoration — but there is more of it than the one
//! yellow the panel used to allow.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use rl_proto::types::RunState;

/// The one accent: the `relay` word, the prompt caret, a live schedule.
pub const ACCENT: Color = Color::Rgb(0x1D, 0x9E, 0x75); // teal
/// Something waits on you: a question, a run that needs input, a rule's yes.
pub const NEEDS: Color = Color::Rgb(0xC8, 0x8A, 0x1E); // amber
/// Something went wrong: a failed run, a failed job's last outcome.
pub const BAD: Color = Color::Rgb(0xD9, 0x4A, 0x48); // red
/// Something is moving: a working or starting run.
pub const LIVE: Color = Color::Rgb(0x37, 0x8A, 0xDD); // blue
/// Drifted out of touch: a stale run.
pub const STALE: Color = Color::Rgb(0x7F, 0x77, 0xDD); // purple

/// A workspace name and other things worth the eye: bright on both themes.
pub fn name() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// Quiet text — summaries, the age, a folded count: the foreground, dimmed,
/// so it fades on whatever background the terminal has.
pub fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// The accent, for the one thing that carries it on a line.
pub fn accent() -> Style {
    Style::default().fg(ACCENT)
}

/// Something that needs you, spoken in amber and bold.
pub fn needs() -> Style {
    Style::default().fg(NEEDS).add_modifier(Modifier::BOLD)
}

/// Something that is broken, in red and bold: a rules file that did not load.
pub fn bad() -> Style {
    Style::default().fg(BAD).add_modifier(Modifier::BOLD)
}

/// A run's mark and its state word share one colour, by where it stands.
pub fn state(s: RunState) -> Style {
    match s {
        RunState::NeedsInput | RunState::Blocked => needs(),
        RunState::Failed => Style::default().fg(BAD).add_modifier(Modifier::BOLD),
        RunState::Stale => Style::default().fg(STALE),
        RunState::Starting | RunState::Working => Style::default().fg(LIVE),
        RunState::Finished | RunState::Stopped | RunState::Idle => dim(),
    }
}

/// The selected row: a band of its own — deep teal under near-white, both
/// stated outright so it reads the same on a light terminal and a dark one.
/// Reverse video, which this used to use, borrows whatever colours the row
/// already has and came out harsh. A span with a colour of its own — a run's
/// state — still wins over the band's foreground.
pub fn selected() -> Style {
    Style::default()
        .bg(Color::Rgb(0x1D, 0x3A, 0x33))
        .fg(Color::Rgb(0xE8, 0xF0, 0xED))
}

/// Text with a block caret at byte `at` (a char boundary): the character
/// there in reverse video, or a reversed space past the end — a whole cell,
/// the way a terminal draws its own block cursor, so nothing moves aside to
/// make room for it. The rest is drawn in `rest`.
pub fn with_caret(text: &str, at: usize, rest: Style) -> Vec<Span<'static>> {
    let (before, from) = text.split_at(at);
    let mut chars = from.chars();
    let under = chars.next().map_or_else(|| " ".to_string(), |c| c.to_string());
    vec![
        Span::styled(before.to_string(), rest),
        Span::styled(under, Style::default().add_modifier(Modifier::REVERSED)),
        Span::styled(chars.as_str().to_string(), rest),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(spans: &[Span]) -> Vec<String> {
        spans.iter().map(|s| s.content.to_string()).collect()
    }

    fn reversed(s: &Span) -> bool {
        s.style.add_modifier.contains(Modifier::REVERSED)
    }

    #[test]
    fn the_caret_sits_on_the_character_it_is_before() {
        let spans = with_caret("abcd", 2, Style::default());
        assert_eq!(shown(&spans), ["ab", "c", "d"]);
        assert!(reversed(&spans[1]) && !reversed(&spans[0]) && !reversed(&spans[2]));
        // Nothing moved aside for it: the text takes the cells it always did.
        assert_eq!(spans.iter().map(Span::width).sum::<usize>(), 4);
    }

    #[test]
    fn past_the_end_it_is_a_cell_of_its_own() {
        let spans = with_caret("abcd", 4, Style::default());
        assert_eq!(shown(&spans), ["abcd", " ", ""]);
        assert!(reversed(&spans[1]));
        assert_eq!(shown(&with_caret("", 0, Style::default())), ["", " ", ""]);
    }

    #[test]
    fn a_character_of_several_bytes_is_covered_whole() {
        let spans = with_caret("héllo", 1, Style::default());
        assert_eq!(shown(&spans), ["h", "é", "llo"]);
    }

    #[test]
    fn the_text_around_it_keeps_its_style_and_the_caret_does_not() {
        let spans = with_caret("hint", 0, dim());
        assert_eq!(spans[2].style, dim());
        assert!(
            !spans[1].style.add_modifier.contains(Modifier::DIM),
            "a dim caret would all but vanish"
        );
    }
}
