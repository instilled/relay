//! What a firing started now would do, told before it starts: which steps would run, which only
//! if a step before them leaves something, which not — and what each does, in a line. Nothing
//! runs; `flow.test` hands in the event, if one was named.

use crate::def::{Does, FlowDef, Step};
use crate::plan;
use crate::template::Lookup;
use rl_proto::action::{Action, Task};
use rl_proto::flow::Runs;
use std::collections::BTreeMap;

/// Each step, in the order written, and whether it would run. A step's `if:` on a step before it
/// cannot be known before that step runs: it is said as a condition, with those of the steps it
/// waits on. One on the event is decided when an event is given (`event` looks up `{event.…}`) —
/// and, for a flow with no `on:`, always as the engine decides it: no event ever fires it.
pub fn steps(def: &FlowDef, event: Option<&dyn Lookup>) -> Vec<(String, Runs)> {
    let graph: Vec<(String, Vec<String>)> = def.steps.iter().map(|s| (s.name.clone(), s.after.clone())).collect();
    // A loaded flow has no cycle; were one there, the order written is as good as any.
    let order = plan::order(&graph).unwrap_or_else(|_| graph.iter().map(|(n, _)| n.clone()).collect());
    let event = if def.trigger.on.is_empty() {
        Event::None
    } else {
        event.map_or(Event::Unknown, Event::Given)
    };
    let mut said: BTreeMap<String, Runs> = BTreeMap::new();
    for name in &order {
        if let Some(step) = def.steps.iter().find(|s| &s.name == name) {
            let runs = runs(step, &said, event);
            said.insert(name.clone(), runs);
        }
    }
    def.steps
        .iter()
        .map(|s| (s.name.clone(), said.remove(&s.name).unwrap_or(Runs::Yes)))
        .collect()
}

/// The event a firing would have, as far as a test knows it.
#[derive(Clone, Copy)]
enum Event<'a> {
    /// The one named.
    Given(&'a dyn Lookup),
    /// One may fire it, but none was named.
    Unknown,
    /// None ever does: the flow has no `on:`.
    None,
}

/// What a firing with no event looks up: nothing.
struct NoEvent;
impl Lookup for NoEvent {
    fn get(&self, _: &crate::template::Ref) -> Option<String> {
        None
    }
}

/// One step, given what was said of those before it.
fn runs(step: &Step, before: &BTreeMap<String, Runs>, event: Event<'_>) -> Runs {
    let mut conds: Vec<String> = Vec::new();
    let mut add = |c: String| {
        if !conds.contains(&c) {
            conds.push(c);
        }
    };
    for a in &step.after {
        match before.get(a) {
            Some(Runs::No(_)) => return Runs::No(format!("after {a}, which would not run")),
            Some(Runs::If(cs)) => cs.iter().cloned().for_each(&mut add),
            _ => {}
        }
    }
    if let Some(c) = &step.if_ {
        match (c.subject().head.as_str(), event) {
            ("event", Event::Given(e)) if !c.holds(e) => return Runs::No(format!("if {c}: not for this event")),
            ("event", Event::None) if !c.holds(&NoEvent) => {
                return Runs::No(format!("if {c}: a flow with no on: has no event"));
            }
            ("event", Event::Given(_) | Event::None) => {}
            _ => add(c.to_string()),
        }
    }
    if conds.is_empty() { Runs::Yes } else { Runs::If(conds) }
}

/// What a step does, in a line: its command, its agent and the first line of its prompt, or its
/// question with its options, the default and when.
pub fn does(step: &Step) -> String {
    match &step.does {
        Does::Run(a) => line(&action_words(a)),
        Does::Ask(ask) => {
            let options: Vec<String> = ask
                .options
                .iter()
                .map(|(o, a)| {
                    if a.is_empty() {
                        o.clone()
                    } else {
                        format!("{o} ({})", who(a))
                    }
                })
                .collect();
            let mut s = format!("asks {} · {}", line(&ask.text), options.join(", "));
            if let Some(d) = &ask.default {
                s.push_str(&format!(" · {d}"));
                if let Some(e) = &ask.expires {
                    s.push_str(&format!(" in {e}"));
                }
            }
            s
        }
    }
}

fn action_words(a: &Action) -> String {
    match &a.run {
        Some(Task::Agent { agent, prompt, .. }) => {
            format!("{agent} · {}", prompt.as_deref().unwrap_or("(its prompt file)"))
        }
        Some(t) => t.label(),
        None => "nothing".to_string(),
    }
}

/// Who an option's action is: its agent, or its command.
fn who(a: &Action) -> String {
    match &a.run {
        Some(Task::Agent { agent, .. }) => agent.clone(),
        _ => line(&a.label()),
    }
}

/// The first line, 60 characters at most, a cut one ending in `…`.
fn line(s: &str) -> String {
    let first = s.lines().next().unwrap_or("").trim_end();
    match first.char_indices().nth(59) {
        Some((cut, _)) if first.chars().count() > 60 => format!("{}…", &first[..cut]),
        _ => first.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::Ref;
    use crate::{def, doc};

    fn flow(steps: &str) -> FlowDef {
        def::check("t", doc::read(&format!("---\nsteps:\n{steps}---\n")).unwrap(), &|_| {
            true
        })
        .unwrap()
    }

    /// A flow an event fires, `on: run.failed`.
    fn on_event(steps: &str) -> FlowDef {
        let text = format!("---\non: run.failed\nsteps:\n{steps}---\n");
        def::check("t", doc::read(&text).unwrap(), &|_| true).unwrap()
    }

    /// An event whose `data.sev` is this.
    struct Sev(&'static str);
    impl Lookup for Sev {
        fn get(&self, r: &Ref) -> Option<String> {
            (r.head == "event" && r.rest == "data.sev").then(|| self.0.to_string())
        }
    }

    #[test]
    fn a_step_runs_runs_only_if_or_does_not() {
        let f = flow(
            "  probe:\n    run: x.sh\n  triage:\n    after: probe\n    if: probe.out\n    run: { agent: claude, prompt: look }\n  \
             review:\n    after: triage\n    ask: \"{triage.out}\"\n    options: { apply: { run: { agent: claude, prompt: do } }, ignore: {} }\n  \
             quiet:\n    after: probe\n    if: \"!probe.out\"\n    run: echo none\n",
        );
        assert_eq!(
            steps(&f, None),
            [
                ("probe".to_string(), Runs::Yes),
                ("triage".to_string(), Runs::If(vec!["probe.out".into()])),
                ("review".to_string(), Runs::If(vec!["probe.out".into()])),
                ("quiet".to_string(), Runs::If(vec!["!probe.out".into()])),
            ]
        );
    }

    #[test]
    fn an_event_says_whether_its_if_holds_and_what_waits_on_it_follows() {
        let f = on_event(
            "  a:\n    if: event.data.sev == high\n    run: echo a\n  b:\n    after: a\n    run: echo b\n  c:\n    run: echo c\n",
        );
        assert_eq!(
            steps(&f, Some(&Sev("high"))),
            [
                ("a".to_string(), Runs::Yes),
                ("b".to_string(), Runs::Yes),
                ("c".to_string(), Runs::Yes)
            ]
        );
        assert_eq!(
            steps(&f, Some(&Sev("low"))),
            [
                (
                    "a".to_string(),
                    Runs::No("if event.data.sev == high: not for this event".into())
                ),
                ("b".to_string(), Runs::No("after a, which would not run".into())),
                ("c".to_string(), Runs::Yes),
            ]
        );
        assert_eq!(
            steps(&f, None)[0].1,
            Runs::If(vec!["event.data.sev == high".into()]),
            "no event: not known yet"
        );
    }

    #[test]
    fn with_no_on_an_if_on_the_event_reads_as_the_engine_decides_it() {
        // No event ever fires it: `if: event.…` never holds, its negation always does — event
        // named or not.
        let f =
            flow("  a:\n    if: event.data.sev\n    run: echo a\n  b:\n    if: \"!event.data.sev\"\n    run: echo b\n");
        for event in [None, Some(&Sev("high") as &dyn Lookup)] {
            assert_eq!(
                steps(&f, event),
                [
                    (
                        "a".to_string(),
                        Runs::No("if event.data.sev: a flow with no on: has no event".into())
                    ),
                    ("b".to_string(), Runs::Yes),
                ]
            );
        }
    }

    #[test]
    fn a_join_waits_on_every_condition_before_it_once() {
        let f = flow(
            "  gchat:\n    run: g\n  notion:\n    run: n\n  check:\n    after: notion\n    if: notion.out\n    run: c\n  \
             brief:\n    after: [gchat, check]\n    if: notion.out\n    run: b\n  post:\n    after: brief\n    if: brief.out\n    run: p\n",
        );
        let got = steps(&f, None);
        assert_eq!(
            got[3],
            ("brief".to_string(), Runs::If(vec!["notion.out".into()])),
            "said once"
        );
        assert_eq!(got[4].1, Runs::If(vec!["notion.out".into(), "brief.out".into()]));
    }

    #[test]
    fn what_a_step_does_in_a_line() {
        let long = "x".repeat(80);
        let f = flow(&format!(
            "  probe:\n    run: new-log-errors.sh\n  triage:\n    after: probe\n    run: {{ agent: claude, prompt: \"These are errors.\\nMore.\" }}\n  \
             review:\n    after: triage\n    ask: \"{{triage.out}}\"\n    options: {{ apply: {{ run: {{ agent: claude, prompt: do }} }}, ignore: {{}} }}\n    \
             default: ignore\n    expires: 4h\n  list:\n    run: [ls, -l]\n  long:\n    run: echo {long}\n",
        ));
        let d: Vec<String> = f.steps.iter().map(does).collect();
        assert_eq!(d[0], "new-log-errors.sh");
        assert_eq!(d[1], "claude · These are errors.", "an agent: its prompt's first line");
        assert_eq!(d[2], "asks {triage.out} · apply (claude), ignore · ignore in 4h");
        assert_eq!(d[3], "ls -l");
        assert_eq!(d[4].chars().count(), 60, "cut: {}", d[4]);
        assert!(d[4].ends_with('…'));
    }
}
