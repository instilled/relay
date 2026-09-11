//! The right pane in words: a run's facts and recent events, or a question
//! and the keys that answer it. Pure — takes what the daemon said, gives
//! lines.

use super::rows::{Job, Names, asked_by, cause_words};
use super::rules;
use crate::render::{fmt_clock, state_label};
use jiff::Timestamp;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use relay_core::time::{fmt_age, fmt_duration};
use relay_proto::decision::Decision;
use relay_proto::methods::DigestItem;
use relay_proto::rule::Rule;
use relay_proto::types::{Event, Run, RunState};

/// How many events a run's pane lists.
const EVENTS: usize = 12;
/// Options past this have no key; they are still listed.
pub const MAX_KEYS: usize = 9;

pub fn state_style(s: RunState) -> Style {
    match s {
        RunState::NeedsInput | RunState::Blocked => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        RunState::Failed => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        RunState::Stale => Style::default().fg(Color::Magenta),
        RunState::Finished | RunState::Stopped | RunState::Idle => Style::default().add_modifier(Modifier::DIM),
        _ => Style::default(),
    }
}

fn fact(k: &str, v: impl Into<String>) -> Line<'static> {
    Line::from(vec![Span::raw(format!(" {k:<8} ")).dim(), Span::raw(v.into())])
}

/// The command line as the user would have typed it: what Relay added for
/// its own hooks (`--settings <file>`) is left out.
fn shown_argv(argv: &[String]) -> String {
    let mut out = Vec::with_capacity(argv.len());
    let mut skip = false;
    for a in argv {
        if skip {
            skip = false;
            continue;
        }
        if a == "--settings" {
            skip = true;
            continue;
        }
        out.push(a.as_str());
    }
    out.join(" ")
}

/// A run: what it is, why it is, where it stands, what happened lately.
pub fn run_lines(item: &DigestItem, events: &[Event], names: &Names, now: Timestamp) -> Vec<Line<'static>> {
    let r = &item.run;
    let mut lines = vec![Line::from(Span::raw(format!(" {}", r.title)).bold())];
    let mut state = vec![
        Span::raw(" "),
        Span::styled(state_label(r.state, r.confidence), state_style(r.state)),
        Span::raw(format!("  {} · via {}", fmt_age(r.state_at, now), r.source)).dim(),
    ];
    if let Some(j) = &item.joined_by {
        state.push(Span::raw(format!("  ● {j}")).dim());
    }
    lines.push(Line::from(state));
    lines.push(Line::raw(""));
    lines.push(fact(
        "why",
        format!(
            "{} · started {} ({} ago)",
            cause_words(&r.cause),
            fmt_clock(r.started_at, now),
            fmt_age(r.started_at, now)
        ),
    ));
    let mut place = names.workspace(&r.workspace);
    if let Some(b) = &r.branch {
        place.push_str(&format!(" · {b}"));
    }
    if let Some(x) = &r.ref_ {
        place.push_str(&format!(" · {x}"));
    }
    lines.push(fact("where", place));
    lines.push(fact("runs", shown_argv(&r.argv)));
    if !r.tags.is_empty() {
        lines.push(fact("tags", r.tags.join(", ")));
    }
    if let Some(e) = r.ended_at {
        let code = r.exit_code.map(|c| format!(" · exit {c}")).unwrap_or_default();
        lines.push(fact(
            "ended",
            format!(
                "{} · after {}{code}",
                fmt_clock(e, now),
                fmt_duration(e.duration_since(r.started_at))
            ),
        ));
    }
    if let Some(d) = &r.detail {
        lines.push(fact("doing", d.clone()));
    }
    if let Some(l) = &r.last_line {
        lines.push(fact("last", l.clone()));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::raw(" lately").dim()));
    if events.is_empty() {
        lines.push(Line::from(Span::raw("   nothing yet").dim()));
    }
    for e in events.iter().rev().take(EVENTS) {
        lines.push(Line::from(vec![
            Span::raw(format!(" {}  ", fmt_clock(e.at, now))).dim(),
            Span::raw(format!("{:<14} ", e.kind.trim_start_matches("run."))),
            Span::raw(e.summary.clone()),
        ]));
    }
    lines
}

/// How many of a job's runs the pane lists, and how many marks it draws.
const JOB_RUNS: usize = 10;
const JOB_MARKS: usize = 40;

/// A job: the rule behind it when known, then how it has been going.
pub fn job_lines(j: &Job, rule: Option<&Rule>, names: &Names, now: Timestamp) -> Vec<Line<'static>> {
    let mut lines = match rule {
        Some(r) => {
            let mut l = rules::lines(r, names, now, Some(&j.last().run.title));
            l.push(Line::raw(""));
            l
        }
        None => vec![
            Line::from(vec![
                Span::raw(format!(" {}", j.name)).bold(),
                Span::raw(format!("  {} · not in any file now", j.kind)).dim(),
            ]),
            Line::raw(""),
        ],
    };
    let (ok, failed, stopped) = (j.ok(), j.failed(), j.stopped());
    let mut tally = vec![format!(
        "{} run{}",
        j.runs.len(),
        if j.runs.len() == 1 { "" } else { "s" }
    )];
    if ok > 0 {
        tally.push(format!("{ok} ok"));
    }
    if failed > 0 {
        tally.push(format!("{failed} failed"));
    }
    if stopped > 0 {
        tally.push(format!("{stopped} stopped"));
    }
    lines.push(Line::from(vec![
        Span::raw(" lately   ").dim(),
        Span::raw(tally.join(" · ")),
    ]));
    let marks = j.marks(JOB_MARKS);
    let spans: Vec<Span> = marks
        .chars()
        .map(|c| match c {
            '×' => Span::raw("×").red().bold(),
            '○' => Span::raw("○").dim(),
            _ => Span::raw("·"),
        })
        .collect();
    let mut row = vec![Span::raw("          ")];
    row.extend(spans);
    lines.push(Line::from(row));
    lines.push(Line::raw(""));
    for i in j.runs.iter().take(JOB_RUNS) {
        lines.push(Line::from(vec![
            Span::raw(format!(" {}  ", fmt_clock(i.at, now))).dim(),
            Span::styled(
                format!("{:<8}", state_label(i.run.state, i.run.confidence)),
                state_style(i.run.state),
            ),
            Span::raw(format!(" {}", i.summary)),
        ]));
    }
    if j.runs.len() > JOB_RUNS {
        lines.push(Line::from(
            Span::raw(format!("   … {} more", j.runs.len() - JOB_RUNS)).dim(),
        ));
    }
    lines
}

/// A question: what it asks, who asked, and the options with their keys.
/// `picked` are the options marked so far on a multi-choice question.
pub fn question_lines(d: &Decision, asker: Option<&Run>, picked: &[usize], now: Timestamp) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::raw(" ? ").yellow().bold(),
        Span::raw(d.title.clone()).bold(),
    ])];
    let mut who = format!(" asked by {} · {} ago", asked_by(d, asker), fmt_age(d.created_at, now));
    if let Some(e) = d.expires_at {
        if e > now {
            who.push_str(&format!(" · {} left", fmt_duration(e.duration_since(now))));
        } else {
            who.push_str(" · due");
        }
    }
    lines.push(Line::from(Span::raw(who).dim()));
    if let Some(s) = &d.summary {
        lines.push(Line::raw(""));
        for l in s.lines() {
            lines.push(Line::raw(format!(" {l}")));
        }
    }
    lines.push(Line::raw(""));
    for (i, o) in d.options.iter().enumerate() {
        let key = if i < MAX_KEYS { (i + 1).to_string() } else { " ".into() };
        let mark = if picked.contains(&i) {
            "◆"
        } else if o.default {
            "*"
        } else {
            " "
        };
        let mut spans = vec![
            Span::raw(format!("  {key}  ")).bold(),
            Span::raw(format!("{:<10} ", o.key)),
            Span::raw(o.text().to_string()),
        ];
        if mark != " " {
            spans.push(Span::raw(format!("  {mark}")).dim());
        }
        lines.push(Line::from(spans));
        let action = o.action().label();
        if !action.is_empty() {
            lines.push(Line::from(Span::raw(format!("     {:<10} → {action}", "")).dim()));
        }
    }
    lines.push(Line::raw(""));
    let n = d.options.len().min(MAX_KEYS);
    let hint = if d.multi {
        format!(" 1-{n} pick · enter answer · n not this")
    } else if n == 1 {
        " 1 answer · n not this".to_string()
    } else {
        format!(" 1-{n} answer · n not this")
    };
    lines.push(Line::from(Span::raw(hint).dim()));
    if d.options.iter().any(|o| o.default) {
        lines.push(Line::from(Span::raw(" * taken on its own if it expires").dim()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_proto::action::Task;
    use relay_proto::decision::{DecisionOption, DecisionStatus};
    use relay_proto::types::{Cause, Confidence, RunKind};

    fn text(lines: &[Line]) -> String {
        lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn a_run_reads_as_words() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let t: Timestamp = "2026-09-10T11:00:00Z".parse().unwrap();
        let mut r = Run::new(
            "k7x2m9qa",
            RunKind::Command,
            "nightly tests",
            vec!["./test.sh".into()],
            "/home/f/relay",
            "/home/f/relay",
            t,
        );
        r.state = RunState::Failed;
        r.confidence = Confidence::Exact;
        r.state_at = t;
        r.ended_at = Some(t);
        r.exit_code = Some(1);
        r.cause = Cause::Schedule("nightly-tests".into());
        r.tags = vec!["nightly".into()];
        let item = DigestItem {
            run: r,
            summary: "exit 1".into(),
            at: t,
            unseen: true,
            joined_by: None,
        };
        let names = Names::new([("/home/f/relay".into(), "relay".to_string())]);
        let s = text(&run_lines(&item, &[], &names, now));
        assert!(s.starts_with(" nightly tests\n failed  1h · via"), "{s}");
        assert!(s.contains(" why      schedule nightly-tests · started"), "{s}");
        assert!(s.contains(" where    relay\n"), "{s}");
        assert!(s.contains(" runs     ./test.sh\n"), "{s}");
        assert!(s.contains(" tags     nightly\n"), "{s}");
        assert!(s.contains("· after 0s · exit 1"), "{s}");
        assert!(s.ends_with(" lately\n   nothing yet"), "{s}");
        assert!(!s.contains("k7x2m9qa"), "no id on screen: {s}");
    }

    #[test]
    fn the_hooks_settings_are_not_shown() {
        let argv: Vec<String> = ["claude", "--settings", "/h/hooks/claude.json", "-p", "why?"]
            .map(String::from)
            .to_vec();
        assert_eq!(shown_argv(&argv), "claude -p why?");
        assert_eq!(shown_argv(&["make".to_string(), "test".to_string()]), "make test");
    }

    #[test]
    fn a_question_lists_keys_and_actions() {
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let t: Timestamp = "2026-09-10T05:00:00Z".parse().unwrap();
        let d = Decision {
            id: "kfmpg3sw".into(),
            workspace: "/w".into(),
            run: None,
            flow: "kfmpg3sw".into(),
            group: Some("reaction:tests-failed".into()),
            title: "nightly tests failed".into(),
            summary: Some("test parser ... FAILED".into()),
            options: vec![
                DecisionOption {
                    key: "look".into(),
                    label: "Have Claude look at it".into(),
                    run: Some(Task::Agent {
                        agent: "claude".into(),
                        prompt: Some("why?".into()),
                        prompt_file: None,
                        args: vec![],
                    }),
                    ..Default::default()
                },
                DecisionOption {
                    key: "ignore".into(),
                    default: true,
                    ..Default::default()
                },
            ],
            multi: false,
            status: DecisionStatus::Open,
            created_at: t,
            updated_at: t,
            expires_at: Some("2026-09-10T13:00:00Z".parse().unwrap()),
            resolved_at: None,
            chosen: vec![],
            outcome_runs: vec![],
            note: None,
        };
        let s = text(&question_lines(&d, None, &[], now));
        assert!(
            s.starts_with(" ? nightly tests failed\n asked by rule tests-failed · 7h ago · 1h00m left"),
            "{s}"
        );
        assert!(s.contains("\n test parser ... FAILED\n"), "{s}");
        assert!(s.contains("  1  look       Have Claude look at it\n"), "{s}");
        assert!(s.contains("→ claude · why?"), "{s}");
        assert!(s.contains("  2  ignore     ignore  *\n"), "{s}");
        assert!(s.contains(" 1-2 answer · n not this\n * taken on its own"), "{s}");
        assert!(!s.contains("kfmpg3sw"), "no id on screen: {s}");

        let s = text(&question_lines(&d, None, &[1], now));
        assert!(s.contains("  2  ignore     ignore  ◆\n"), "{s}");
    }
}
