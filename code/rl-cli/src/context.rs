//! The context pack: what an assistant is told before it drafts a run, or a
//! flow, from plain words. Built from the code — the event kinds, `rl --help` — so it
//! cannot drift from what Relay does; the prose around it is here, in one
//! place. `rl context` prints it.

mod flows;

use crate::client::Client;
use anyhow::Result;
use rl_core::display_path;
use rl_proto::flow::{FlowFilter, FlowList};
use rl_proto::methods::{AgentList, WorkspaceList};
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
/// knows, the flows, the agents on this machine.
pub async fn gather(rpc: &mut Client, folder: &Path) -> Result<Here> {
    let workspaces = rpc.call::<WorkspaceList>(()).await?;
    let flows = rpc.call::<FlowList>(FlowFilter { all: false }).await?;
    let agents = rpc.call::<AgentList>(()).await?;
    let name = workspaces.iter().find(|w| w.path == folder).map(|w| w.name.clone());
    // Where a flow's steps run: the project it is in, by name where it has one.
    let in_word = |p: &Path| -> String {
        workspaces
            .iter()
            .find(|w| w.path == p)
            .map(|w| w.name.clone())
            .unwrap_or_else(|| display_path(p))
    };
    Ok(Here {
        folder: display_path(folder),
        name,
        folders: workspaces
            .iter()
            .map(|w| (display_path(&w.path), w.name.clone()))
            .collect(),
        flows: flows
            .iter()
            .map(|f| {
                (
                    f.name.clone(),
                    f.when(),
                    f.folder.as_deref().map(in_word).unwrap_or_default(),
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

/// The pack, then the words.
pub fn ask(pack: &str, words: &str) -> String {
    format!("{pack}\n## The words\n\n{words}\n")
}

/// What is true where the words were typed.
#[derive(Debug, Clone, Default)]
pub struct Here {
    /// The folder, and its registered name if it has one.
    pub folder: String,
    pub name: Option<String>,
    /// Every folder Relay knows: path, name.
    pub folders: Vec<(String, String)>,
    /// Every flow: its name, what starts it, the project it is in (empty
    /// when it has no `in`).
    pub flows: Vec<(String, String, String)>,
    /// Agent CLIs on this machine.
    pub agents: Vec<String>,
}

pub fn vocabulary() -> &'static str {
    "\
- run: one thing Relay keeps alive — a shell command, or an agent CLI in a terminal — in a \
folder. It has a title, a state (starting, working, idle, needs you, blocked, stale, done, \
failed, stopped), tags, and a cause (by hand, a flow's step, a decision).
- folder: a directory Relay knows; what `in` names.
- flow: work that starts without the person — on time, on an event, or by hand — as steps, \
written in ~/.relay/flows/<name>/flow.md (Flows, below, says what one holds).
- question: something a run or a flow asks the person. Its options are answers; an option \
can start a run when chosen.
- agent: an agent CLI (see `agents` below). A `run` line that starts with an agent's name is \
that agent with the rest of the line as its prompt. Any other `run` line is a shell line.
- agent's own session: one an agent CLI runs that Relay did not start. It shows on the panel \
but is not a run: it emits no events, so no flow's `on:` fires on it, and a `run` line cannot \
name it.
- tag, ref: labels on runs (`ci`, `#123`) for grouping and filtering.
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
        event_kind::FLOW_FIRED => "a flow fired: by hand, on time or on an event (data: flow, firing, trigger)",
        event_kind::FLOW_ENDED => "a flow's firing is over (data: flow, firing, status: done, failed or stopped)",
        event_kind::FLOW_SKIPPED => "a flow, or one of its steps, did not run (data: flow, firing, step, reason)",
        event_kind::FLOW_REFUSED => {
            "a step's $RELAY_OUT said something Relay would not keep, so the step failed (data: flow, firing, step, reason)"
        }
        event_kind::DECISION_RAISED => "a question was put to the person",
        event_kind::DECISION_TAKEN => "the person answered a question",
        event_kind::DECISION_DISMISSED => "the person dismissed a question",
        event_kind::DECISION_EXPIRED => "a question expired, its default taken if it had one",
        event_kind::PLUGIN_INVOKED => "a plugin was run",
        event_kind::RUN_TROUBLE => {
            "something went wrong around a run without ending it (output lost while rld was away, a holder that went): the run may be fine, but part of its story is missing"
        }
        event_kind::RUN_LOST => {
            "Relay came back and a run's holder was gone with no exit recorded, most likely a restart; /resume picks it back up"
        }
        event_kind::RUN_JOURNAL_DROPPED => "retention let a run's terminal bytes go; the run and its events stay",
        event_kind::RUN_SUMMARISED => {
            "an assistant wrote a run's recap (/summary in the panel, rl summary, or p in the report)"
        }
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

/// What the person asked the assistant for: whatever fits the words, or a flow by this name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    Any,
    Flow { name: String },
}

/// The answer's shapes, indented so an echo of them is never read as one, and what to prefer.
pub fn contract(want: &Want) -> String {
    // Not `"\` and a new line: that would eat the first line's indent.
    let run = "  kind: run          # something to do now, once
  in: <folder>       # one of the folders listed under Here, or the current one
  run: <a shell line, or an agent's name followed by its prompt>
  title: <a few words, optional>
";
    let flow = |name: &str| {
        format!(
            "  kind: flow         # work that starts without the person: on time, on an event, or by hand
  name: {name}
  ---
  <its frontmatter, as Flows says>
  ---
  <a line or two on what it does>

  ## <each agent step>
  <its prompt>
"
        )
    };
    let after = "Write every line of the answer from the start of the line: the shapes above are \
indented only to set them apart. A flow is flow.md alone: all after `name:` is written to \
~/.relay/flows/<name>/flow.md as it is. It may run commands on this machine and agents, never a \
script of its own — it has none, and you cannot add one. Never invent a folder, an event kind, a key or a \
field.\n";
    match want {
        Want::Any => format!(
            "Answer with one run or one flow, and nothing around it: no prose, no code fence. Its \
first line is its `kind`.\n\n{run}\n{}\n{after}\nPrefer a run for something to do now, once; a \
flow for something to do again, later, or when something happens. Prefer the folder the words \
were typed in. Prefer a shell line for one command, an agent for work that needs judgement.\n",
            flow("<a new flow's name: a-z, 0-9 and ->")
        ),
        Want::Flow { name } => format!(
            "The person asked for a flow named `{name}`. Answer with it, and nothing around it: no \
prose, no code fence.\n\n{}\n{after}",
            flow(name)
        ),
    }
}

/// The whole pack, in the order an assistant reads it, for what is wanted.
pub fn render(help: &str, here: &Here, want: &Want) -> String {
    let mut s = String::new();
    let drafting = match want {
        Want::Any => "one run, or one flow,".to_string(),
        Want::Flow { name } => format!("the flow `{name}`"),
    };
    let _ = write!(
        s,
        "# Relay, for an assistant\n\nRelay keeps a person's agents and long-running work alive, and starts \
what they wrote down to start on its own. You are drafting {drafting} from what they typed; \
they will read your draft and press enter, or not. Be exact and short.\n\n"
    );
    s.push_str("## Words\n\n");
    s.push_str(vocabulary());
    s.push_str("\n## Flows\n\n");
    s.push_str(&flows::section());
    s.push_str("\n## Events (what a flow's `on` can take)\n\n");
    s.push_str(&events());
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
    if here.flows.is_empty() {
        s.push_str("flows: none yet\n");
    } else {
        s.push_str("flows (name · what starts it · in):\n");
        for (name, when, in_) in &here.flows {
            let in_ = if in_.is_empty() { "—" } else { in_ };
            let _ = writeln!(s, "- {name} · {when} · {in_}");
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
    s.push_str(&contract(want));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_pack_reads_top_to_bottom() {
        let here = Here {
            folder: "~/w/relay".into(),
            name: Some("relay".into()),
            folders: vec![("~/w/relay".into(), "relay".into()), ("~/w/api".into(), "api".into())],
            flows: vec![
                ("nightly".into(), "every 4h".into(), "relay".into()),
                ("notify".into(), "on run.failed".into(), String::new()),
            ],
            agents: vec!["claude".into()],
        };
        let text = render("rl [OPTIONS] <COMMAND>", &here, &Want::Any);
        let order = [
            "# Relay, for an assistant",
            "## Words",
            "- flow: ",
            "~/.relay/flows/<name>/flow.md",
            "- agent's own session: ",
            "is not a run: it emits no events",
            "## Flows",
            "### The frontmatter",
            "- when: ",
            "### A step",
            "- after: ",
            "### if:",
            "### Placeholders",
            "### What a step leaves, and starts with",
            "### An example",
            "## Events",
            "- run.failed:",
            "## The rl command line",
            "rl [OPTIONS] <COMMAND>",
            "## Here",
            "folder: ~/w/relay  (relay)",
            "- ~/w/api  (api)",
            "- nightly · every 4h · relay",
            "- notify · on run.failed · —",
            "agents on this machine: claude",
            "## Answer",
            "  kind: run",
        ];
        let mut at = 0;
        for needle in order {
            let i = text[at..]
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing or out of order"));
            at += i;
        }
        assert!(
            !text.lines().any(|l| l.starts_with("kind:")),
            "the shapes are indented: an echo of the pack is never read as an answer"
        );
        let ask = ask(&text, "run the tests");
        assert!(ask.starts_with(&text) && ask.ends_with("## The words\n\nrun the tests\n"));
    }

    #[test]
    fn the_answer_asked_for_is_the_one_shown() {
        let here = Here::default();
        let any = render("rl", &here, &Want::Any);
        assert!(any.contains("  kind: run ") && any.contains("  kind: flow "), "{any}");
        assert!(any.contains("You are drafting one run, or one flow,"));
        assert!(!any.contains("A flow is not drafted here"), "it is now");
        let flow = render("rl", &here, &Want::Flow { name: "brief".into() });
        assert!(
            flow.contains("a flow named `brief`") && flow.contains("  name: brief\n"),
            "{flow}"
        );
        assert!(flow.contains("You are drafting the flow `brief`"));
        assert!(!flow.contains("kind: run"), "a flow was asked for");
    }
}
