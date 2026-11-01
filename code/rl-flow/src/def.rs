//! A read flow, checked: what it is, and each step ready to carry out.
//!
//! Everything wrong is said at once, each at the line it is about, so a flow
//! is fixed in one pass rather than one mistake at a time.

use crate::cond::{self, Cond};
use crate::cron::Cron;
use crate::doc::{Doc, Problem};
use crate::plan;
use crate::state;
use crate::template::{self, Ref};
use rl_core::time::parse_duration;
use rl_proto::action::{Action, Task};
use rl_proto::flow::{CatchUpPlace, DEFAULT_THROTTLE, Filter, Missed, Overlap, StepSpec};
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
    /// `top`: its latest firing since you left heads the catch-up. A def kept before this was
    /// known has no such key, and reads as none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catch_up: Option<CatchUpPlace>,
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
        catch_up: spec.catch_up,
        steps,
    })
}

/// Kept with the wire's types, where a parked folder's name is read back too.
pub use rl_proto::flow::valid_name;

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
            command(name, name, &action, flow, said);
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
                command(name, &label, &action, flow, said);
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
/// folder's: relative, never out of it, and not under `runs/`, so the folder's hash covers it.
fn filled(label: &str, mut action: Action, sections: &BTreeMap<String, String>, said: &mut Vec<String>) -> Action {
    if let Some(Task::Agent {
        prompt_file: Some(f), ..
    }) = &action.run
    {
        let mut parts = f.components().filter(|c| *c != Component::CurDir);
        if !f
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        {
            said.push(format!(
                "`{label}`: prompt_file {} is not in the flow's folder",
                f.display()
            ));
        } else if parts
            .next()
            // A case-blind disk opens `Runs/` as `runs/`.
            .is_some_and(|c| c.as_os_str().eq_ignore_ascii_case("runs"))
        {
            said.push(format!(
                "`{label}`: prompt_file {} is under runs/, which no yes covers",
                f.display()
            ));
        }
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

/// Refuses a placeholder in what a command line is made of: what a step left could otherwise
/// become shell. `step` is the step it is in; `label` says where (`b`, or `b.ok` for an option).
fn command(step: &str, label: &str, action: &Action, flow: &Flow, said: &mut Vec<String>) {
    let words: Vec<&String> = match &action.run {
        Some(Task::Shell(line)) => vec![line],
        Some(Task::Argv(argv)) => argv.iter().collect(),
        Some(Task::Agent { args, .. }) => args.iter().collect(),
        None => vec![],
    };
    for r in words.iter().flat_map(|w| template::refs(w)) {
        if r.head == "event" || flow.names.contains(r.head.as_str()) {
            said.push(format!(
                "`{label}`: `{{{}.{}}}` cannot go into a command — read `{}`",
                r.head,
                r.rest,
                instead(step, &r)
            ));
        }
    }
}

/// The variable a command reads instead: the event's, what a step printed under `$RELAY_IN`, an
/// ask's own answer in its option, and any other value as `$RELAY_V_<step>_<key>`.
fn instead(step: &str, r: &Ref) -> String {
    match (r.head.as_str(), r.rest.as_str()) {
        ("event", "summary") => "$RELAY_EVENT_SUMMARY".to_string(),
        ("event", "kind") => "$RELAY_EVENT_KIND".to_string(),
        ("event", "run") => "$RELAY_EVENT_RUN".to_string(),
        ("event", _) => "$RELAY_EVENT".to_string(),
        (head, "out") => format!("$RELAY_IN/{head}.out"),
        (head, "choice") if head == step => "$RELAY_CHOICE".to_string(),
        (head, "note") if head == step => "$RELAY_NOTE".to_string(),
        (head, key) => format!("${}", state::env_name(head, key)),
    }
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
mod tests;
