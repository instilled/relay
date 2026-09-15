//! Attention ranking for the fleet (J8): the run that needs you is the top
//! row. Score first, then group so related runs sit together, then the most
//! recent change first.

use jiff::{SignedDuration, Timestamp};
use rl_proto::types::{Run, RunState};

/// Lower is more urgent.
pub fn score(r: &Run) -> u8 {
    match r.state {
        RunState::NeedsInput | RunState::Blocked => 0,
        RunState::Failed => 1,
        RunState::Stale => 2,
        RunState::Finished => 3,
        RunState::Working | RunState::Starting => 4,
        RunState::Idle => 5,
        RunState::Stopped => 6,
    }
}

/// Attention-ranked, grouped, newest change first within a group.
pub fn sort(runs: &mut [Run]) {
    runs.sort_by(|a, b| {
        score(a)
            .cmp(&score(b))
            .then_with(|| a.group_key().cmp(&b.group_key()))
            .then_with(|| a.workspace.cmp(&b.workspace))
            .then_with(|| b.state_at.cmp(&a.state_at))
    });
}

/// What the fleet shows: everything alive, plus what ended within `recent`.
pub fn fleet(runs: Vec<Run>, now: Timestamp, recent: SignedDuration) -> Vec<Run> {
    let mut keep: Vec<Run> = runs
        .into_iter()
        .filter(|r| !r.state.is_terminal() || r.ended_at.is_some_and(|t| now.duration_since(t) <= recent))
        .collect();
    sort(&mut keep);
    keep
}

/// Runs waiting on a human.
pub fn needs_you(runs: &[Run]) -> usize {
    runs.iter().filter(|r| r.state.needs_attention()).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::types::RunKind;

    fn run(id: &str, state: RunState, group: &str, at: &str) -> Run {
        let t: Timestamp = at.parse().unwrap();
        let mut r = Run::new(id, RunKind::Agent, id, vec![], "/w", "/w", t);
        r.state = state;
        r.ref_ = Some(group.into());
        r.state_at = t;
        if state.is_terminal() {
            r.ended_at = Some(t);
        }
        r
    }

    #[test]
    fn the_one_that_needs_you_is_first() {
        let now: Timestamp = "2026-09-08T12:00:00Z".parse().unwrap();
        let runs = vec![
            run("idle", RunState::Idle, "b", "2026-09-08T11:59:00Z"),
            run("work1", RunState::Working, "a", "2026-09-08T11:50:00Z"),
            run("done", RunState::Finished, "c", "2026-09-08T11:00:00Z"),
            run("old", RunState::Finished, "c", "2026-09-07T11:00:00Z"),
            run("ask", RunState::NeedsInput, "b", "2026-09-08T11:58:00Z"),
            run("fail", RunState::Failed, "a", "2026-09-08T11:55:00Z"),
            run("work2", RunState::Working, "a", "2026-09-08T11:57:00Z"),
            run("stale", RunState::Stale, "c", "2026-09-08T11:30:00Z"),
        ];
        let fleet = fleet(runs, now, SignedDuration::from_hours(24));
        let ids: Vec<&str> = fleet.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["ask", "fail", "stale", "done", "work2", "work1", "idle"]);
        assert_eq!(needs_you(&fleet), 2);
    }
}
