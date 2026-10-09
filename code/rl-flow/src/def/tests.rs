//! What `check` takes and what it refuses, each problem at its line.

use super::*;
use crate::{doc, plan};
use rl_proto::action::Task;

/// The spec's prod-logs `flow.md`, as written there.
const PROD_LOGS_FULL: &str = r#"---
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

fn problems_known(text: &str, known: &dyn Fn(&Path) -> bool) -> Vec<Problem> {
    check("t", doc::read(text).unwrap(), known).unwrap_err()
}

fn problems(text: &str) -> Vec<Problem> {
    problems_known(text, &|_| true)
}

/// Steps under the frontmatter's `steps:`, the first step's key on line 3.
fn flow(steps: &str) -> String {
    format!("---\nsteps:\n{steps}---\n")
}

/// `text` is refused with a problem saying `want`, at `line`.
#[track_caller]
fn refused(text: &str, want: &str, line: Option<usize>) {
    let ps = problems(text);
    assert!(
        ps.iter().any(|p| p.message.contains(want) && p.line == line),
        "wanted {want:?} at {line:?}, got {ps:#?}"
    );
}

#[test]
fn a_good_flow_is_planned_with_its_prompts_filled() {
    let d = doc::read(PROD_LOGS_FULL).unwrap();
    let f = check("prod-logs", d, &|_| true).unwrap();
    assert_eq!(
        f.steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["probe", "triage", "review"]
    );
    let Does::Run(a) = &f.steps[1].does else { panic!() };
    let Some(Task::Agent { prompt: Some(p), .. }) = &a.run else {
        panic!()
    };
    assert!(p.starts_with("Analyse the new errors"));
    let Does::Ask(ask) = &f.steps[2].does else { panic!() };
    let apply = &ask.options[0].1;
    assert!(matches!(&apply.run, Some(Task::Agent { prompt: Some(p), .. }) if p.contains("{review.note}")));
    assert_eq!(
        plan::upstream(&f, "review"),
        ["probe", "triage"].map(String::from).into()
    );
}

#[test]
fn the_flow_itself_is_kept() {
    let f = check("prod-logs", doc::read(PROD_LOGS_FULL).unwrap(), &|_| true).unwrap();
    assert_eq!(f.name, "prod-logs");
    assert_eq!(f.in_, Some(PathBuf::from("~/Projects/flint-platform")));
    assert_eq!(f.trigger.when, Some(When::Every(30 * 60)));
    assert_eq!(f.throttle_secs, 10);
    assert_eq!(f.missed, Missed::CatchUpOnce, "a firing slept through is made up by default");
    let Does::Ask(ask) = &f.steps[2].does else { panic!() };
    assert_eq!(ask.text, "{triage.out}");
    assert_eq!(ask.group.as_deref(), Some("{probe.signature}"));
    assert_eq!(ask.default.as_deref(), Some("ignore"));
    assert!(ask.options[1].1.is_empty());
    assert!(matches!(f.steps[1].if_, Some(Cond::Set(_))));
}

#[test]
fn a_flow_crosses_the_wire_with_its_order() {
    let f = check("prod-logs", doc::read(PROD_LOGS_FULL).unwrap(), &|_| true).unwrap();
    let v = serde_json::to_value(&f).unwrap();
    let back: FlowDef = serde_json::from_value(v).unwrap();
    assert_eq!(back, f);
    let Does::Ask(ask) = &back.steps[2].does else { panic!() };
    assert_eq!(
        ask.options.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        ["apply", "ignore"]
    );
}

#[test]
fn neither_run_nor_ask_or_both_is_refused() {
    refused(
        &flow("  a:\n    after: []\n"),
        "`a` needs `run:` or `ask:`, not both",
        Some(3),
    );
    refused(
        &flow("  a:\n    run: x\n    ask: y\n    options: { ok: {} }\n"),
        "`a` needs `run:` or `ask:`, not both",
        Some(3),
    );
}

#[test]
fn ask_keys_without_ask_are_refused() {
    refused(
        &flow("  a:\n    run: x\n    default: y\n"),
        "`a`: `default` is for an `ask:` step",
        Some(3),
    );
    refused(
        &flow("  a:\n    run: x\n    multi: true\n"),
        "`a`: `multi` is for an `ask:` step",
        Some(3),
    );
    refused(
        &flow("  a:\n    run: x\n    options: { ok: {} }\n"),
        "`a`: `options` is for an `ask:` step",
        Some(3),
    );
}

#[test]
fn an_ask_with_no_options_is_refused() {
    refused(&flow("  a:\n    ask: sure?\n"), "`a` asks with no options", Some(3));
}

#[test]
fn a_default_that_is_no_option_is_refused() {
    refused(
        &flow("  a:\n    ask: sure?\n    options: { yes: {} }\n    default: no\n"),
        "`a`: default `no` is not an option",
        Some(3),
    );
}

#[test]
fn an_expiry_that_is_no_duration_is_refused() {
    refused(
        &flow("  a:\n    ask: sure?\n    options: { ok: {} }\n    expires: soon\n"),
        "`a`: expires `soon` is not a duration",
        Some(3),
    );
}

#[test]
fn an_unknown_after_is_refused() {
    refused(
        &flow("  a:\n    run: x\n  b:\n    after: nope\n    run: y\n"),
        "`b` comes after `nope`, which is no step",
        Some(5),
    );
}

#[test]
fn a_cycle_is_refused() {
    refused(
        &flow("  x:\n    after: y\n    run: a\n  y:\n    after: x\n    run: b\n"),
        "`x` and `y` wait on each other",
        Some(3),
    );
}

#[test]
fn an_if_that_does_not_parse_is_refused() {
    refused(
        &flow("  a:\n    run: x\n  b:\n    after: a\n    if: a.out && a.err\n    run: y\n"),
        "`b`: an if: holds one condition",
        Some(5),
    );
}

#[test]
fn an_if_or_a_placeholder_from_no_step_before_is_refused() {
    refused(
        &flow("  a:\n    run: x\n  b:\n    if: a.out\n    run: y\n"),
        "`b` uses `{a.out}`, but does not come after `a`",
        Some(5),
    );
    refused(
        &flow("  a:\n    run: x\n  b:\n    if: nope.out\n    run: y\n"),
        "`b` uses `{nope.out}`, but does not come after `nope`",
        Some(5),
    );
    refused(
        &flow("  a:\n    run: x\n  b:\n    run: { agent: claude, prompt: \"see {a.out}\" }\n"),
        "`b` uses `{a.out}`, but does not come after `a`",
        Some(5),
    );
    refused(
        &flow("  a:\n    run: x\n  b:\n    ask: \"{a.out}\"\n    options: { ok: {} }\n"),
        "`b` uses `{a.out}`, but does not come after `a`",
        Some(5),
    );
    refused(
        &flow("  a:\n    run: x\n  b:\n    ask: ok?\n    group: \"{a.sig}\"\n    options: { ok: {} }\n"),
        "`b` uses `{a.sig}`, but does not come after `a`",
        Some(5),
    );
    // An option's prompt may use its own ask's answer, and nothing else of it.
    refused(
        &format!(
            "{}## b.ok\n{{b.choice}} {{b.note}} {{b.out}}\n",
            flow("  b:\n    ask: ok?\n    options: { ok: { run: { agent: claude } } }\n")
        ),
        "`b` uses `{b.out}`, but does not come after `b`",
        Some(3),
    );
    let ps = problems(&format!(
        "{}## b.ok\n{{b.choice}} {{b.note}} {{b.out}}\n",
        flow("  b:\n    ask: ok?\n    options: { ok: { run: { agent: claude } } }\n")
    ));
    assert_eq!(ps.len(), 1, "{ps:#?}");
}

#[test]
fn braces_that_name_no_step_are_left_alone_in_a_prompt() {
    let text = flow("  a:\n    run: { agent: claude, prompt: \"{file.txt} {event.summary}\" }\n");
    assert!(check("t", doc::read(&text).unwrap(), &|_| true).is_ok());
}

#[test]
fn a_placeholder_in_a_command_is_refused_saying_what_to_read() {
    refused(
        &flow("  probe:\n    run: x\n  b:\n    after: probe\n    run: \"echo {probe.out}\"\n"),
        "`b`: `{probe.out}` cannot go into a command — read `$RELAY_IN/probe.out`",
        Some(5),
    );
    refused(
        &flow("  probe:\n    run: x\n  b:\n    after: probe\n    run: [echo, \"{probe.signature}\"]\n"),
        "`b`: `{probe.signature}` cannot go into a command — read `$RELAY_V_probe_signature`",
        Some(5),
    );
    refused(
        &flow("  new-errors:\n    run: x\n  b:\n    after: new-errors\n    run: \"echo {new-errors.found}\"\n"),
        "read `$RELAY_V_new_errors_found`",
        Some(5),
    );
    refused(
        &flow(
            "  review:\n    ask: ok?\n    options: { ok: {} }\n  b:\n    after: review\n    run: \"echo {review.choice}\"\n",
        ),
        "`b`: `{review.choice}` cannot go into a command — read `$RELAY_V_review_choice`",
        Some(6),
    );
    refused(
        &flow("  b:\n    ask: ok?\n    options: { ok: { run: \"echo {b.note}\" } }\n"),
        "`b.ok`: `{b.note}` cannot go into a command — read `$RELAY_NOTE`",
        Some(3),
    );
    refused(
        &flow("  b:\n    run: \"echo {event.summary}\"\n"),
        "`b`: `{event.summary}` cannot go into a command — read `$RELAY_EVENT_SUMMARY`",
        Some(3),
    );
    refused(
        &flow("  b:\n    run: \"echo {event.data.x}\"\n"),
        "`b`: `{event.data.x}` cannot go into a command — read `$RELAY_EVENT`",
        Some(3),
    );
    // The shell's own braces are its own.
    let text = flow("  a:\n    run: \"awk '{print $1}' {x}\"\n");
    assert!(check("t", doc::read(&text).unwrap(), &|_| true).is_ok());
}

#[test]
fn a_group_and_an_if_may_name_a_value() {
    let text = flow(
        "  probe:\n    run: x\n  triage:\n    after: probe\n    if: probe.found\n    run: echo t\n  \
         review:\n    after: probe\n    ask: \"{probe.signature}?\"\n    group: \"{probe.signature}\"\n    options: { ok: {} }\n",
    );
    assert!(check("t", doc::read(&text).unwrap(), &|_| true).is_ok());
}

#[test]
fn an_agent_with_no_prompt_needs_its_section() {
    refused(
        &flow("  x:\n    run: { agent: claude }\n"),
        "`x`: an agent needs a prompt, or a `## x` section",
        Some(3),
    );
    refused(
        &flow("  r:\n    ask: ok?\n    options: { go: { run: { agent: claude } } }\n"),
        "`r.go`: an agent needs a prompt, or a `## r.go` section",
        Some(3),
    );
    let text = flow("  x:\n    run: { agent: claude, prompt_file: prompts/x.md }\n");
    assert!(check("t", doc::read(&text).unwrap(), &|_| true).is_ok());
}

#[test]
fn a_prompt_file_outside_the_folder_is_refused() {
    for file in ["/etc/motd", "../other/x.md", "prompts/../../x.md"] {
        refused(
            &flow(&format!(
                "  x:\n    run: {{ agent: claude, prompt_file: \"{file}\" }}\n"
            )),
            &format!("`x`: prompt_file {file} is not in the flow's folder"),
            Some(3),
        );
    }
    refused(
        &flow("  r:\n    ask: ok?\n    options: { go: { run: { agent: claude, prompt_file: /x.md } } }\n"),
        "`r.go`: prompt_file /x.md is not in the flow's folder",
        Some(3),
    );
}

#[test]
fn a_prompt_file_under_runs_is_refused() {
    for file in ["runs/x.md", "./runs/f1/x.md", "Runs/x.md"] {
        refused(
            &flow(&format!(
                "  x:\n    run: {{ agent: claude, prompt_file: \"{file}\" }}\n"
            )),
            &format!("`x`: prompt_file {file} is under runs/, which no yes covers"),
            Some(3),
        );
    }
}

#[test]
fn a_multi_ask_with_more_than_one_option_to_run_is_refused() {
    refused(
        &flow(
            "  r:\n    ask: which?\n    multi: true\n    options: { a: { run: echo a }, b: { run: echo b }, c: {} }\n",
        ),
        "`r`: `multi` with more than one option that runs something",
        Some(3),
    );
    let one = flow("  r:\n    ask: which?\n    multi: true\n    options: { a: { run: echo a }, b: {}, c: {} }\n");
    assert!(check("t", doc::read(&one).unwrap(), &|_| true).is_ok());
}

#[test]
fn a_section_for_nothing_is_refused() {
    refused(
        &format!(
            "{}## Triage\nnot a heading\n## triag\nhello\n",
            flow("  triage:\n    run: x\n")
        ),
        "`## triag` is not a step or an ask's option",
        Some(8),
    );
    refused(
        &format!("{}## a.ok\nhello\n", flow("  a:\n    run: x\n")),
        "`## a.ok` is not a step or an ask's option",
        Some(6),
    );
}

#[test]
fn an_unknown_in_is_refused() {
    let known = |p: &Path| p == Path::new("~/known");
    let ps = problems_known("---\nin: ~/x\nsteps:\n  a:\n    run: x\n---\n", &known);
    assert!(
        ps.iter()
            .any(|p| p.message == "in: ~/x is not a project Relay knows — `rl add ~/x`" && p.line == Some(2)),
        "{ps:#?}"
    );
    let ps = problems_known("---\nin: ~/known\nsteps:\n  a:\n    in: ~/y\n    run: x\n---\n", &known);
    assert_eq!(ps.len(), 1, "{ps:#?}");
    assert_eq!(ps[0].line, Some(4));
    assert_eq!(ps[0].message, "in: ~/y is not a project Relay knows — `rl add ~/y`");
}

#[test]
fn a_when_is_a_cadence_or_a_cron_line() {
    let when = |w: &str| {
        let text = format!("---\nwhen: \"{w}\"\nsteps:\n  a:\n    run: x\n---\n");
        check("t", doc::read(&text).unwrap(), &|_| true).map(|f| f.trigger.when)
    };
    assert_eq!(when("every 90s").unwrap(), Some(When::Every(90)));
    assert_eq!(when("every 1h30m").unwrap(), Some(When::Every(5400)));
    assert_eq!(when("30 7 * * 1-5").unwrap(), Some(When::Cron("30 7 * * 1-5".into())));
    assert_eq!(when("hourly").unwrap_err()[0].line, Some(2));
    let said = |w: &str| when(w).unwrap_err()[0].message.clone();
    assert!(said("hourly").contains("cron needs 5 fields"), "{}", said("hourly"));
    assert!(said("61 * * * *").contains("minute"), "{}", said("61 * * * *"));
    assert!(
        said("every soon").contains("`soon` is not a duration"),
        "{}",
        said("every soon")
    );
    assert!(said("every 0s").contains("not a duration"), "{}", said("every 0s"));
}

#[test]
fn neither_when_nor_on_is_a_flow_run_by_hand() {
    let f = check("t", doc::read(&flow("  a:\n    run: x\n")).unwrap(), &|_| true).unwrap();
    assert_eq!(f.trigger.when, None);
    assert!(f.trigger.on.is_empty());
}

#[test]
fn a_throttle_is_a_duration() {
    let f = check(
        "t",
        doc::read("---\non: run.failed\nthrottle: 5m\nsteps:\n  a:\n    run: x\n---\n").unwrap(),
        &|_| true,
    )
    .unwrap();
    assert_eq!(f.throttle_secs, 300);
    assert_eq!(f.trigger.on, ["run.failed"]);
    let ps = problems("---\nthrottle: often\nsteps:\n  a:\n    run: x\n---\n");
    assert_eq!(ps[0].message, "throttle: `often` is not a duration");
    assert_eq!(ps[0].line, Some(2));
}

#[test]
fn a_flow_with_no_steps_is_refused() {
    refused(
        "---\nwhen: every 1h\nsteps: {}\n---\n",
        "a flow needs at least one step",
        Some(3),
    );
}

#[test]
fn a_step_name_is_one_a_placeholder_can_name() {
    refused(&flow("  Probe:\n    run: x\n"), "`Probe` is not a step name", Some(3));
    refused(&flow("  event:\n    run: x\n"), "`event` is not a step name", Some(3));
}

#[test]
fn every_problem_is_said_at_once() {
    let ps = problems(&flow("  a:\n    after: nope\n    run: x\n  b:\n    ask: ok?\n"));
    assert_eq!(ps.len(), 2, "{ps:#?}");
    assert_eq!((ps[0].line, ps[1].line), (Some(3), Some(6)));
}

#[test]
fn names_are_lowercase_words_and_dashes() {
    for good in ["prod-logs", "a", "0x", "brief-2"] {
        assert!(valid_name(good), "{good}");
    }
    for bad in ["", "-a", "Prod", "a_b", "a.b", "a b", "é"] {
        assert!(!valid_name(bad), "{bad}");
    }
}

#[test]
fn catch_up_top_is_kept_and_a_def_from_before_it_still_reads() {
    let text = "---\nwhen: \"30 7 * * 1-5\"\ncatch_up: top\nsteps:\n  brief:\n    run: echo brief\n---\n";
    let f = check("morning-brief", doc::read(text).unwrap(), &|_| true).unwrap();
    assert_eq!(f.catch_up, Some(CatchUpPlace::Top));
    let mut v = serde_json::to_value(&f).unwrap();
    assert_eq!(v["catch_up"], "top", "the client reads it from the def");
    v.as_object_mut().unwrap().remove("catch_up");
    let old: FlowDef = serde_json::from_value(v).unwrap();
    assert_eq!(old.catch_up, None, "a firing's def kept before catch_up existed");
    let plain = check("p", doc::read(&flow("  a:\n    run: x\n")).unwrap(), &|_| true).unwrap();
    assert!(serde_json::to_value(&plain).unwrap().get("catch_up").is_none());
}

#[test]
fn a_catch_up_that_is_not_top_is_refused_at_its_line() {
    let p = doc::read("---\ncatch_up: bottom\nsteps:\n  a:\n    run: x\n---\n").unwrap_err();
    assert_eq!(p.line, Some(2), "the value's line in the file: {p:?}");
    assert!(p.message.contains("top"), "{p:?}");
}
