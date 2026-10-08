//! A read flow, checked: what it is, and each step ready to carry out.
//!
//! Everything wrong is said at once, each at the line it is about, so a flow
//! is fixed in one pass rather than one mistake at a time.

use crate::cond::{self, Cond};
use crate::cron::Cron;
use crate::doc::{Doc, Problem};
use crate::plan;
use crate::template::{self, Ref};
use rl_core::time::parse_duration;
use rl_proto::action::{Action, Task};
use rl_proto::flow::{DEFAULT_THROTTLE, Filter, Missed, Overlap, StepSpec};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// A flow that checked out: what it is, and each step ready to carry out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowDef {
    pub name: String,
    /// As written: `~` is the daemon's to expand.
    pub in_: Option<PathBuf>,
    pub trigger: Trigger,
    pub missed: Missed,
    pub overlap: Overlap,
    /// For `on`: a burst within this many seconds is one firing.
    pub throttle_secs: u64,
    /// In the order they are written.
    pub steps: Vec<Step>,
}

/// What starts a firing. With no `when` and no `on`, only asking does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trigger {
    pub when: Option<When>,
    pub on: Vec<String>,
    pub filter: Option<Filter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum When {
    /// Seconds between firings.
    Every(u64),
    /// A five-field cron line, local time.
    Cron(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub name: String,
    pub after: Vec<String>,
    pub if_: Option<Cond>,
    pub in_: Option<PathBuf>,
    pub does: Does,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Does {
    /// The action, with an agent's prompt filled from its `## <step>` section when it had none.
    Run(Action),
    Ask(Ask),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    pub text: String,
    /// Each option's action, an agent's prompt filled from `## <step>.<option>`; empty does nothing.
    /// A list of pairs, so the order survives JSON.
    pub options: Vec<(String, Action)>,
    pub default: Option<String>,
    pub expires: Option<String>,
    pub group: Option<String>,
    pub multi: bool,
}

/// What every step's check needs to know of the others.
struct Flow<'a> {
    names: BTreeSet<&'a str>,
    graph: Vec<(String, Vec<String>)>,
    sections: &'a BTreeMap<String, String>,
    known: &'a dyn Fn(&Path) -> bool,
}

/// Checks a read flow. `known` says whether a folder is a project Relay knows;
/// it is given the path as written. Every problem is returned, each at the
/// line of its step, key or heading.
pub fn check(name: &str, doc: Doc, known: &dyn Fn(&Path) -> bool) -> Result<FlowDef, Vec<Problem>> {
    let Doc {
        spec,
        sections,
        lines,
        keys,
        headings,
    } = doc;
    let mut problems = Vec::new();
    // A problem with the flow itself is said at the key it is about.
    let mut flow_problem = |key: &str, message: String| {
        problems.push(Problem {
            line: keys.get(key).copied(),
            column: None,
            message,
        })
    };
    if spec.steps.is_empty() {
        flow_problem("steps", "a flow needs at least one step".to_string());
    }
    if let Some(dir) = spec.in_.as_deref().filter(|d| !known(d)) {
        flow_problem("in", unknown_in(dir));
    }
    let when = spec.when.as_deref().map(when).transpose().unwrap_or_else(|e| {
        flow_problem("when", e);
        None
    });
    let throttle_secs = throttle(spec.throttle.as_deref()).unwrap_or_else(|e| {
        flow_problem("throttle", e);
        0
    });
    let flow = Flow {
        names: spec.steps.iter().map(|(n, _)| n.as_str()).collect(),
        graph: spec.steps.iter().map(|(n, s)| (n.clone(), s.after.clone())).collect(),
        sections: &sections,
        known,
    };
    let mut steps = Vec::new();
    for (step_name, s) in &spec.steps {
        let mut said = Vec::new();
        let made = step(step_name, s, &flow, &mut said);
        let line = lines.get(step_name).copied();
        problems.extend(said.into_iter().map(|message| Problem {
            line,
            column: None,
            message,
        }));
        steps.extend(made);
    }
    let unknown_after = spec
        .steps
        .iter()
        .any(|(_, s)| s.after.iter().any(|a| !flow.names.contains(a.as_str())));
    if !unknown_after && let Err((at, message)) = plan::sort(&flow.graph) {
        problems.push(Problem {
            line: lines.get(&at).copied(),
            column: None,
            message,
        });
    }
    for heading in sections.keys().filter(|h| !is_for_something(h, &spec.steps)) {
        problems.push(Problem {
            line: headings.get(heading).copied(),
            column: None,
            message: format!("`## {heading}` is not a step or an ask's option"),
        });
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(FlowDef {
        name: name.to_string(),
        in_: spec.in_,
        trigger: Trigger {
            when,
            on: spec.on,
            filter: spec.filter,
        },
        missed: spec.missed,
        overlap: spec.overlap,
        throttle_secs,
        steps,
    })
}

/// `[a-z0-9][a-z0-9-]*`: a flow's folder, or a step a placeholder can name.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let word = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    chars.next().is_some_and(word) && chars.all(|c| word(c) || c == '-')
}

/// One step checked, what is wrong with it pushed to `said`. `None` when it
/// cannot be made at all.
fn step(name: &str, s: &StepSpec, flow: &Flow, said: &mut Vec<String>) -> Option<Step> {
    if !valid_name(name) {
        said.push(format!(
            "`{name}` is not a step name: a-z, 0-9 and -, from a letter or digit"
        ));
    } else if name == "event" {
        said.push("`event` is not a step name: `{event.…}` is the event a flow fired on".to_string());
    }
    for a in s.after.iter().filter(|a| !flow.names.contains(a.as_str())) {
        said.push(plan::no_step(name, a));
    }
    if let Some(dir) = s.in_.as_deref().filter(|d| !(flow.known)(d)) {
        said.push(unknown_in(dir));
    }
    let up = plan::reach(&flow.graph, name);
    let uses = |r: &Ref, own: bool, said: &mut Vec<String>| {
        let mine = own && r.head == name && (r.rest == "choice" || r.rest == "note");
        if flow.names.contains(r.head.as_str()) && !up.contains(&r.head) && !mine {
            said.push(format!(
                "`{name}` uses `{{{}.{}}}`, but does not come after `{}`",
                r.head, r.rest, r.head
            ));
        }
    };
    let uses_in = |text: &str, own: bool, said: &mut Vec<String>| {
        for r in template::refs(text) {
            uses(&r, own, said);
        }
    };
    let if_ = match s.if_.as_deref().map(cond::parse).transpose() {
        Ok(c) => c,
        Err(e) => {
            said.push(format!("`{name}`: {e}"));
            None
        }
    };
    if let Some(c) = &if_ {
        let r = c.subject();
        if r.head != "event" && !flow.names.contains(r.head.as_str()) {
            // A step that does not exist does not come before this one either.
            said.push(format!(
                "`{name}` uses `{{{}.{}}}`, but does not come after `{}`",
                r.head, r.rest, r.head
            ));
        } else {
            uses(r, false, said);
        }
    }
    let does = match (&s.run, &s.ask) {
        (Some(_), None) => {
            for key in ask_keys(s) {
                said.push(format!("`{name}`: `{key}` is for an `ask:` step"));
            }
            let action = filled(name, s.action(), flow.sections, said);
            command(name, &action, flow, said);
            prompt_of(&action).inspect(|p| uses_in(p, false, said));
            Does::Run(action)
        }
        (None, Some(text)) => {
            uses_in(text, false, said);
            if let Some(g) = &s.group {
                uses_in(g, false, said);
            }
            if s.options.is_empty() {
                said.push(format!("`{name}` asks with no options"));
            }
            if let Some(d) = s.default.as_ref().filter(|d| !s.options.iter().any(|(o, _)| o == *d)) {
                said.push(format!("`{name}`: default `{d}` is not an option"));
            }
            if let Some(e) = s.expires.as_ref().filter(|e| parse_duration(e).is_none()) {
                said.push(format!("`{name}`: expires `{e}` is not a duration (4h, 2d)"));
            }
            let mut options = Vec::new();
            for (option, action) in &s.options {
                let label = format!("{name}.{option}");
                let action = filled(&label, action.clone(), flow.sections, said);
                command(&label, &action, flow, said);
                prompt_of(&action).inspect(|p| uses_in(p, true, said));
                options.push((option.clone(), action));
            }
            // Only the first option chosen would run: until each does, one at most may.
            if s.multi && options.iter().filter(|(_, a)| !a.is_empty()).count() > 1 {
                said.push(format!(
                    "`{name}`: `multi` with more than one option that runs something — only one may"
                ));
            }
            Does::Ask(Ask {
                text: text.clone(),
                options,
                default: s.default.clone(),
                expires: s.expires.clone(),
                group: s.group.clone(),
                multi: s.multi,
            })
        }
        _ => {
            said.push(format!("`{name}` needs `run:` or `ask:`, not both"));
            return None;
        }
    };
    Some(Step {
        name: name.to_string(),
        after: s.after.clone(),
        if_,
        in_: s.in_.clone(),
        does,
    })
}

/// The keys of an `ask:` a step gives.
fn ask_keys(s: &StepSpec) -> Vec<&'static str> {
    [
        ("options", !s.options.is_empty()),
        ("default", s.default.is_some()),
        ("expires", s.expires.is_some()),
        ("group", s.group.is_some()),
        ("multi", s.multi),
    ]
    .into_iter()
    .filter_map(|(k, given)| given.then_some(k))
    .collect()
}

/// `action`, an agent with no prompt given the `## <label>` section. A `prompt_file` is the
/// folder's: relative, and never out of it, so the folder's hash covers it.
fn filled(label: &str, mut action: Action, sections: &BTreeMap<String, String>, said: &mut Vec<String>) -> Action {
    if let Some(Task::Agent {
        prompt_file: Some(f), ..
    }) = &action.run
        && !f
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        said.push(format!(
            "`{label}`: prompt_file {} is not in the flow's folder",
            f.display()
        ));
    }
    if let Some(Task::Agent {
        prompt: prompt @ None,
        prompt_file: None,
        ..
    }) = &mut action.run
    {
        match sections.get(label).filter(|t| !t.is_empty()) {
            Some(text) => *prompt = Some(text.clone()),
            None => said.push(format!("`{label}`: an agent needs a prompt, or a `## {label}` section")),
        }
    }
    action
}

/// An agent's prompt, as written or filled.
fn prompt_of(action: &Action) -> Option<&str> {
    match &action.run {
        Some(Task::Agent { prompt, .. }) => prompt.as_deref(),
        _ => None,
    }
}

/// Refuses a placeholder in what a command line is made of: what a step
/// left could otherwise become shell.
fn command(label: &str, action: &Action, flow: &Flow, said: &mut Vec<String>) {
    let words: Vec<&String> = match &action.run {
        Some(Task::Shell(line)) => vec![line],
        Some(Task::Argv(argv)) => argv.iter().collect(),
        Some(Task::Agent { args, .. }) => args.iter().collect(),
        None => vec![],
    };
    for r in words.iter().flat_map(|w| template::refs(w)) {
        if r.head == "event" || flow.names.contains(r.head.as_str()) {
            said.push(format!(
                "`{label}`: `{{{}.{}}}` cannot go into a command — {}",
                r.head,
                r.rest,
                instead(&r)
            ));
        }
    }
}

/// What to do instead: a command reads the event from its environment; what a
/// step left reaches only a prompt or a question.
fn instead(r: &Ref) -> String {
    let var = match (r.head.as_str(), r.rest.as_str()) {
        ("event", "summary") => "$RELAY_EVENT_SUMMARY",
        ("event", "kind") => "$RELAY_EVENT_KIND",
        ("event", "run") => "$RELAY_EVENT_RUN",
        ("event", _) => "$RELAY_EVENT",
        _ => return "what a step left goes into an agent's prompt or an `ask:`".to_string(),
    };
    format!("read `{var}`")
}

/// Whether a `## heading` is some step's, or some ask's option's.
fn is_for_something(heading: &str, steps: &[(String, StepSpec)]) -> bool {
    steps.iter().any(|(name, s)| {
        heading == name
            || heading
                .strip_prefix(name.as_str())
                .and_then(|rest| rest.strip_prefix('.'))
                .is_some_and(|option| s.ask.is_some() && s.options.iter().any(|(o, _)| o == option))
    })
}

fn unknown_in(dir: &Path) -> String {
    let d = dir.display();
    format!("in: {d} is not a project Relay knows — `rl add {d}`")
}

/// `every 30m`, or a five-field cron line.
fn when(s: &str) -> Result<When, String> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("every ") {
        return every(rest).map(When::Every);
    }
    Cron::parse(s)
        .map(|_| When::Cron(s.to_string()))
        .map_err(|e| format!("when: {e:#}"))
}

/// `30m`, `4h`, `1h30m`: a `when` cadence, in whole seconds, never none.
fn every(s: &str) -> Result<u64, String> {
    let s = s.trim();
    parse_duration(s)
        .map(|d| d.as_secs().unsigned_abs())
        .filter(|secs| *secs > 0)
        .ok_or_else(|| format!("when: every `{s}` is not a duration (30m, 4h)"))
}

/// Seconds, `10s` unless said, `0` for none.
fn throttle(said: Option<&str>) -> Result<u64, String> {
    let text = said.unwrap_or(DEFAULT_THROTTLE).trim();
    if text == "0" {
        return Ok(0);
    }
    parse_duration(text)
        .map(|d| d.as_secs().unsigned_abs())
        .ok_or_else(|| format!("throttle: `{text}` is not a duration"))
}

#[cfg(test)]
mod tests {
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
        assert_eq!(f.missed, Missed::Skip);
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
    fn a_placeholder_in_a_command_is_refused() {
        refused(
            &flow("  probe:\n    run: x\n  b:\n    after: probe\n    run: \"echo {probe.out}\"\n"),
            "`b`: `{probe.out}` cannot go into a command — what a step left goes into an agent's prompt or an `ask:`",
            Some(5),
        );
        refused(
            &flow("  probe:\n    run: x\n  b:\n    after: probe\n    run: [echo, \"{probe.out}\"]\n"),
            "`b`: `{probe.out}` cannot go into a command",
            Some(5),
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
        refused(
            &flow(
                "  probe:\n    run: x\n  b:\n    after: probe\n    ask: ok?\n    options: { ok: { run: \"echo {probe.out}\" } }\n",
            ),
            "`b.ok`: `{probe.out}` cannot go into a command",
            Some(5),
        );
        // The shell's own braces are its own.
        let text = flow("  a:\n    run: \"awk '{print $1}' {x}\"\n");
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
}
