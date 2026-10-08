//! The context pack: what an assistant is told before it drafts a run from
//! plain words. Built from the code — the event kinds, `rl --help` — so it
//! cannot drift from what Relay does; the prose around it is here, in one
//! place. `rl context` prints it.

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
written in ~/.relay/flows/<name>/flow.md.
  A flow is not drafted here: the person writes one with `rl flow edit <name>`.
- question: something a run or a flow asks the person. Its options are answers; an option \
can start a run when chosen.
- agent: an agent CLI (see `agents` below). A `run` line that starts with an agent's name is \
that agent with the rest of the line as its prompt. Any other `run` line is a shell line.
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
            "a run was recapped: its agent's own, or an assistant's (/summary in the panel, rl summary)"
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

pub fn contract() -> &'static str {
    "\
Answer with exactly one YAML document and nothing around it: no prose, no code fence.
The first key is `kind`; the rest are the fields of that kind, and only those.

kind: run          # something to do now, once
in: <folder>       # one of the folders listed under Here, or the current one
run: <a shell line, or an agent's name followed by its prompt>
title: <a few words, optional>

Prefer the folder the words were typed in. Prefer a shell line for one command, an agent for \
work that needs judgement. Never invent a folder, an event kind, or a field.
"
}

/// The whole pack, in the order an assistant reads it.
pub fn render(help: &str, here: &Here) -> String {
    let mut s = String::new();
    s.push_str(
        "# Relay, for an assistant\n\nRelay keeps a person's agents and long-running work alive, and starts \
what they wrote down to start on its own. You are drafting one run from what they typed; \
they will read your draft and press enter, or not. Be exact and short.\n\n",
    );
    s.push_str("## Words\n\n");
    s.push_str(vocabulary());
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
    s.push_str(contract());
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
        let text = render("rl [OPTIONS] <COMMAND>", &here);
        let order = [
            "# Relay, for an assistant",
            "## Words",
            "- flow: ",
            "~/.relay/flows/<name>/flow.md",
            "`rl flow edit <name>`",
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
            "kind: run",
        ];
        let mut at = 0;
        for needle in order {
            let i = text[at..]
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing or out of order"));
            at += i;
        }
        assert_eq!(text.matches("kind: ").count(), 1, "the answer is a run, and only a run");
        let ask = ask(&text, "run the tests");
        assert!(ask.starts_with(&text) && ask.ends_with("## The words\n\nrun the tests\n"));
    }
}
