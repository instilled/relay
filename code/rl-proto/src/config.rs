//! `~/.relay/config.yaml`: your defaults, human-edited. Absent means all
//! defaults; a key that is not known is refused, so a typo cannot silently
//! do nothing. Keys are added one at a time, when a real need shows up.
//!
//! ```yaml
//! assistants:
//!   interpret: claude            # or, with a model and extra arguments:
//!   interpret: { agent: claude, model: claude-haiku-4-5-20251001, args: ["--max-turns", "1"] }
//!   summarise: claude            # a recap where the agent keeps none (/summarise, rl summarise)
//! retention: 30d                 # the journal of a run ended this long ago goes; or { journal: 30d }
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The agents Relay itself calls on, by job.
    #[serde(default, skip_serializing_if = "Assistants::is_empty")]
    pub assistants: Assistants,
    /// The one thing Relay deletes, and only when told: unset, nothing ever goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
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
