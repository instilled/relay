//! The context pack: what an assistant is told before it drafts a run or a
//! rule from plain words. Built from the code — the event kinds, the specs'
//! fields, `rl --help` — so it cannot drift from what Relay does; the prose
//! around it is here, in one place. `rl context` prints it.

use crate::client::Client;
use anyhow::Result;
use rl_core::display_path;
use rl_proto::methods::{AgentList, RuleList, ScheduleFilter, WorkspaceList};
use rl_proto::rule::Rule;
use rl_proto::types::event_kind;
use std::fmt::Write;
use std::path::Path;
use std::sync::OnceLock;

/// `rl --help` and the help of the commands a draft becomes, rendered by the
/// binary at start (only `main` has the clap definitions).
static HELP: OnceLock<String> = OnceLock::new();

pub fn set_help(text: String) {
    let _ = HELP.set(text);
}

pub fn help() -> &'static str {
    HELP.get().map(String::as_str).unwrap_or("rl --help")
}

/// What is true in `folder`, from the daemon: its name, the folders Relay
/// knows, its rules and the person's own, the agents on this machine.
pub async fn gather(rpc: &mut Client, folder: &Path) -> Result<Here> {
    let workspaces = rpc.call::<WorkspaceList>(()).await?;
    let rules = rpc.call::<RuleList>(ScheduleFilter { all: false }).await?;
    let agents = rpc.call::<AgentList>(()).await?;
    let name = workspaces.iter().find(|w| w.path == folder).map(|w| w.name.clone());
    let scope_word = |r: &Rule| -> String {
        if r.is_global() {
            "yours".into()
        } else {
            workspaces
                .iter()
                .find(|w| w.path.as_os_str() == r.scope())
                .map(|w| w.name.clone())
                .unwrap_or_else(|| display_path(Path::new(r.scope())))
        }
    };
    Ok(Here {
        folder: display_path(folder),
        name,
        folders: workspaces
            .iter()
            .map(|w| (display_path(&w.path), w.name.clone()))
            .collect(),
        rules: rules
            .iter()
            .filter(|r| r.is_global() || Path::new(r.scope()) == folder)
            .map(|r| {
                (
                    scope_word(r),
                    r.kind().to_string(),
                    r.name().to_string(),
                    r.when(),
                    r.does(),
                )
            })
            .collect(),
        agents: agents
            .iter()
            .filter(|a| a.available())
            .map(|a| a.name.clone())
            .collect(),
    })
}

/// The pack, then what is being asked: the rule being edited, as it is,
/// when there is one; then the words. `editing` is (kind, whose).
pub fn ask(pack: &str, words: &str, editing: Option<&(String, String)>, yaml: Option<&str>) -> String {
    let mut s = String::from(pack);
    if let (Some((kind, whose)), Some(yaml)) = (editing, yaml) {
        let _ = write!(
            s,
            "\n## Editing\n\nThe words are about this {kind} ({whose}), as it stands. Answer with the whole \
{kind} as it should be afterwards — every field, changed or not — with `kind: {kind}` first and \
`in` as it is.\n\n```yaml\n{yaml}\n```\n"
        );
    }
    let _ = write!(s, "\n## The words\n\n{words}\n");
    s
}

/// What is true where the words were typed.
#[derive(Debug, Clone, Default)]
pub struct Here {
    /// The folder, and its registered name if it has one.
    pub folder: String,
    pub name: Option<String>,
    /// Every folder Relay knows: path, name.
    pub folders: Vec<(String, String)>,
    /// The rules of this folder and the person's own: scope word, kind, name, when, does.
    pub rules: Vec<(String, String, String, String, String)>,
    /// Agent CLIs on this machine.
    pub agents: Vec<String>,
}

pub fn vocabulary() -> &'static str {
    "\
- run: one thing Relay keeps alive — a shell command, or an agent CLI in a terminal — in a \
folder. It has a title, a state (starting, working, idle, needs you, blocked, stale, done, \
failed, stopped), tags, and a cause (by hand, a schedule, a reaction, a decision).
- folder: a directory Relay knows; what `in` names. `yours` is not a folder: it means the \
person's own files in ~/.relay, which apply everywhere.
- rule: what starts work without the person. A schedule fires on time; a reaction fires on an \
event. A folder's rules live in <folder>/.relay/schedules.yaml and reactions.yaml; the \
person's own in ~/.relay/schedules.yaml and reactions.yaml.
- job: a rule's ended runs, read as one line: how often, how many went well, the last outcome.
- question: something a run or a reaction asks the person. Its options are answers; an \
option can start a run when chosen.
- agent: an agent CLI (see `agents` below). A `run` line that starts with an agent's name is \
that agent with the rest of the line as its prompt. Any other `run` line is a shell line.
- tag, ref: labels on runs (`ci`, `#123`) for grouping and filtering.
- when: how often (`every 30m`, a five-field cron line such as `0 2 * * *`), or on what \
(`on run.failed`, several events separated by spaces). `every …` and cron make a schedule; \
`on …` makes a reaction.
"
}

/// One line per event kind `rld` emits. A kind without words here fails the
/// test below, so a new kind cannot go unmentioned.
fn event_words(kind: &str) -> Option<&'static str> {
    Some(match kind {
        event_kind::RUN_STARTED => "a run began",
        event_kind::RUN_STATE => "a run's state changed (any change; the specific ones below are the useful ones)",
        event_kind::RUN_NEEDS_INPUT => "a run is waiting for the person to type something",
        event_kind::RUN_BLOCKED => "a run is stuck on something it cannot do itself",
        event_kind::RUN_RESUMED => "a run that needed the person, or was blocked, is working again",
        event_kind::RUN_FINISHED => "a run ended well (exit 0, or marked done)",
        event_kind::RUN_FAILED => "a run ended badly (non-zero exit, a signal, or marked failed)",
        event_kind::RUN_STOPPED => "a run was stopped by the person",
        event_kind::COMMIT_LANDED => "a commit landed on a branch Relay watches",
        event_kind::BRANCH_DONE => "a branch was finished (merged or closed)",
        event_kind::SCHEDULE_FIRED => "a schedule started its run",
        event_kind::SCHEDULE_SKIPPED => "a schedule did not fire (the previous run still going, or a missed slot)",
        event_kind::DECISION_RAISED => "a question was put to the person",
        event_kind::DECISION_TAKEN => "the person answered a question",
        event_kind::DECISION_DISMISSED => "the person dismissed a question",
        event_kind::DECISION_EXPIRED => "a question expired, its default taken if it had one",
        event_kind::PLUGIN_INVOKED => "a plugin was run",
        event_kind::RUN_JOURNAL_DROPPED => "retention let a run's terminal bytes go; the run and its events stay",
        event_kind::RUN_SUMMARISED => "an assistant wrote a summary of an ended run (s in the panel, rl summarise)",
        _ => return None,
    })
}

pub fn events() -> String {
    let mut s = String::new();
    for k in event_kind::ALL {
        let _ = writeln!(s, "- {k}: {}", event_words(k).unwrap_or("(no words yet)"));
    }
    s.push_str("- run.*, decision.*: a family, every kind with that prefix.\n");
    s
}

pub fn schedule() -> &'static str {
    "\
A schedule fires on time. Its fields:
- every: a duration — 30s, 30m, 4h, 1d. Or instead:
- cron: five fields, minute hour day-of-month month day-of-week, in the person's local time.
- run: what it starts — a shell line (a string), argv (a list), or an agent as a map \
{agent, prompt} (or {agent, prompt_file, args}).
- cwd: where it runs; a folder's rules run in the folder unless told otherwise; the person's \
own rules need one.
- title: what to call its runs.
- tags: labels on its runs, a list.
- env: extra environment, a map.
- ref: a ref its runs belong to.
- no_tty: true runs it through pipes instead of a terminal (rarely wanted).
- missed: skip (default) or catch-up-once — what to do about a firing missed while asleep.
- overlap: skip (default), queue or kill — what to do when the previous run is still going.
"
}

pub fn reaction() -> &'static str {
    "\
A reaction fires on an event and either runs something or asks the person. Its fields:
- on: an event kind, or a list of them, or a family (run.*).
- where: only when the run the event is about matches every field given: tag (carries this \
tag), cause (its cause starts with this: `schedule:`, `schedule:nightly`, `reaction:`), \
cli (the agent it runs: claude, codex), group (its group key, exactly), data (fields of the \
event's payload, each a value or any of a list: `data: { severity: [ERROR, WARNING] } }`).
- run: what it starts, as for a schedule. Placeholders (below) are filled in from the event.
- ask: instead of run — a question for the person: title, summary, options (each: key, \
label, default, and optionally what choosing it runs: run, cwd, tags, title), multi \
(several answers), group (asking again replaces the open question with this key), expires.
- cwd, title, tags, env, ref, no_tty: as for a schedule.
- throttle: not again within this long (default 10s), so a noisy event does not storm.
"
}

pub fn placeholders() -> &'static str {
    "\
In a reaction's run or ask, {name} is filled in from the event: {kind}, {summary}, {title} \
(the run's), {run} (its id), {group}, {workspace}, {cause}, {cli}, and {data.<key>} for what \
the event carried (for example {data.exit}). A name nothing knows is left as written, so \
shell braces survive.
"
}

pub fn contract() -> &'static str {
    "\
Answer with exactly one YAML document and nothing around it: no prose, no code fence.
The first key is `kind`; the rest are the fields of that kind, and only those.

kind: run          # something to do now, once
in: <folder>       # one of the folders listed under Here, or the current one
run: <a shell line, or an agent's name followed by its prompt>
title: <a few words, optional>

kind: schedule     # something to do on time
in: <folder, or yours>
name: <letters, digits, . _ -; short, no spaces>
when: <every 30m | a cron line>
run: <as above>
title: <optional>
tags: [<optional>]
cwd: <optional; needed when in: yours>

kind: reaction     # something to do when an event happens
in: <folder, or yours>
name: <as above>
when: on <event> [<event> ...]
run: <as above>
title: <optional>
tags: [<optional>]
cwd: <optional; needed when in: yours>

Prefer the folder the words were typed in. Prefer a shell line for one command, an agent for \
work that needs judgement. Never invent a folder, an event kind, or a field.
"
}

/// The whole pack, in the order an assistant reads it.
pub fn render(help: &str, here: &Here) -> String {
    let mut s = String::new();
    s.push_str(
        "# Relay, for an assistant\n\nRelay keeps a person's agents and long-running work alive, and lets them \
write down what should start on its own. You are drafting one thing from what they typed; \
they will read your draft and press enter, or not. Be exact and short.\n\n",
    );
    s.push_str("## Words\n\n");
    s.push_str(vocabulary());
    s.push_str("\n## Events (what a reaction can be on)\n\n");
    s.push_str(&events());
    s.push_str("\n## A schedule\n\n");
    s.push_str(schedule());
    s.push_str("\n## A reaction\n\n");
    s.push_str(reaction());
    s.push_str("\n## Placeholders\n\n");
    s.push_str(placeholders());
    s.push_str("\n## The rl command line\n\n```\n");
    s.push_str(help.trim_end());
    s.push_str("\n```\n\n## Here\n\n");
    let _ = writeln!(
        s,
        "folder: {}{}",
        here.folder,
        here.name.as_ref().map(|n| format!("  ({n})")).unwrap_or_default()
    );
    if !here.folders.is_empty() {
        s.push_str("folders Relay knows:\n");
        for (p, n) in &here.folders {
            let _ = writeln!(s, "- {p}  ({n})");
        }
    }
    if here.rules.is_empty() {
        s.push_str("rules: none yet\n");
    } else {
        s.push_str("rules (scope · kind · name · when · does):\n");
        for (scope, kind, name, when, does) in &here.rules {
            let _ = writeln!(s, "- {scope} · {kind} · {name} · {when} · {does}");
        }
    }
    let _ = writeln!(
        s,
        "agents on this machine: {}",
        if here.agents.is_empty() {
            "none".to_string()
        } else {
            here.agents.join(", ")
        }
    );
    s.push_str("\n## Answer\n\n");
    s.push_str(contract());
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::action::Task;
    use rl_proto::decision::DecisionOption;
    use rl_proto::reaction::{AskSpec, Filter, ReactionSpec};
    use rl_proto::schedule::ScheduleSpec;

    /// Every serialised key of `v`, at every depth — except inside `env`,
    /// whose keys are the person's, not fields.
    fn keys(v: &serde_json::Value, out: &mut Vec<String>) {
        if let Some(m) = v.as_object() {
            for (k, v) in m {
                out.push(k.clone());
                if k != "env" {
                    keys(v, out);
                }
            }
        }
    }

    #[test]
    fn every_event_kind_has_words() {
        for k in event_kind::ALL {
            assert!(event_words(k).is_some(), "no words for {k}");
        }
        let text = events();
        for k in event_kind::ALL {
            assert!(
                text.contains(&format!("- {k}: ")),
                "{k} missing from the events section"
            );
        }
    }

    #[test]
    fn every_field_of_a_rule_is_explained() {
        let task = Task::Agent {
            agent: "claude".into(),
            prompt: Some("p".into()),
            prompt_file: Some("f".into()),
            args: vec!["a".into()],
        };
        let mut s = ScheduleSpec {
            cron: Some("c".into()),
            every: Some("e".into()),
            run: Some(task.clone()),
            cwd: Some("/w".into()),
            tags: vec!["t".into()],
            ref_: Some("r".into()),
            title: Some("t".into()),
            no_tty: true,
            ..Default::default()
        };
        s.env.insert("K".into(), "v".into());
        let mut found = Vec::new();
        keys(&serde_json::to_value(&s).unwrap(), &mut found);
        let text = schedule();
        for k in &found {
            assert!(
                text.contains(&format!("- {k}:")) || text.contains(k.as_str()),
                "schedule field `{k}` unexplained"
            );
        }

        let mut r = ReactionSpec {
            on: vec!["run.failed".into()],
            filter: Some(Filter {
                tag: Some("t".into()),
                cause: Some("c".into()),
                cli: Some("c".into()),
                group: Some("g".into()),
                ..Default::default()
            }),
            ask: Some(AskSpec {
                title: "t".into(),
                summary: Some("s".into()),
                options: vec![DecisionOption {
                    key: "k".into(),
                    label: "l".into(),
                    default: true,
                    run: Some(Task::Shell("x".into())),
                    cwd: Some("/w".into()),
                    tags: vec!["t".into()],
                    ref_: Some("r".into()),
                    title: Some("t".into()),
                    no_tty: true,
                    ..Default::default()
                }],
                multi: true,
                group: Some("g".into()),
                expires: Some("1h".into()),
            }),
            run: Some(task),
            cwd: Some("/w".into()),
            tags: vec!["t".into()],
            ref_: Some("r".into()),
            title: Some("t".into()),
            no_tty: true,
            throttle: Some("10s".into()),
            ..Default::default()
        };
        r.env.insert("K".into(), "v".into());
        let mut found = Vec::new();
        keys(&serde_json::to_value(&r).unwrap(), &mut found);
        let text = format!("{}{}", reaction(), schedule());
        for k in &found {
            assert!(text.contains(k.as_str()), "reaction field `{k}` unexplained");
        }
    }

    #[test]
    fn the_pack_reads_top_to_bottom() {
        let here = Here {
            folder: "~/w/relay".into(),
            name: Some("relay".into()),
            folders: vec![("~/w/relay".into(), "relay".into()), ("~/w/api".into(), "api".into())],
            rules: vec![(
                "relay".into(),
                "schedule".into(),
                "nightly".into(),
                "every 4h".into(),
                "make test".into(),
            )],
            agents: vec!["claude".into()],
        };
        let text = render("rl [OPTIONS] <COMMAND>", &here);
        let order = [
            "# Relay, for an assistant",
            "## Words",
            "## Events",
            "- run.failed:",
            "## A schedule",
            "## A reaction",
            "## Placeholders",
            "## The rl command line",
            "rl [OPTIONS] <COMMAND>",
            "## Here",
            "folder: ~/w/relay  (relay)",
            "- ~/w/api  (api)",
            "- relay · schedule · nightly · every 4h · make test",
            "agents on this machine: claude",
            "## Answer",
            "kind: reaction",
        ];
        let mut at = 0;
        for needle in order {
            let i = text[at..]
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing or out of order"));
            at += i;
        }
    }
}
