//! What a firing does next: its steps moved on from what happened, and what
//! the daemon is to carry out.
//!
//! A replayed event is harmless: anything about a step that is not where the
//! event expects it changes nothing and asks for nothing.

use crate::def::{Does, FlowDef};
use crate::template::Lookup;
pub use rl_proto::flow::{FiringStatus, StepStatus};
use std::collections::BTreeMap;

/// What happened to a firing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Happened {
    Started,
    /// A step's run ended; `ok` when it finished well.
    Ended {
        step: String,
        ok: bool,
    },
    /// A step's run was stopped by hand: the firing stops.
    Stopped {
        step: String,
    },
    /// An ask step was answered with an option that runs something.
    Answered {
        step: String,
    },
    /// An ask step was answered with an option that does nothing.
    AnsweredNothing {
        step: String,
    },
    /// An ask step was dismissed, or expired with no default.
    Unanswered {
        step: String,
    },
    Lost {
        step: String,
    },
    Resumed {
        step: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Do {
    /// Start this step's run (a `Run` step).
    Start(String),
    /// Raise this step's question.
    Ask(String),
    /// Start the chosen option's run, as this step's own.
    RunOption(String),
    /// The firing is over.
    End(FiringStatus),
}

/// Moves `steps` on from what happened and says what to do. `facts` answers the `if:`s.
/// Pure: calling it twice with the same `Happened` on the result of the first does nothing new.
pub fn advance(def: &FlowDef, steps: &mut BTreeMap<String, StepStatus>, h: &Happened, facts: &dyn Lookup) -> Vec<Do> {
    use StepStatus::*;
    let mut todo = Vec::new();
    match h {
        Happened::Started => {
            if !steps.is_empty() {
                return todo;
            }
            steps.extend(def.steps.iter().map(|s| (s.name.clone(), Pending)));
        }
        Happened::Ended { step, ok } => {
            if !moved(steps, step, Running, if *ok { Done } else { Failed }) {
                return todo;
            }
        }
        Happened::Stopped { step } => {
            if !moved(steps, step, Running, Failed) {
                return todo;
            }
            for s in steps.values_mut().filter(|s| **s == Pending) {
                *s = Skipped;
            }
        }
        Happened::Answered { step } => {
            if !moved(steps, step, Waiting, Running) {
                return todo;
            }
            todo.push(Do::RunOption(step.clone()));
        }
        Happened::AnsweredNothing { step } => {
            if !moved(steps, step, Waiting, Done) {
                return todo;
            }
        }
        Happened::Unanswered { step } => {
            if !moved(steps, step, Waiting, Skipped) {
                return todo;
            }
        }
        Happened::Lost { step } => {
            if !moved(steps, step, Running, Lost) {
                return todo;
            }
        }
        Happened::Resumed { step } => {
            if !moved(steps, step, Lost, Running) {
                return todo;
            }
        }
    }
    todo.extend(ready(def, steps, facts));
    if !steps.values().copied().any(live) {
        todo.push(Do::End(match h {
            Happened::Stopped { .. } => FiringStatus::Stopped,
            _ => over(steps),
        }));
    }
    todo
}

/// The firing's status from its steps (for the ledger row and the list). A step going makes
/// it running; else a question or a lost step makes it waiting, steps pending after them
/// or not; else steps pending (not started yet) make it running; else it is over. A stop
/// is not told apart from a failure here: the stopped step is `Failed` either way.
pub fn status(steps: &BTreeMap<String, StepStatus>) -> FiringStatus {
    use StepStatus::*;
    let any = |want: StepStatus| steps.values().any(|s| *s == want);
    if any(Running) {
        FiringStatus::Running
    } else if any(Waiting) || any(Lost) {
        FiringStatus::Waiting
    } else if any(Pending) {
        FiringStatus::Running
    } else {
        over(steps)
    }
}

/// `step` moved from `from` to `to`; false, and nothing changed, when it was not at `from`.
fn moved(steps: &mut BTreeMap<String, StepStatus>, step: &str, from: StepStatus, to: StepStatus) -> bool {
    match steps.get_mut(step) {
        Some(s) if *s == from => {
            *s = to;
            true
        }
        _ => false,
    }
}

/// Every pending step that can be decided now, decided, until none can: one after a step
/// that did not finish well is skipped, its `if:` unasked; one whose steps before are all
/// done is skipped when its `if:` is false, else started or asked. Steps ready together
/// come out in the order they are written.
fn ready(def: &FlowDef, steps: &mut BTreeMap<String, StepStatus>, facts: &dyn Lookup) -> Vec<Do> {
    use StepStatus::*;
    let mut todo = Vec::new();
    let mut changed = true;
    while changed {
        changed = false;
        for step in &def.steps {
            if steps.get(&step.name) != Some(&Pending) {
                continue;
            }
            let at = |a: &String| steps.get(a).copied();
            let next = if step.after.iter().any(|a| matches!(at(a), Some(Skipped | Failed))) {
                Skipped
            } else if !step.after.iter().all(|a| at(a) == Some(Done)) {
                continue;
            } else if step.if_.as_ref().is_some_and(|c| !c.holds(facts)) {
                Skipped
            } else {
                match step.does {
                    Does::Run(_) => {
                        todo.push(Do::Start(step.name.clone()));
                        Running
                    }
                    Does::Ask(_) => {
                        todo.push(Do::Ask(step.name.clone()));
                        Waiting
                    }
                }
            };
            steps.insert(step.name.clone(), next);
            changed = true;
        }
    }
    todo
}

/// Not over yet: pending, going, or waiting on a person.
fn live(s: StepStatus) -> bool {
    matches!(
        s,
        StepStatus::Pending | StepStatus::Running | StepStatus::Waiting | StepStatus::Lost
    )
}

/// How a firing with nothing left to do ended.
fn over(steps: &BTreeMap<String, StepStatus>) -> FiringStatus {
    if steps.values().any(|s| *s == StepStatus::Failed) {
        FiringStatus::Failed
    } else {
        FiringStatus::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::{Ref, tests::map};
    use crate::{def, doc};

    /// The spec's prod-logs `flow.md`.
    const PROD_LOGS: &str = r#"---
in: ~/Projects/flint-platform
when: every 30m
steps:
  probe:
    run: new-log-errors.sh
  triage:
    after: probe
    if: probe.out
    run: { agent: claude }
  review:
    after: triage
    ask: "{triage.out}"
    group: "{probe.signature}"
    options:
      apply:  { run: { agent: claude } }
      ignore: {}
    default: ignore
    expires: 4h
---

## triage
Analyse the new errors and warnings below. Propose exactly one action, in two lines at most.

{probe.out}

## review.apply
Do this, in this repository: {triage.out}

What was said with the yes: {review.note}
"#;

    /// The spec's morning-brief `flow.md`, with its agents' sections written out.
    const MORNING_BRIEF: &str = r#"---
when: "30 7 * * 1-5"
steps:
  gchat:  { run: { agent: claude, args: [--allowedTools, "mcp__gchat__*,Bash(rl flow state:*)"] } }
  notion: { run: { agent: claude, args: [--allowedTools, "mcp__notion__*,Bash(rl flow state:*)"] } }
  github: { run: "gh search prs --involves=@me --updated=>$(date -v-1d +%F) --json title,author,url" }
  brief:
    after: [gchat, notion, github]
    run: { agent: claude }
---

## gchat
What did each of my teammates talk about in gchat yesterday? One line each.

## notion
What did each of my teammates change in Notion yesterday? One line each.

## brief
Super high-level, one or two lines per teammate, what they worked on yesterday:

{gchat.out}
{notion.out}
{github.out}
"#;

    fn checked(name: &str, text: &str) -> FlowDef {
        def::check(name, doc::read(text).unwrap(), &|_| true).unwrap()
    }

    fn prod_logs() -> FlowDef {
        checked("prod-logs", PROD_LOGS)
    }

    fn morning_brief() -> FlowDef {
        checked("morning-brief", MORNING_BRIEF)
    }

    fn facts(v: &[(&str, &str, &str)]) -> impl Lookup {
        map(v)
    }

    /// Facts that must not be asked for.
    struct Unasked;
    impl Lookup for Unasked {
        fn get(&self, r: &Ref) -> Option<String> {
            panic!("asked for {}.{}", r.head, r.rest)
        }
    }

    fn ended(step: &str, ok: bool) -> Happened {
        Happened::Ended { step: step.into(), ok }
    }

    fn start(step: &str) -> Do {
        Do::Start(step.into())
    }

    #[test]
    fn prod_logs_with_nothing_new_stops_before_the_agent() {
        let f = prod_logs();
        let mut s = BTreeMap::new();
        assert_eq!(advance(&f, &mut s, &Happened::Started, &facts(&[])), [start("probe")]);
        assert_eq!(status(&s), FiringStatus::Running);
        let d = advance(&f, &mut s, &ended("probe", true), &facts(&[("probe", "out", "")]));
        assert_eq!(d, [Do::End(FiringStatus::Done)]);
        assert_eq!(s["probe"], StepStatus::Done);
        assert_eq!(s["triage"], StepStatus::Skipped);
        assert_eq!(s["review"], StepStatus::Skipped);
        assert_eq!(status(&s), FiringStatus::Done);
    }

    #[test]
    fn prod_logs_asks_then_runs_the_choice() {
        let f = prod_logs();
        let mut s = BTreeMap::new();
        let x = facts(&[("probe", "out", "502 spike")]);
        assert_eq!(advance(&f, &mut s, &Happened::Started, &x), [start("probe")]);
        assert_eq!(advance(&f, &mut s, &ended("probe", true), &x), [start("triage")]);
        assert_eq!(
            advance(&f, &mut s, &ended("triage", true), &x),
            [Do::Ask("review".into())]
        );
        assert_eq!(s["review"], StepStatus::Waiting);
        assert_eq!(status(&s), FiringStatus::Waiting);
        let answered = Happened::Answered { step: "review".into() };
        assert_eq!(advance(&f, &mut s, &answered, &x), [Do::RunOption("review".into())]);
        assert_eq!(s["review"], StepStatus::Running);
        assert_eq!(status(&s), FiringStatus::Running);
        assert_eq!(
            advance(&f, &mut s, &ended("review", true), &x),
            [Do::End(FiringStatus::Done)]
        );
        assert_eq!(s["review"], StepStatus::Done);
    }

    #[test]
    fn an_answer_that_does_nothing_finishes_the_step() {
        let f = prod_logs();
        let mut s = BTreeMap::new();
        let x = facts(&[("probe", "out", "502 spike")]);
        for h in [Happened::Started, ended("probe", true), ended("triage", true)] {
            advance(&f, &mut s, &h, &x);
        }
        let nothing = Happened::AnsweredNothing { step: "review".into() };
        assert_eq!(advance(&f, &mut s, &nothing, &x), [Do::End(FiringStatus::Done)]);
        assert_eq!(s["review"], StepStatus::Done);
    }

    #[test]
    fn a_dismissed_ask_ends_the_firing() {
        let f = prod_logs();
        let mut s = BTreeMap::new();
        let x = facts(&[("probe", "out", "502 spike")]);
        for h in [Happened::Started, ended("probe", true), ended("triage", true)] {
            advance(&f, &mut s, &h, &x);
        }
        let dismissed = Happened::Unanswered { step: "review".into() };
        assert_eq!(advance(&f, &mut s, &dismissed, &x), [Do::End(FiringStatus::Done)]);
        assert_eq!(s["review"], StepStatus::Skipped);
        assert_eq!(status(&s), FiringStatus::Done);
    }

    #[test]
    fn fan_in_starts_together_and_joins_once_all_are_done() {
        let f = morning_brief();
        let mut s = BTreeMap::new();
        let x = facts(&[]);
        assert_eq!(
            advance(&f, &mut s, &Happened::Started, &x),
            [start("gchat"), start("notion"), start("github")]
        );
        assert_eq!(s["brief"], StepStatus::Pending);
        assert_eq!(advance(&f, &mut s, &ended("gchat", true), &x), []);
        assert_eq!(advance(&f, &mut s, &ended("notion", true), &x), []);
        assert_eq!(s["brief"], StepStatus::Pending);
        assert_eq!(advance(&f, &mut s, &ended("github", true), &x), [start("brief")]);
        assert_eq!(
            advance(&f, &mut s, &ended("brief", true), &x),
            [Do::End(FiringStatus::Done)]
        );
    }

    #[test]
    fn a_failed_source_skips_the_join_and_fails_the_firing() {
        let f = morning_brief();
        let mut s = BTreeMap::new();
        let x = facts(&[]);
        advance(&f, &mut s, &Happened::Started, &x);
        assert_eq!(advance(&f, &mut s, &ended("gchat", true), &x), []);
        assert_eq!(advance(&f, &mut s, &ended("notion", true), &x), []);
        assert_eq!(
            advance(&f, &mut s, &ended("github", false), &x),
            [Do::End(FiringStatus::Failed)]
        );
        assert_eq!(s["github"], StepStatus::Failed);
        assert_eq!(s["brief"], StepStatus::Skipped);
        assert_eq!(status(&s), FiringStatus::Failed);
    }

    #[test]
    fn a_source_failing_first_skips_the_join_and_the_rest_still_finish() {
        let f = morning_brief();
        let mut s = BTreeMap::new();
        let x = facts(&[]);
        advance(&f, &mut s, &Happened::Started, &x);
        assert_eq!(advance(&f, &mut s, &ended("github", false), &x), []);
        assert_eq!(s["brief"], StepStatus::Skipped);
        assert_eq!(status(&s), FiringStatus::Running);
        assert_eq!(advance(&f, &mut s, &ended("gchat", true), &x), []);
        assert_eq!(
            advance(&f, &mut s, &ended("notion", true), &x),
            [Do::End(FiringStatus::Failed)]
        );
    }

    #[test]
    fn a_step_after_one_that_failed_is_skipped_without_its_if() {
        let f = prod_logs();
        let mut s = BTreeMap::new();
        advance(&f, &mut s, &Happened::Started, &Unasked);
        assert_eq!(
            advance(&f, &mut s, &ended("probe", false), &Unasked),
            [Do::End(FiringStatus::Failed)]
        );
        assert_eq!(s["triage"], StepStatus::Skipped);
        assert_eq!(s["review"], StepStatus::Skipped);
    }

    #[test]
    fn a_stop_ends_the_firing_stopped() {
        let f = prod_logs();
        let mut s = BTreeMap::new();
        let x = facts(&[("probe", "out", "502 spike")]);
        advance(&f, &mut s, &Happened::Started, &x);
        let stopped = Happened::Stopped { step: "probe".into() };
        assert_eq!(advance(&f, &mut s, &stopped, &x), [Do::End(FiringStatus::Stopped)]);
        assert_eq!(s["probe"], StepStatus::Failed);
        assert_eq!(s["triage"], StepStatus::Skipped);
        assert_eq!(s["review"], StepStatus::Skipped);
        assert_eq!(advance(&f, &mut s, &stopped, &x), []);
        assert_eq!(advance(&f, &mut s, &ended("probe", true), &x), []);
    }

    #[test]
    fn a_stop_skips_everything_pending_and_ends_when_nothing_runs() {
        let f = morning_brief();
        let mut s = BTreeMap::new();
        let x = facts(&[]);
        advance(&f, &mut s, &Happened::Started, &x);
        let stop = |step: &str| Happened::Stopped { step: step.into() };
        assert_eq!(advance(&f, &mut s, &stop("gchat"), &x), []);
        assert_eq!(s["brief"], StepStatus::Skipped);
        assert_eq!(advance(&f, &mut s, &stop("notion"), &x), []);
        assert_eq!(
            advance(&f, &mut s, &stop("github"), &x),
            [Do::End(FiringStatus::Stopped)]
        );
    }

    #[test]
    fn a_lost_step_waits_and_resumes() {
        let f = prod_logs();
        let mut s = BTreeMap::new();
        let x = facts(&[("probe", "out", "502 spike")]);
        advance(&f, &mut s, &Happened::Started, &x);
        assert_eq!(advance(&f, &mut s, &Happened::Lost { step: "probe".into() }, &x), []);
        assert_eq!(s["probe"], StepStatus::Lost);
        assert_eq!(status(&s), FiringStatus::Waiting);
        // A lost run's end is not the step's: only resuming picks it up.
        assert_eq!(advance(&f, &mut s, &ended("probe", true), &x), []);
        assert_eq!(advance(&f, &mut s, &Happened::Resumed { step: "probe".into() }, &x), []);
        assert_eq!(s["probe"], StepStatus::Running);
        assert_eq!(status(&s), FiringStatus::Running);
        assert_eq!(advance(&f, &mut s, &ended("probe", true), &x), [start("triage")]);
    }

    #[test]
    fn a_repeated_event_does_nothing() {
        let f = prod_logs();
        let mut s = BTreeMap::new();
        let x = facts(&[("probe", "out", "502 spike")]);
        advance(&f, &mut s, &Happened::Started, &x);
        assert_eq!(advance(&f, &mut s, &ended("probe", true), &x), [start("triage")]);
        let before = s.clone();
        assert_eq!(advance(&f, &mut s, &ended("probe", true), &x), []);
        assert_eq!(advance(&f, &mut s, &Happened::Started, &x), []);
        assert_eq!(
            advance(&f, &mut s, &Happened::Answered { step: "triage".into() }, &x),
            []
        );
        assert_eq!(advance(&f, &mut s, &ended("nope", true), &x), []);
        assert_eq!(s, before);
    }

    #[test]
    fn a_firing_only_a_person_can_move_on_is_waiting() {
        use StepStatus::*;
        let steps = |v: &[(&str, StepStatus)]| -> BTreeMap<String, StepStatus> {
            v.iter().map(|(n, s)| (n.to_string(), *s)).collect()
        };
        assert_eq!(status(&steps(&[("a", Pending), ("b", Pending)])), FiringStatus::Running);
        assert_eq!(status(&steps(&[("a", Waiting), ("b", Pending)])), FiringStatus::Waiting);
        assert_eq!(status(&steps(&[("a", Lost), ("b", Running)])), FiringStatus::Running);
        assert_eq!(status(&steps(&[("a", Done), ("b", Skipped)])), FiringStatus::Done);
        assert_eq!(status(&steps(&[("a", Failed), ("b", Skipped)])), FiringStatus::Failed);
    }
}
