//! What to do when something fires.
//!
//! One shape, used by everything that can start a run on its own — a
//! schedule's entry, a decision's option, later a workflow's step — so that
//! the YAML looks the same wherever it is written and one piece of code turns
//! it into a `RunSpec`.
//!
//! ```yaml
//! run: "date +%T >> pulse.log"     # a string goes through sh -c
//! run: [grep, -c, ERROR, app.log]  # a list is argv, no shell
//! run:                             # a map is an agent, run headless
//!   agent: claude
//!   prompt_file: standup.md
//!   args: [--model, opus]
//! cwd: ~/work/api                  # everything else decorates the run
//! tags: [nightly]
//! ```

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

/// The three ways to say what runs. Written under one key, `run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    /// A shell line: `sh -c "…"`.
    Shell(String),
    /// argv, no shell involved.
    Argv(Vec<String>),
    /// An agent CLI with a prompt, run headless.
    Agent {
        agent: String,
        prompt: Option<String>,
        /// Relative to wherever the action was written down.
        prompt_file: Option<PathBuf>,
        /// Extra arguments, before the prompt.
        args: Vec<String>,
    },
}

impl Task {
    /// `claude · fix the build`, or the command itself, for lists.
    pub fn label(&self) -> String {
        match self {
            Task::Shell(s) => s.clone(),
            Task::Argv(v) => v.join(" "),
            Task::Agent {
                agent,
                prompt,
                prompt_file,
                ..
            } => {
                let what = prompt
                    .clone()
                    .or_else(|| prompt_file.as_ref().map(|_| "(prompt file)".into()))
                    .unwrap_or_default();
                format!("{agent} · {what}")
            }
        }
    }
}

/// What the three forms are, said once, for every error message about them.
const FORMS: &str = "a string (a shell line), a list (argv), or a map with `agent` and `prompt` \
                     or `prompt_file`";

impl<'de> Deserialize<'de> for Task {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(TaskVisitor)
    }
}

struct TaskVisitor;

impl<'de> Visitor<'de> for TaskVisitor {
    type Value = Task;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{FORMS}")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Task, E> {
        if v.trim().is_empty() {
            return Err(E::custom("`run` is empty"));
        }
        Ok(Task::Shell(v.to_string()))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Task, A::Error> {
        let mut argv = Vec::new();
        while let Some(word) = seq.next_element::<String>()? {
            argv.push(word);
        }
        if argv.is_empty() {
            return Err(de::Error::custom("`run` is an empty list"));
        }
        Ok(Task::Argv(argv))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Task, A::Error> {
        let (mut agent, mut prompt, mut prompt_file, mut args) = (None, None, None, Vec::new());
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "agent" => agent = Some(map.next_value::<String>()?),
                "prompt" => prompt = Some(map.next_value::<String>()?),
                "prompt_file" => prompt_file = Some(map.next_value::<PathBuf>()?),
                "args" => args = map.next_value::<Vec<String>>()?,
                other => {
                    return Err(de::Error::custom(format!("`run` has no `{other}`; it takes {FORMS}")));
                }
            }
        }
        let Some(agent) = agent else {
            return Err(de::Error::custom(format!(
                "`run` as a map needs `agent`; it takes {FORMS}"
            )));
        };
        match (&prompt, &prompt_file) {
            (Some(_), Some(_)) => Err(de::Error::custom("`prompt` or `prompt_file`, not both")),
            (None, None) => Err(de::Error::custom("an agent needs `prompt` or `prompt_file`")),
            _ => Ok(Task::Agent {
                agent,
                prompt,
                prompt_file,
                args,
            }),
        }
    }
}

impl Serialize for Task {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Task::Shell(line) => s.serialize_str(line),
            Task::Argv(argv) => {
                let mut seq = s.serialize_seq(Some(argv.len()))?;
                for word in argv {
                    seq.serialize_element(word)?;
                }
                seq.end()
            }
            Task::Agent {
                agent,
                prompt,
                prompt_file,
                args,
            } => {
                let n = 1 + prompt.is_some() as usize + prompt_file.is_some() as usize + !args.is_empty() as usize;
                let mut map = s.serialize_map(Some(n))?;
                map.serialize_entry("agent", agent)?;
                if let Some(p) = prompt {
                    map.serialize_entry("prompt", p)?;
                }
                if let Some(p) = prompt_file {
                    map.serialize_entry("prompt_file", p)?;
                }
                if !args.is_empty() {
                    map.serialize_entry("args", args)?;
                }
                map.end()
            }
        }
    }
}

/// A `run` and everything that decorates it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Action {
    /// Nothing at all is allowed: a decision option can be an answer with no
    /// next step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<Task>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_: Option<String>,
    /// Title of the runs it starts; the caller's name by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Pipes instead of a pty.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_tty: bool,
}

impl Action {
    /// Nothing to start.
    pub fn is_empty(&self) -> bool {
        self.run.is_none()
    }

    /// What it starts, in one line.
    pub fn label(&self) -> String {
        self.run.as_ref().map(Task::label).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(json: serde_json::Value) -> Task {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn the_three_forms() {
        assert_eq!(task(serde_json::json!("echo hi")), Task::Shell("echo hi".into()));
        assert_eq!(
            task(serde_json::json!(["echo", "hi"])),
            Task::Argv(vec!["echo".into(), "hi".into()])
        );
        assert_eq!(
            task(serde_json::json!({ "agent": "claude", "prompt": "fix it", "args": ["-v"] })),
            Task::Agent {
                agent: "claude".into(),
                prompt: Some("fix it".into()),
                prompt_file: None,
                args: vec!["-v".into()],
            }
        );
    }

    #[test]
    fn every_form_round_trips() {
        for v in [
            serde_json::json!("echo hi"),
            serde_json::json!(["echo", "hi"]),
            serde_json::json!({ "agent": "claude", "prompt_file": "p.md" }),
        ] {
            let back = serde_json::to_value(task(v.clone())).unwrap();
            assert_eq!(back, v);
        }
    }

    #[test]
    fn a_bad_run_says_what_it_takes() {
        let err = |json: serde_json::Value| serde_json::from_value::<Task>(json).unwrap_err().to_string();
        assert!(err(serde_json::json!({ "shell": "echo hi" })).contains("no `shell`"));
        assert!(err(serde_json::json!({ "prompt": "hi" })).contains("needs `agent`"));
        assert!(err(serde_json::json!({ "agent": "claude", "prompt": "a", "prompt_file": "b" })).contains("not both"));
        assert!(err(serde_json::json!({ "agent": "claude" })).contains("needs `prompt`"));
        assert!(err(serde_json::json!([])).contains("empty list"));
        assert!(err(serde_json::json!("  ")).contains("empty"));
        assert!(err(serde_json::json!(7)).contains("a string (a shell line)"));
    }

    #[test]
    fn an_action_can_be_nothing() {
        let a: Action = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(a.is_empty());
        assert_eq!(a.label(), "");
    }
}
