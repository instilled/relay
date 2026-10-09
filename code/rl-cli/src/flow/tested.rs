//! What `flow.test` found, in lines: for `rl flow test` and the panel's `/flow test` alike.

use crate::tui::flows::when_words;
use jiff::Timestamp;
use rl_core::display_path;
use rl_core::time::fmt_when;
use rl_proto::flow::{Going, Held, Overlap, Runs, Tested};

/// The flow and what starts it; what holds it, its next times, a firing still going, the event;
/// a line per step — its name, whether it would run, what it does and where; and that nothing
/// was started.
pub fn lines(t: &Tested, now: Timestamp) -> Vec<String> {
    let name = &t.flow;
    let row = |label: &str, text: String| format!("  {label:<7} {text}");
    let mut v = vec![format!("{name} · {}", when_words(&t.when))];
    match t.held {
        Some(Held::NeedsYes) => v.push(row(
            "held",
            format!("needs a yes: nothing fires it until rl flow approve {name}"),
        )),
        Some(Held::Paused) => v.push(row("held", format!("paused: only rl flow run {name} fires it"))),
        None => {}
    }
    if !t.next.is_empty() {
        let at: Vec<String> = t.next.iter().map(|x| fmt_when(*x, now)).collect();
        v.push(row("next", at.join(" · ")));
    }
    if let Some(g) = &t.going {
        v.push(row("going", going(g)));
    }
    match &t.event {
        Some(e) => {
            let verdict = if e.fires {
                "fires it".to_string()
            } else {
                format!("not: {}", e.why)
            };
            let mut s = format!("#{} {} · {} — {verdict}", e.id, e.kind, e.summary);
            if e.old {
                s.push_str(" (over 5 minutes old: the loop would pass over it)");
            }
            v.push(row("event", s));
        }
        None if !t.on.is_empty() => v.push(row(
            "on",
            format!(
                "{} · rl flow test {name} --event <id> says whether one fires it (rl events lists them)",
                t.on.join(", ")
            ),
        )),
        None => {}
    }
    let runs: Vec<String> = t.steps.iter().map(|s| runs_words(&s.runs)).collect();
    let w = t.steps.iter().map(|s| s.name.chars().count()).max().unwrap_or(0);
    let rw = runs.iter().map(|r| r.chars().count()).max().unwrap_or(0);
    for (s, r) in t.steps.iter().zip(&runs) {
        let at = match &s.folder {
            Some(p) => format!(" · in {}", display_path(p)),
            None => " · where the event happens".to_string(),
        };
        v.push(format!("  {:<w$}  {r:<rw$}  {}{at}", s.name, s.does));
    }
    v.push(format!("started nothing · rl flow run {name} fires it now"));
    v
}

fn runs_words(r: &Runs) -> String {
    match r {
        Runs::Yes => "runs".into(),
        Runs::If(c) => format!("if {}", c.join(" and ")),
        Runs::No(why) => format!("not: {why}"),
    }
}

fn going(g: &Going) -> String {
    let (then, word) = match g.overlap {
        Overlap::Skip => ("a new firing would be skipped", "skip"),
        Overlap::Queue => ("a new firing would wait for it", "queue"),
        Overlap::Kill => ("a new firing would stop it first", "kill"),
    };
    format!("{} is still {}: {then} (overlap: {word})", g.firing, g.status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::flow::{EventTest, FiringStatus, StepTest};
    use std::path::PathBuf;

    fn step(name: &str, does: &str, runs: Runs, folder: Option<&str>) -> StepTest {
        StepTest {
            name: name.into(),
            does: does.into(),
            runs,
            folder: folder.map(PathBuf::from),
        }
    }

    #[test]
    fn what_a_test_found_reads_a_line_each() {
        let now: Timestamp = "2026-10-09T08:00:00Z".parse().unwrap();
        let t = Tested {
            flow: "prod-logs".into(),
            when: "every 30m".into(),
            on: vec![],
            held: Some(Held::NeedsYes),
            next: vec![now + jiff::SignedDuration::from_mins(30)],
            going: Some(Going {
                firing: "f1".into(),
                status: FiringStatus::Waiting,
                overlap: Overlap::Skip,
            }),
            event: None,
            steps: vec![
                step("probe", "new-log-errors.sh", Runs::Yes, Some("/w/api")),
                step(
                    "triage",
                    "claude · look",
                    Runs::If(vec!["probe.out".into()]),
                    Some("/w/api"),
                ),
                step(
                    "review",
                    "asks {triage.out}",
                    Runs::No("after triage, which would not run".into()),
                    Some("/w/api"),
                ),
            ],
        };
        let l = lines(&t, now);
        assert_eq!(l[0], "prod-logs · every 30m");
        assert_eq!(
            l[1],
            "  held    needs a yes: nothing fires it until rl flow approve prod-logs"
        );
        assert!(l[2].starts_with("  next    "), "{l:?}");
        assert_eq!(
            l[3],
            "  going   f1 is still waiting: a new firing would be skipped (overlap: skip)"
        );
        let (w, rw) = (6, "not: after triage, which would not run".len());
        assert_eq!(
            l[4],
            format!("  {:<w$}  {:<rw$}  new-log-errors.sh · in /w/api", "probe", "runs")
        );
        assert_eq!(
            l[5],
            format!("  {:<w$}  {:<rw$}  claude · look · in /w/api", "triage", "if probe.out")
        );
        assert_eq!(
            l.last().unwrap(),
            "started nothing · rl flow run prod-logs fires it now"
        );

        let on = Tested {
            when: "on logs.issue".into(),
            on: vec!["logs.issue".into()],
            held: None,
            next: vec![],
            going: None,
            steps: vec![step("a", "echo a", Runs::Yes, None)],
            ..t.clone()
        };
        let l = lines(&on, now);
        assert_eq!(
            l[1],
            "  on      logs.issue · rl flow test prod-logs --event <id> says whether one fires it (rl events lists them)"
        );
        assert_eq!(l[2], "  a  runs  echo a · where the event happens");
        let fired = Tested {
            event: Some(EventTest {
                id: 412,
                kind: "logs.issue".into(),
                summary: "a lot".into(),
                fires: false,
                why: "it is paused".into(),
                old: false,
            }),
            ..on
        };
        assert_eq!(
            lines(&fired, now)[1],
            "  event   #412 logs.issue · a lot — not: it is paused"
        );
        // Over five minutes old: said whether it fires or not.
        for (fires, verdict) in [(true, "fires it"), (false, "not: it is paused")] {
            let mut old = fired.clone();
            let e = old.event.as_mut().unwrap();
            (e.fires, e.old) = (fires, true);
            assert_eq!(
                lines(&old, now)[1],
                format!(
                    "  event   #412 logs.issue · a lot — {verdict} (over 5 minutes old: the loop would pass over it)"
                )
            );
        }
    }
}
