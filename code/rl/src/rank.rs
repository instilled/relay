//! Attention ranking: what needs you comes first. The list keeps its rows
//! where they came; this orders only what comes through from hiding.

use crate::sources::Row;
use rl_core::state::RunState;

/// Lower is more urgent.
pub fn score(r: &Row) -> u8 {
    state_score(r.state)
}

/// What `score` gives a row in this state.
pub fn state_score(s: RunState) -> u8 {
    match s {
        RunState::NeedsInput | RunState::Blocked => 0,
        RunState::Failed => 1,
        RunState::Stale => 2,
        RunState::Finished => 3,
        RunState::Working | RunState::Starting => 4,
        RunState::Idle => 5,
        RunState::Stopped => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_one_that_needs_you_is_first() {
        let mut states = RunState::ALL.to_vec();
        states.sort_by_key(|s| state_score(*s));
        assert_eq!(
            states,
            [
                RunState::NeedsInput,
                RunState::Blocked,
                RunState::Failed,
                RunState::Stale,
                RunState::Finished,
                RunState::Starting,
                RunState::Working,
                RunState::Idle,
                RunState::Stopped,
            ]
        );
    }
}
