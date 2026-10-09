//! What a firing came to: what its last step to run printed — the brief, the proposal — or, for
//! one that failed, what its first failed step printed: the error. A flow's row and its catch-up line say the first line of it; a `catch_up: top` flow's
//! head shows all of it. Read from the step's run each time, never kept.

use super::facts::Facts;
use rl_proto::flow::{Firing, FiringStatus, Outcome, StepState, StepStatus};
use rusqlite::Connection;

/// The most of an outcome's first line a row is sent, in characters.
const LINE_MAX: usize = 200;

/// What a firing came to, by where it stands. Failed: what its first failed step printed — the
/// step its row names, `failed at gchat`. Running: nothing yet; its state says it all. Waiting,
/// done or stopped: the last step in the flow's order that ran and printed something, and what it
/// printed — a step skipped, an ask whose option ran nothing, a run whose journal is gone: passed
/// over for the step before. None while nothing has.
pub(crate) fn of(conn: &Connection, f: &Firing) -> Option<Outcome> {
    let facts = Facts::new(conn, f, None);
    let said = |s: &StepState| -> Option<Outcome> {
        s.run.as_ref()?;
        let text = facts.out(&s.name).filter(|t| !t.is_empty())?;
        Some(Outcome {
            firing: f.id.clone(),
            step: s.name.clone(),
            text,
        })
    };
    match f.status {
        FiringStatus::Running => None,
        FiringStatus::Failed => f.steps.iter().find(|s| s.status == StepStatus::Failed).and_then(said),
        FiringStatus::Waiting | FiringStatus::Done | FiringStatus::Stopped => f
            .steps
            .iter()
            .rev()
            .filter(|s| matches!(s.status, StepStatus::Done | StepStatus::Failed))
            .find_map(said),
    }
}

/// The first line of `text` that says something, trimmed and cut to `LINE_MAX` characters.
pub(crate) fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    match line.char_indices().nth(LINE_MAX) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    }
}

/// `f` as a client is sent it: with the first line of what it came to.
pub(crate) fn told(conn: &Connection, mut f: Firing) -> Firing {
    f.outcome = of(conn, &f).map(|o| first_line(&o.text));
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use rl_ledger::firings::{self, NewFiring};
    use rl_ledger::{Ledger, runs, session_io};
    use rl_proto::types::{Run, RunKind};
    use std::collections::BTreeMap;

    /// The outcome of a firing of `steps` — (name, status, what its run printed; None: no run) —
    /// as the ledger has it.
    fn outcome(steps: &[(&str, StepStatus, Option<&str>)]) -> Option<Outcome> {
        outcome_of(FiringStatus::Done, steps)
    }

    /// As `outcome`, of a firing that stands at `stands`.
    fn outcome_of(stands: FiringStatus, steps: &[(&str, StepStatus, Option<&str>)]) -> Option<Outcome> {
        let steps: Vec<(String, StepStatus, Option<String>)> = steps
            .iter()
            .map(|(n, s, o)| (n.to_string(), *s, o.map(String::from)))
            .collect();
        let ledger = Ledger::open_in_memory().unwrap();
        ledger
            .call_blocking(move |c| {
                let names: Vec<String> = steps.iter().map(|(n, ..)| n.clone()).collect();
                let at = Timestamp::UNIX_EPOCH;
                firings::insert(
                    c,
                    &NewFiring {
                        id: "f1",
                        flow: "brief",
                        def_json: "{}",
                        hash: "h",
                        trigger: "hand",
                        event: None,
                        at,
                        steps: &names,
                    },
                )?;
                let mut status = BTreeMap::new();
                for (n, s, out) in &steps {
                    status.insert(n.clone(), *s);
                    let Some(out) = out else { continue };
                    let id = format!("run-{n}");
                    runs::insert(
                        c,
                        &Run::new(id.clone(), RunKind::Command, n.clone(), vec![], "/w", "/w", at),
                    )?;
                    session_io::append(c, &id, session_io::Direction::Out, out.as_bytes(), at)?;
                    firings::set_step_run(c, "f1", n, &id)?;
                }
                firings::save(c, "f1", &status, stands, Some(at))?;
                let f = firings::get(c, "f1")?.unwrap();
                Ok(of(c, &f))
            })
            .unwrap()
    }

    #[test]
    fn the_last_step_that_ran_and_said_something() {
        use StepStatus::*;
        let o = outcome(&[
            ("gchat", Done, Some("Anna: parser\n")),
            (
                "brief",
                Done,
                Some("\x1b[1m*Morning brief*\x1b[0m\n\n*Anna*\n• the parser\n"),
            ),
        ])
        .unwrap();
        assert_eq!(o.step, "brief");
        assert_eq!(
            o.text, "*Morning brief*\n\n*Anna*\n• the parser",
            "ANSI stripped, trimmed, whole"
        );
        assert_eq!(o.firing, "f1");

        // review ignored: an ask whose option ran nothing; triage's proposal is what it came to.
        let o = outcome(&[
            ("probe", Done, Some("3x connection reset\n")),
            ("triage", Done, Some("restart the api pod\n")),
            ("review", Done, None),
        ])
        .unwrap();
        assert_eq!((o.step.as_str(), o.text.as_str()), ("triage", "restart the api pod"));

        // Still waiting on a question: what triage proposed is what it has come to so far.
        let o = outcome_of(
            FiringStatus::Waiting,
            &[
                ("probe", Done, Some("3x connection reset\n")),
                ("triage", Done, Some("restart the api pod\n")),
                ("review", Waiting, None),
            ],
        )
        .unwrap();
        assert_eq!((o.step.as_str(), o.text.as_str()), ("triage", "restart the api pod"));

        // Nothing found: the probe printed nothing, and the rest was skipped.
        assert_eq!(
            outcome(&[("probe", Done, Some("  \n")), ("triage", Skipped, None)]),
            None
        );
        // Still going: nothing done yet.
        assert_eq!(outcome(&[("probe", Running, None)]), None);
    }

    #[test]
    fn a_failed_firing_came_to_what_its_first_failed_step_printed() {
        use StepStatus::*;
        // morning-brief's order: gchat failed first, so its error is what the firing came to —
        // not days' date, though days ran after it.
        let o = outcome_of(
            FiringStatus::Failed,
            &[
                ("gchat", Failed, Some("token expired\n")),
                ("notion", Done, Some("3 pages\n")),
                ("github", Failed, Some("rate limited\n")),
                ("days", Done, Some("Thu 9 Oct\n")),
                ("brief", Skipped, None),
            ],
        )
        .unwrap();
        assert_eq!((o.step.as_str(), o.text.as_str()), ("gchat", "token expired"));
        // The failed step said nothing: no outcome, rather than another step's words.
        assert_eq!(
            outcome_of(
                FiringStatus::Failed,
                &[("gchat", Failed, Some(" \n")), ("days", Done, Some("Thu 9 Oct\n"))],
            ),
            None
        );
    }

    #[test]
    fn a_running_firing_has_come_to_nothing_yet() {
        use StepStatus::*;
        assert_eq!(
            outcome_of(
                FiringStatus::Running,
                &[
                    ("gchat", Done, Some("Anna: parser\n")),
                    ("days", Done, Some("Thu 9 Oct\n")),
                    ("brief", Running, None),
                ],
            ),
            None,
            "the state alone, not days' date"
        );
    }

    #[test]
    fn its_first_line_is_the_first_that_says_something_cut_for_a_row() {
        assert_eq!(first_line("\n\n  *Morning brief*  \nmore"), "*Morning brief*");
        assert_eq!(first_line(""), "");
        let long = "x".repeat(LINE_MAX + 5);
        let cut = first_line(&long);
        assert_eq!(cut.chars().count(), LINE_MAX + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(
            first_line(&"é".repeat(LINE_MAX)),
            "é".repeat(LINE_MAX),
            "cut by characters"
        );
    }
}
