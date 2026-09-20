//! `~/.relay/config.yaml`: your defaults, human-edited. Absent means all
//! defaults; a key that is not known is refused, so a typo cannot silently
//! do nothing. Keys are added one at a time, when a real need shows up.
//!
//! ```yaml
//! agents: [claude, codex]        # the agents the panel offers; unset: all of them
//! assistants:
//!   interpret: claude            # or, with a model and extra arguments:
//!   interpret: { agent: claude, model: claude-haiku-4-5-20251001, args: ["--max-turns", "1"] }
//!   summarise: claude            # a recap where the agent keeps none (/summarise, rl summarise)
//! retention: 30d                 # the journal of a run ended this long ago goes; or { journal: 30d }
//! runs:
//!   reap_after: 6h               # a run nothing has come for in this long stops itself; `never` keeps it
//! workspaces:
//!   badges: [gh-pr]              # beside a git folder's branch on its header: `PR #95`
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The agent CLIs the panel offers to start, by profile name. Empty: all
    /// of them. `rl setup` asks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    /// The agents Relay itself calls on, by job.
    #[serde(default, skip_serializing_if = "Assistants::is_empty")]
    pub assistants: Assistants,
    /// The one thing Relay deletes, and only when told: unset, nothing ever goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
    /// What becomes of a run Relay has lost touch with.
    #[serde(default, skip_serializing_if = "Runs::is_empty")]
    pub runs: Runs,
    /// What a folder's header says.
    #[serde(default, skip_serializing_if = "Workspaces::is_empty")]
    pub workspaces: Workspaces,
}

/// A folder's header, beyond what Relay reads itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspaces {
    /// Commands whose first line goes after a git folder's branch — `gh-pr`
    /// says `PR #95`. Each runs in the folder through `sh -c`, with
    /// `RELAY_WORKSPACE` and `RELAY_BRANCH` set and `~/.relay/plugins` first
    /// on the PATH: when the branch changes, then at most every five minutes,
    /// and only while something shows the list. Nothing printed, a failure or
    /// a detached HEAD: no badge. Provisional: the plugin host takes it over.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<String>,
}

impl Workspaces {
    pub fn is_empty(&self) -> bool {
        self.badges.is_empty()
    }
}

/// How long a run outlives Relay itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runs {
    /// A run's holder keeps the run alive when `rld` stops — that is the
    /// point — but nothing journals it or knows its state while no daemon is
    /// there. With nobody attached at all for this long, the holder hangs up
    /// its child and leaves: the throwaway homes of tests and one-off scripts
    /// stop piling up. `never` waits forever. Unset: `6h`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reap_after: Option<String>,
}

impl Runs {
    pub fn is_empty(&self) -> bool {
        self.reap_after.is_none()
    }

    /// What the file says, or the default. `Some("never")` is not a duration:
    /// the daemon reads it as "wait forever".
    pub fn reap_after(&self) -> &str {
        self.reap_after.as_deref().unwrap_or(DEFAULT_REAP)
    }
}

/// How long a run waits for a Relay that never comes back, unset.
pub const DEFAULT_REAP: &str = "6h";

/// Whether a `reap_after` value means "wait forever".
pub fn is_never(s: &str) -> bool {
    s.eq_ignore_ascii_case("never") || s.eq_ignore_ascii_case("off")
}

/// What goes, and after how long. Written as one duration (the journal), or
/// as a map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Retention {
    /// A run's terminal bytes, log and sidecar files, once it has ended this
    /// long ago. The run and its events stay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<String>,
}

impl<'de> Deserialize<'de> for Retention {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Map {
            #[serde(default)]
            journal: Option<String>,
        }
        let v = serde_json::Value::deserialize(d)?;
        match v {
            serde_json::Value::String(s) => Ok(Retention { journal: Some(s) }),
            other => {
                let m = Map::deserialize(&other).map_err(serde::de::Error::custom)?;
                Ok(Retention { journal: m.journal })
            }
        }
    }
}

/// Each job Relay hands to an agent, and who does it. Unset means the
/// first available agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assistants {
    /// What you type at `a` / `rl interpret` in plain words → a run or a
    /// rule, drafted for your enter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpret: Option<Assistant>,
    /// What an ended run did, in a few sentences (`s`, `rl summarise`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summarise: Option<Assistant>,
}

impl Assistants {
    pub fn is_empty(&self) -> bool {
        self.interpret.is_none() && self.summarise.is_none()
    }

    /// The assistant for a job, by its name in the file.
    pub fn for_job(&self, job: &str) -> Option<Option<&Assistant>> {
        match job {
            "interpret" => Some(self.interpret.as_ref()),
            "summarise" => Some(self.summarise.as_ref()),
            _ => None,
        }
    }

    /// The jobs there are, in the order `rl config` lists them.
    pub const JOBS: &[&str] = &["interpret", "summarise"];
}

/// An agent doing a job for Relay: written as its name, or as a map when
/// a model or extra arguments are wanted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assistant {
    pub agent: String,
    /// Passed through the profile's `model_flag`; refused when the profile
    /// has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Verbatim, after the headless arguments and before the prompt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

impl<'de> Deserialize<'de> for Assistant {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Form {
            Name(String),
            Full {
                agent: String,
                #[serde(default)]
                model: Option<String>,
                #[serde(default)]
                args: Vec<String>,
            },
        }
        // The untagged form loses `deny_unknown_fields`; check by hand.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Strict {
            #[allow(dead_code)]
            agent: String,
            #[allow(dead_code)]
            #[serde(default)]
            model: Option<String>,
            #[allow(dead_code)]
            #[serde(default)]
            args: Vec<String>,
        }
        let v = serde_json::Value::deserialize(d)?;
        if v.is_object() {
            Strict::deserialize(&v).map_err(serde::de::Error::custom)?;
        }
        match Form::deserialize(&v).map_err(serde::de::Error::custom)? {
            Form::Name(agent) => Ok(Assistant {
                agent,
                model: None,
                args: vec![],
            }),
            Form::Full { agent, model, args } => Ok(Assistant { agent, model, args }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_or_a_map() {
        let c: Config = serde_json::from_str(r#"{"assistants":{"interpret":"claude"}}"#).unwrap();
        assert_eq!(
            c.assistants.interpret,
            Some(Assistant {
                agent: "claude".into(),
                model: None,
                args: vec![]
            })
        );
        let c: Config = serde_json::from_str(
            r#"{"assistants":{"interpret":{"agent":"codex","model":"o3","args":["--full-auto"]}}}"#,
        )
        .unwrap();
        assert_eq!(
            c.assistants.interpret,
            Some(Assistant {
                agent: "codex".into(),
                model: Some("o3".into()),
                args: vec!["--full-auto".into()]
            })
        );
        assert_eq!(Config::default(), serde_json::from_str("{}").unwrap());
        // Written back the short way when short, the long way when not.
        let c: Config = serde_json::from_str(r#"{"assistants":{"interpret":"claude"}}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"assistants":{"interpret":{"agent":"claude"}}}"#
        );
    }

    #[test]
    fn retention_is_a_duration_or_a_map() {
        let c: Config = serde_json::from_str(r#"{"retention":"30d"}"#).unwrap();
        assert_eq!(c.retention.unwrap().journal.as_deref(), Some("30d"));
        let c: Config = serde_json::from_str(r#"{"retention":{"journal":"7d"}}"#).unwrap();
        assert_eq!(c.retention.unwrap().journal.as_deref(), Some("7d"));
        let e = serde_json::from_str::<Config>(r#"{"retention":{"runs":"1y"}}"#).unwrap_err();
        assert!(e.to_string().contains("unknown field `runs`"), "{e}");
    }

    #[test]
    fn a_run_outlives_relay_for_six_hours_unless_told_otherwise() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c.runs.reap_after(), "6h");
        assert!(c.runs.is_empty());
        let c: Config = serde_json::from_str(r#"{"runs":{"reap_after":"30m"}}"#).unwrap();
        assert_eq!(c.runs.reap_after(), "30m");
        let c: Config = serde_json::from_str(r#"{"runs":{"reap_after":"never"}}"#).unwrap();
        assert!(is_never(c.runs.reap_after()));
        let e = serde_json::from_str::<Config>(r#"{"runs":{"reap":"30m"}}"#).unwrap_err();
        assert!(e.to_string().contains("unknown field `reap`"), "{e}");
    }

    #[test]
    fn the_agents_offered_are_names() {
        let c: Config = serde_json::from_str(r#"{"agents":["claude","gemini"]}"#).unwrap();
        assert_eq!(c.agents, ["claude", "gemini"]);
        assert!(Config::default().agents.is_empty());
    }

    #[test]
    fn badges_are_commands() {
        let c: Config = serde_json::from_str(r#"{"workspaces":{"badges":["gh-pr","echo hi"]}}"#).unwrap();
        assert_eq!(c.workspaces.badges, ["gh-pr", "echo hi"]);
        let e = serde_json::from_str::<Config>(r#"{"workspaces":{"badge":["gh-pr"]}}"#).unwrap_err();
        assert!(e.to_string().contains("unknown field `badge`"), "{e}");
    }

    #[test]
    fn a_key_that_is_not_known_is_refused() {
        let e = serde_json::from_str::<Config>(r#"{"assistant":{"interpret":"claude"}}"#).unwrap_err();
        assert!(e.to_string().contains("unknown field `assistant`"), "{e}");
        let e = serde_json::from_str::<Config>(r#"{"assistants":{"triage":"claude"}}"#).unwrap_err();
        assert!(e.to_string().contains("unknown field `triage`"), "{e}");
        let e = serde_json::from_str::<Config>(r#"{"assistants":{"interpret":{"agent":"claude","modle":"x"}}}"#)
            .unwrap_err();
        assert!(e.to_string().contains("unknown field `modle`"), "{e}");
        let e = serde_json::from_str::<Config>(r#"{"assistants":{"interpret":{"model":"x"}}}"#).unwrap_err();
        assert!(e.to_string().contains("missing field `agent`"), "{e}");
    }
}
