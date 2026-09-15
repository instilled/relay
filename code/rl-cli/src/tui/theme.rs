//! The panel's colours in one place. Accents are saturated mid-tones in
//! truecolor, so they read on a light terminal and a dark one alike; body and
//! quiet text stay on the terminal's own foreground (`Reset`, dimmed), so the
//! panel borrows the user's theme instead of fighting it. Colour still means
//! attention — it is not decoration — but there is more of it than the one
//! yellow the panel used to allow.

use ratatui::style::{Color, Modifier, Style};
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

/// The selected row: the terminal reverses whatever colours the row already
/// has, which reads on both themes without a colour of its own.
pub fn selected() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}
