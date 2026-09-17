//! The right pane in words: a run's facts and recent events, or a question
//! and the keys that answer it. Pure — takes what the daemon said, gives
//! lines.

use super::rows::{Job, Names, asked_by, cause_words};
use super::rules;
use crate::render::{fmt_clock, state_label};
use jiff::Timestamp;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use rl_core::time::{fmt_age, fmt_duration};
use rl_proto::decision::Decision;
use rl_proto::methods::DigestItem;
use rl_proto::rule::Rule;
use rl_proto::types::{Event, Run, RunState, Summary};

/// How many events a run's pane lists.
const EVENTS: usize = 12;
/// Options past this have no key; they are still listed.
pub const MAX_KEYS: usize = 9;

pub fn state_style(s: RunState) -> Style {
    super::theme::state(s)
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

/// A run: what it is, why it is, where it stands, what an assistant said
/// about it if asked, what happened lately.
pub fn run_lines(
    item: &DigestItem,
    events: &[Event],
    summary: Option<&Summary>,
    names: &Names,
    now: Timestamp,
) -> Vec<Line<'static>> {
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
    if let Some(s) = summary {
        lines.push(Line::raw(""));
        lines.push(Line::from(
            Span::raw(format!(" {} said, {} ago · s asks again", s.agent, fmt_age(s.at, now))).dim(),
        ));
        for l in s.text.lines() {
            lines.push(Line::raw(format!(" {l}")));
        }
    } else if r.state.is_terminal() && !matches!(r.cause, rl_proto::types::Cause::Assistant(_)) {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::raw(" s asks the assistant what it did").dim()));
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

/// The "global" row's pane: the schedules & reactions in the Relay home —
/// the rules that apply in every folder — told the way a project's are.
pub fn global_lines(rules: &[Rule], home: &str, now: Timestamp) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::raw(" global").bold())];
    lines.push(Line::from(
        Span::raw(format!(" {home} · rules that apply in every folder")).dim(),
    ));
    lines.extend(rules_section(rules, None, now));
    lines.push(Line::raw(""));
    lines.push(Line::from(
        Span::raw(" ^g shows them in the wiring · /edit schedule edits the file").dim(),
    ));
    lines
}

/// The rules section shared by `global_lines` and `project_lines`: a leading
/// blank line, the " rules" label, and the schedule/reaction chips (or the
/// empty-state line), computed via `super::rows::chips` and filtered to
/// `workspace` (`None` for the global rules).
fn rules_section(rules: &[Rule], workspace: Option<&std::path::Path>, now: Timestamp) -> Vec<Line<'static>> {
    let chips = super::rows::chips(rules, workspace, now);
    let (sched, react): (Vec<_>, Vec<_>) = chips.iter().partition(|c| c.kind == rl_proto::rule::RuleKind::Schedule);
    let mut lines = vec![Line::raw(""), Line::from(Span::raw(" rules").dim())];
    if chips.is_empty() {
        lines.push(Line::from(
            Span::raw("   none · /new adds a schedule or reaction").dim(),
        ));
    } else {
        for (label, group) in [("schedule", &sched), ("reaction", &react)] {
            for c in group {
                lines.push(Line::from(vec![
                    Span::raw(format!("   {:<9} ", label)).dim(),
                    Span::raw(c.text()),
                ]));
            }
        }
    }
    lines
}

/// A project: the folder itself, how its runs stand, and the rules that live
/// there. `items` is the whole digest; this picks the ones in the folder.
pub fn project_lines(
    name: &str,
    place: &str,
    workspace: &std::path::Path,
    items: &[DigestItem],
    rules: &[Rule],
    now: Timestamp,
) -> Vec<Line<'static>> {
    let mine: Vec<&DigestItem> = items.iter().filter(|i| i.run.workspace == workspace).collect();
    let mut lines = vec![Line::from(Span::raw(format!(" {name}")).bold())];
    lines.push(Line::from(Span::raw(format!(" {place}")).dim()));
    lines.push(Line::raw(""));

    // How the runs here stand, the buckets in the order attention wants them.
    let count = |f: &dyn Fn(RunState) -> bool| mine.iter().filter(|i| f(i.run.state)).count();
    let needs = count(&|s| matches!(s, RunState::NeedsInput | RunState::Blocked | RunState::Failed));
    let going = count(&|s| {
        matches!(
            s,
            RunState::Starting | RunState::Working | RunState::Idle | RunState::Stale
        )
    });
    let done = count(&|s| matches!(s, RunState::Finished | RunState::Stopped));
    let tally: Vec<String> = [("needs you", needs), ("going", going), ("done", done)]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(w, n)| format!("{n} {w}"))
        .collect();
    let n = mine.len();
    let word = if n == 1 { "run" } else { "runs" };
    let runs = if n == 0 {
        "no runs yet".to_string()
    } else if tally.is_empty() {
        format!("{n} {word}")
    } else {
        format!("{n} {word} · {}", tally.join(" · "))
    };
    lines.push(fact("runs", runs));
    if let Some(last) = mine.iter().map(|i| i.at).max() {
        lines.push(fact("last", format!("{} ago", fmt_age(last, now))));
    }

    // The rules that live here, told the same way the header's chips are.
    lines.extend(rules_section(rules, Some(workspace), now));
    lines.push(Line::raw(""));
    lines.push(Line::from(
        Span::raw(" ^g shows its wiring · /edit schedule edits the file · ^r renames").dim(),
    ));
    lines
}

/// A question: what it asks, who asked, and the options with their keys.
/// `picked` are the options marked so far on a multi-choice question.
pub fn question_lines(d: &Decision, asker: Option<&Run>, picked: &[usize], now: Timestamp) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::raw(" ? ").yellow().bold(),
        Span::raw(d.title.clone()).bold(),
    ])];
    lines.extend(question_body(d, asker, picked, now, None));
    lines
}

/// A summary longer than this is cut in the card on the list; the pane has
/// all of it.
pub const CARD_SUMMARY: usize = 3;

/// Everything of a question under its title — who asked, the summary (cut to
/// `summary_max` lines, if given), the options, how to answer — for the pane,
/// and for the card the selected question opens into on the list.
pub fn question_body(
    d: &Decision,
    asker: Option<&Run>,
    picked: &[usize],
    now: Timestamp,
    summary_max: Option<usize>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
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
        let all: Vec<&str> = s.lines().collect();
        let max = summary_max.unwrap_or(all.len());
        for l in all.iter().take(max) {
            lines.push(Line::raw(format!(" {l}")));
        }
        if all.len() > max {
            lines.push(Line::from(
                Span::raw(format!(" … {} more · ^o reads it all", all.len() - max)).dim(),
            ));
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
        format!(" 1-{n} pick · enter answer · or `1,2 and a note` on the line · ^x not this")
    } else if n == 1 {
        " 1 answer · or `1 and a note` on the line · ^x not this".to_string()
    } else {
        format!(" 1-{n} answer · or `1 and a note` on the line · ^x not this")
    };
    lines.push(Line::from(Span::raw(hint).dim()));
    if d.options.iter().any(|o| o.default) {
        lines.push(Line::from(Span::raw(" * taken on its own if it expires").dim()));
    }
    lines
}

/// A line that answers a question: the option first — its number or its key,
/// several joined by commas where the question takes several — and whatever
/// follows as the note that goes with the answer. `None` when the first word
/// names no option, so the line means what it would anywhere else.
pub fn answer_of(d: &Decision, line: &str) -> Option<(Vec<String>, Option<String>)> {
    let line = line.trim();
    let (head, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let mut keys = Vec::new();
    for part in head.split(',').filter(|p| !p.is_empty()) {
        let opt = match part.parse::<usize>() {
            Ok(i) if (1..=d.options.len().min(MAX_KEYS)).contains(&i) => &d.options[i - 1],
            Ok(_) => return None,
            Err(_) => d.options.iter().find(|o| o.key.eq_ignore_ascii_case(part))?,
        };
        if !keys.contains(&opt.key) {
            keys.push(opt.key.clone());
        }
    }
    if keys.is_empty() || (keys.len() > 1 && !d.multi) {
        return None;
    }
    let note = rest.trim();
    Some((keys, (!note.is_empty()).then(|| note.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::action::Task;
    use rl_proto::decision::{DecisionOption, DecisionStatus};
    use rl_proto::types::{Cause, Confidence, RunKind};

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
        let s = text(&run_lines(&item, &[], None, &names, now));
        assert!(s.starts_with(" nightly tests\n failed  1h · via"), "{s}");
        assert!(s.contains(" why      schedule nightly-tests · started"), "{s}");
        assert!(s.contains(" where    relay\n"), "{s}");
        assert!(s.contains(" runs     ./test.sh\n"), "{s}");
        assert!(s.contains(" tags     nightly\n"), "{s}");
        assert!(s.contains("· after 0s · exit 1"), "{s}");
        assert!(s.contains("\n s asks the assistant what it did\n"), "{s}");
        assert!(s.ends_with(" lately\n   nothing yet"), "{s}");
        assert!(!s.contains("k7x2m9qa"), "no id on screen: {s}");

        // Summarised: the assistant's words, marked as its, under the facts.
        let summary = Summary {
            run: "k7x2m9qa".into(),
            agent: "claude".into(),
            by: "a1".into(),
            at: "2026-09-10T11:58:00Z".parse().unwrap(),
            text: "The parser test failed on an empty file.\nNothing was changed.".into(),
        };
        let s = text(&run_lines(&item, &[], Some(&summary), &names, now));
        assert!(
            s.contains("\n claude said, 2m ago · s asks again\n The parser test failed on an empty file.\n Nothing was changed.\n\n lately"),
            "{s}"
        );
        assert!(!s.contains("s asks the assistant"), "{s}");
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
        assert!(
            s.contains(" 1-2 answer · or `1 and a note` on the line · ^x not this\n * taken on its own"),
            "{s}"
        );
        assert!(!s.contains("kfmpg3sw"), "no id on screen: {s}");

        let s = text(&question_lines(&d, None, &[1], now));
        assert!(s.contains("  2  ignore     ignore  ◆\n"), "{s}");

        // The card cuts a long summary and says where the rest is.
        let mut long = d.clone();
        long.summary = Some("a\nb\nc\nd\ne".into());
        let s = text(&question_body(&long, None, &[], now, Some(CARD_SUMMARY)));
        assert!(s.contains(" a\n b\n c\n … 2 more · ^o reads it all\n"), "{s}");
        assert!(!s.contains(" d\n"), "{s}");

        // The line answers by number or key, the rest the note.
        let some = |k: &[&str], n: Option<&str>| Some((k.iter().map(|x| x.to_string()).collect(), n.map(String::from)));
        assert_eq!(answer_of(&d, "1"), some(&["look"], None));
        assert_eq!(answer_of(&d, "2  not tonight "), some(&["ignore"], Some("not tonight")));
        assert_eq!(
            answer_of(&d, "LOOK only the parser"),
            some(&["look"], Some("only the parser"))
        );
        assert_eq!(answer_of(&d, "3"), None, "no third option");
        assert_eq!(answer_of(&d, "make test"), None, "not an option: the line is a line");
        assert_eq!(answer_of(&d, "1,2"), None, "one option only");
        let mut multi = d.clone();
        multi.multi = true;
        assert_eq!(
            answer_of(&multi, "1,ignore both"),
            some(&["look", "ignore"], Some("both"))
        );
    }
}
