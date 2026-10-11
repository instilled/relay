//! The pack's part on flows: what `flow.md` holds, each key of it, `if:`, the placeholders, what
//! a step may leave and the variables it starts with, and an example that loads.
//! The words are here; which keys there are, the `if:` forms, the variables and an event's names
//! come from the code, and the tests hold the words to them.

use rl_flow::{cond, state, template};
use std::fmt::Write;

/// Each key of a flow's frontmatter, and what it does. A test holds these to `FlowSpec`'s.
pub const FRONTMATTER: &[(&str, &str)] = &[
    (
        "in",
        "the project the steps run in: a path (`~` allowed) of a folder Relay knows, never a name; a step's own `in` overrides it",
    ),
    (
        "when",
        "a cadence: `every 30m`, or a five-field cron line in quotes, local time (`\"30 7 * * 1-5\"`)",
    ),
    (
        "on",
        "the event kinds it fires on, one or a list: `run.failed`, `run.*`, `*` (Events, below)",
    ),
    (
        "where",
        "narrows `on`: `tag`, `cause`, `cli`, `group`, and `data: { field: value or [values] }`",
    ),
    (
        "missed",
        "`catch-up-once` (the default) or `skip`: a `when` slept through",
    ),
    (
        "overlap",
        "`skip` (the default), `queue` or `kill`: a trigger while its last firing still runs",
    ),
    (
        "throttle",
        "for `on`: a burst within it is one firing (10s unless said)",
    ),
    (
        "catch_up",
        "`top`: its latest firing heads the person's catch-up — for a brief they read first",
    ),
    ("steps", "the steps, by name (a-z, 0-9 and -), in the order written"),
];

/// Each key of a step, and what it does. A test holds these to `StepSpec`'s.
pub const STEP: &[(&str, &str)] = &[
    (
        "run",
        "a shell line (a string), argv (a list), or an agent run headless: `{ agent: claude, prompt: …, args: [...] }`; an agent with no prompt takes the `## <step>` section",
    ),
    (
        "ask",
        "a question for the person, built from what came before (`\"{triage.out}\"`), with `options`",
    ),
    (
        "options",
        "the answers, in order, each an action like `run:`'s (`apply: { run: { agent: claude } }`) or `{}` for nothing; an option's agent with no prompt takes `## <step>.<option>`",
    ),
    ("default", "the option taken when the question expires"),
    ("expires", "how long the question waits: `4h`"),
    (
        "group",
        "an open question with the same group is restated, not asked again: `\"{probe.signature}\"`",
    ),
    (
        "multi",
        "more than one option may be chosen; only one of them may run something",
    ),
    (
        "after",
        "a step or a list of steps: it starts once they are done; with none, as the firing starts",
    ),
    (
        "if",
        "runs only if this holds of a step before it (below); false, it and everything after it are skipped",
    ),
    ("in", "where it runs, instead of the flow's `in`"),
    ("cwd", "the folder its run starts in, relative to where it runs"),
    (
        "env",
        "variables for its run: `{ KEY: value }` — how a script is given what is particular to the person",
    ),
    ("tags", "labels on its run"),
    ("ref", "a ref on its run: `#123`"),
    ("title", "its run's title"),
    (
        "no_tty",
        "pipes rather than a terminal (a step has no terminal either way)",
    ),
];

/// A flow that loads, as the pack shows one.
pub const EXAMPLE: &str = "\
---
in: ~/Projects/api
when: every 30m
steps:
  probe:
    run: \"gcloud logging read 'severity>=ERROR' --freshness=30m --limit=50 --format='value(textPayload)'\"
  triage:
    after: probe
    if: probe.out
    run: { agent: claude }
  review:
    after: triage
    ask: \"{triage.out}\"
    options:
      apply:  { run: { agent: claude } }
      ignore: {}
    default: ignore
    expires: 4h
---

Every 30 minutes, the errors logged since; an agent proposes one action, and with a yes an agent
does it in the checkout.

## triage
Analyse the errors below. Propose exactly one action, in two lines at most.

{probe.out}

## review.apply
Do this, in this repository: {triage.out}

What was said with the yes: {review.note}
";

/// The part, in the order an assistant reads it.
pub fn section() -> String {
    let mut s = String::new();
    s.push_str(
        "A flow is a folder, ~/.relay/flows/<name>/: its `flow.md` is YAML frontmatter between \
`---` lines, then a line or two of prose, then a `## <step>` section holding each agent step's \
prompt. Scripts of its own may sit beside it, first on its steps' PATH. It runs only once the \
person has said yes to the folder as it is, and any change to it asks again.\n\n### The frontmatter\n\n",
    );
    for (k, w) in FRONTMATTER {
        let _ = writeln!(s, "- {k}: {w}");
    }
    s.push_str("With neither `when` nor `on`, it runs only when asked: `rl flow run <name>`.\n\n### A step\n\n");
    s.push_str("Each step is a `run:` or an `ask:`; the rest says when and where.\n\n");
    for (k, w) in STEP {
        let _ = writeln!(s, "- {k}: {w}");
    }
    let _ = write!(
        s,
        "\n### if:\n\n{}. A bare one is true when the step set it and it is not empty; a value never \
made (a step skipped) is not set, and is not anything. One condition only — no `&&`: a step that \
needs more works it out and leaves a value.\n\n",
        cond::ALLOWED
    );
    let events: Vec<String> = template::EVENT_NAMES.iter().map(|n| format!("{{event.{n}}}")).collect();
    let _ = write!(
        s,
        "### Placeholders\n\n\
They go into an agent's prompt and an `ask:` only. A `run:` line or argv that names one is \
refused at load: what a step printed must never become shell. A command reads a variable instead.\n\n\
- {{<step>.out}}: what the step printed — a command's output, an agent's final message. A command reads $RELAY_IN/<step>.out.\n\
- {{<step>.<key>}}: a value the step left. A command reads ${}, a `-` in the step's name made `_`.\n\
- {{<ask>.choice}}, {{<ask>.note}}: the option chosen and what was said with it; the option's own run reads $RELAY_CHOICE and $RELAY_NOTE.\n\
- for an `on` flow, {}, and {{event.data.<field>}}. A command reads $RELAY_EVENT (the event, as JSON), $RELAY_EVENT_KIND, $RELAY_EVENT_SUMMARY and $RELAY_EVENT_RUN.\n\n",
        state::env_name("<step>", "<key>"),
        events.join(", ")
    );
    let _ = write!(
        s,
        "### What a step leaves, and starts with\n\n\
What it prints is its out. A value: `rl flow state set key=value …`, or a `key=value` line appended \
to $RELAY_OUT (a key is a-z, 0-9 and _, from a letter; a value is {} KB at most). A file: `rl flow state write \
<name> < file`, or a file written in $RELAY_WORK ({} MB at most). A later step reads them with `rl flow \
state get <step>.<key>` and `rl flow state read <name>`. Allowed `Bash(rl flow state:*)`, an agent \
reaches its firing's state and nothing else. Every step starts with:\n\n",
        state::VALUE_MAX >> 10,
        state::FILE_MAX >> 20
    );
    for (var, w) in state::STEP_ENV {
        let _ = writeln!(s, "- ${var}: {w}");
    }
    s.push_str("\n### An example\n\n");
    s.push_str(EXAMPLE);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::flow::{FlowSpec, StepSpec};

    /// The keys a type that denies unknown ones takes, as serde lists them when given one it does
    /// not: "unknown field `not-a-key`, expected one of `in`, `when`, … at line 1 column 13".
    fn keys_of<T: serde::de::DeserializeOwned>() -> Vec<String> {
        let e = serde_json::from_str::<T>(r#"{"not-a-key": 0}"#)
            .err()
            .expect("refused")
            .to_string();
        let listed = e.split("expected one of ").nth(1).expect("serde lists the keys");
        let mut keys: Vec<String> = listed.split('`').skip(1).step_by(2).map(String::from).collect();
        keys.sort();
        keys
    }

    fn words(table: &[(&str, &str)]) -> Vec<String> {
        let mut keys: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
        keys.sort();
        keys
    }

    #[test]
    fn every_key_a_flow_takes_has_words_and_none_that_it_does_not() {
        assert_eq!(words(FRONTMATTER), keys_of::<FlowSpec>(), "the frontmatter");
        assert_eq!(words(STEP), keys_of::<StepSpec>(), "a step");
    }

    fn said(key: &str) -> &'static str {
        FRONTMATTER.iter().find(|(k, _)| *k == key).expect(key).1
    }

    /// Whether `fm` (frontmatter lines) with one step is a flow that loads.
    fn loads(fm: &str, step: &str) -> bool {
        let text = format!("---\n{fm}steps:\n  a:\n    run: \"{step}\"\n---\n");
        rl_flow::def::check("t", rl_flow::doc::read(&text).unwrap(), &|_| true).is_ok()
    }

    #[test]
    fn the_values_and_defaults_it_names_are_the_ones_that_load() {
        use rl_proto::flow::{Filter, Missed, Overlap};
        for v in ["catch-up-once", "skip"] {
            assert!(loads(&format!("missed: {v}\nwhen: every 1h\n"), "true"), "missed: {v}");
            assert!(said("missed").contains(&format!("`{v}`")), "{v}");
        }
        assert_eq!(Missed::default(), Missed::CatchUpOnce);
        assert!(said("missed").contains("`catch-up-once` (the default)"));
        for v in ["skip", "queue", "kill"] {
            assert!(loads(&format!("overlap: {v}\n"), "true"), "overlap: {v}");
            assert!(said("overlap").contains(&format!("`{v}`")), "{v}");
        }
        assert_eq!(Overlap::default(), Overlap::Skip);
        assert!(said("overlap").contains("`skip` (the default)"));
        assert!(loads("catch_up: top\n", "true"));
        assert!(said("catch_up").contains("`top`"));
        assert!(
            said("throttle").contains(rl_proto::flow::DEFAULT_THROTTLE),
            "the default throttle"
        );
        for k in keys_of::<Filter>() {
            assert!(said("where").contains(&format!("`{k}")), "where: {k}");
        }
    }

    #[test]
    fn the_variables_a_command_reads_instead_are_the_ones_load_refuses_with() {
        // (the placeholder in a command, the variable the pack names for it)
        let cases = [
            ("{event.kind}", "$RELAY_EVENT_KIND", "$RELAY_EVENT_KIND"),
            ("{event.summary}", "$RELAY_EVENT_SUMMARY", "$RELAY_EVENT_SUMMARY"),
            ("{event.run}", "$RELAY_EVENT_RUN", "$RELAY_EVENT_RUN"),
            ("{event.data.f}", "$RELAY_EVENT", "$RELAY_EVENT (the event, as JSON)"),
            ("{a.out}", "$RELAY_IN/a.out", "$RELAY_IN/<step>.out"),
            ("{a.found}", "$RELAY_V_a_found", "$RELAY_V_<step>_<key>"),
        ];
        let pack = section();
        for (ph, var, taught) in cases {
            let text = format!(
                "---\non: run.failed\nsteps:\n  a:\n    run: x\n  b:\n    after: a\n    run: \"echo {ph}\"\n---\n"
            );
            let p = rl_flow::def::check("t", rl_flow::doc::read(&text).unwrap(), &|_| true).unwrap_err();
            assert!(
                p.iter().any(|p| p.message.contains(&format!("read `{var}`"))),
                "{ph}: {p:?}"
            );
            assert!(pack.contains(taught), "{ph}: the pack says {taught}");
        }
        for (ph, var) in [("{s.choice}", "$RELAY_CHOICE"), ("{s.note}", "$RELAY_NOTE")] {
            let text =
                format!("---\nsteps:\n  s:\n    ask: ok?\n    options: {{ ok: {{ run: \"echo {ph}\" }} }}\n---\n");
            let p = rl_flow::def::check("t", rl_flow::doc::read(&text).unwrap(), &|_| true).unwrap_err();
            assert!(
                p.iter().any(|p| p.message.contains(&format!("read `{var}`"))),
                "{ph}: {p:?}"
            );
            assert!(pack.contains(var), "{var}");
        }
    }

    #[test]
    fn the_key_rule_it_states_is_the_one_state_holds() {
        for ok in ["a", "found", "a_1", "z9_"] {
            assert!(rl_flow::state::key(ok).is_ok(), "{ok}");
        }
        for bad in ["A", "a-b", "1a", "_a", "a b", ""] {
            assert!(rl_flow::state::key(bad).is_err(), "{bad}");
        }
        assert!(section().contains("a key is a-z, 0-9 and _, from a letter"));
    }

    #[test]
    fn the_section_says_what_the_code_does_and_its_example_loads() {
        let d = rl_flow::def::check("prod-logs", rl_flow::doc::read(EXAMPLE).unwrap(), &|_| true)
            .unwrap_or_else(|p| panic!("the example does not load: {p:?}"));
        assert_eq!(d.steps.len(), 3);
        let s = section();
        assert!(s.contains(EXAMPLE), "the example, whole");
        assert!(s.contains(rl_flow::cond::ALLOWED));
        for (var, _) in rl_flow::state::STEP_ENV {
            assert!(s.contains(&format!("- ${var}: ")), "{var}");
        }
        for name in rl_flow::template::EVENT_NAMES {
            assert!(s.contains(&format!("{{event.{name}}}")), "{name}");
        }
        assert!(
            s.contains("$RELAY_V_<step>_<key>"),
            "a value's variable, as state names it"
        );
        for cmd in [
            "rl flow state set",
            "rl flow state get",
            "rl flow state write",
            "rl flow state read",
        ] {
            assert!(s.contains(cmd), "{cmd}");
        }
    }
}
