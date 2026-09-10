//! What to do when something fires: a command, or an agent with a prompt.
//!
//! One shape, used by everything that can start a run on its own — a
//! schedule's entry, a decision's option, later a workflow's step — so that
//! the YAML looks the same wherever it is written and one piece of code turns
//! it into a `RunSpec`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Action {
    /// A command, argv.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run: Vec<String>,
    /// An agent CLI, run headless with `prompt` (or `prompt_file`) and `args`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Relative to wherever the action was written down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
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
    /// Nothing to start: the option is a plain answer, not a next step.
    pub fn is_empty(&self) -> bool {
        self.run.is_empty() && self.agent.is_none()
    }

    /// `sh -c …` or `claude · fix the build`, for lists.
    pub fn label(&self) -> String {
        match &self.agent {
            Some(cli) => {
                let what = self
                    .prompt
                    .clone()
                    .or_else(|| self.prompt_file.as_ref().map(|_| "(prompt file)".into()))
                    .unwrap_or_default();
                format!("{cli} · {what}")
            }
            None if self.run.is_empty() => String::new(),
            None => self.run.join(" "),
        }
    }
}
