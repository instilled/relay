//! `~/.relay/config.yaml`: your defaults, human-edited. Absent means all
//! defaults; a key that is not known is refused, so a typo cannot silently
//! do nothing. Keys are added one at a time, when a real need shows up.
//!
//! ```yaml
//! agents: [claude, codex]        # the agents the panel offers; unset: all of them
//! assistants:
//!   interpret: claude            # or, with a model and extra arguments:
//!   interpret: { agent: claude, model: claude-haiku-4-5-20251001, args: ["--max-turns", "1"] }
//!   summary: claude              # a recap where the agent keeps none (/summary, rl summary)
//! retention: 30d                 # the journal of a run ended this long ago goes (the default);
//!                                # `never` keeps every byte. Or { journal: 30d }
//! runs:
//!   reap_after: 6h               # a run nothing has come for in this long stops itself; `never` keeps it
//!   settle: 5ms                  # how long a burst of output may gather before the holder passes it on
//! workspaces:
//!   badges: [gh-pr]              # beside a git folder's branch on its header: `PR #95`
//! catch_up:
//!   after: 8h                    # away longer than this, the panel opens on what happened; `off` never
//!   summary: auto                # `auto`: the top is written when you come back after `after`,
//!                                # by assistants.summary · `ask`: only on /summary
//! report:
//!   brief_after: 2h              # back within this, the report opens on what needs you; else on all teams
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
    /// The one thing Relay deletes: a run's terminal bytes, once it ended
    /// long enough ago. Unset means the default, not "never" — see
    /// [`Config::retention_journal`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
    /// What becomes of a run Relay has lost touch with.
    #[serde(default, skip_serializing_if = "Runs::is_empty")]
    pub runs: Runs,
    /// What a folder's header says.
    #[serde(default, skip_serializing_if = "Workspaces::is_empty")]
    pub workspaces: Workspaces,
    /// What the panel says when you come back after a while.
    #[serde(default, skip_serializing_if = "CatchUp::is_empty")]
    pub catch_up: CatchUp,
    /// How deep the report goes, by how long you were away.
    #[serde(default, skip_serializing_if = "ReportConfig::is_empty")]
    pub report: ReportConfig,
}

/// Where the report opens: on the first thing that needs you when you were
/// away less than `brief_after`, else on all the teams. Named `ReportConfig` so it does not clash with `methods::Report`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportConfig {
    /// Away less than this, the report opens on what needs you. Unset: `2h`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_after: Option<String>,
}

impl ReportConfig {
    pub const DEFAULT_BRIEF_AFTER: &str = "2h";

    pub fn is_empty(&self) -> bool {
        self.brief_after.is_none()
    }

    /// What the file says, or the default when unset or not a duration.
    pub fn brief_after(&self) -> jiff::SignedDuration {
        Self::duration(self.brief_after.as_deref(), Self::DEFAULT_BRIEF_AFTER)
    }

    fn duration(said: Option<&str>, default: &str) -> jiff::SignedDuration {
        said.and_then(rl_core::time::parse_duration)
            .unwrap_or_else(|| rl_core::time::parse_duration(default).expect("the default is a duration"))
    }
}

/// What the panel does when you open it after being away.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatchUp {
    /// Away longer than this, the panel opens on the catch-up; `off` (or
    /// `never`) never does by itself. Unset: `8h`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// `auto`: the top is written when you come back after `after`, by
    /// `assistants.summary` · `ask`: only on /summary. Unset: `auto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl CatchUp {
    /// How long away, at the least, before the panel opens on the catch-up.
    pub const DEFAULT_AFTER: &str = "8h";

    pub fn is_empty(&self) -> bool {
        self.after.is_none() && self.summary.is_none()
    }

    /// What the file says, or the default; `None` when it never opens by
    /// itself. A value that is not a duration falls back to the default, and
    /// the daemon's report says so.
    pub fn after(&self) -> Option<jiff::SignedDuration> {
        let said = self.after.as_deref().unwrap_or(Self::DEFAULT_AFTER);
        if is_never(said) {
            return None;
        }
        Some(
            rl_core::time::parse_duration(said).unwrap_or_else(|| {
                rl_core::time::parse_duration(Self::DEFAULT_AFTER).expect("the default is a duration")
            }),
        )
    }

    /// Whether the top is written when the catch-up opens: unless the file
    /// says `ask`.
    pub fn auto_summary(&self) -> bool {
        !self.summary.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("ask"))
    }
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

impl Config {
    /// How long a run's journal is kept after it ends: what the file says,
    /// or the default. `never` keeps every byte for good. Only the terminal
    /// bytes and log of an *ended* run are ever at stake — the run itself,
    /// its events and its summary stay whatever this says.
    pub fn retention_journal(&self) -> &str {
        self.retention
            .as_ref()
            .and_then(|r| r.journal.as_deref())
            .unwrap_or(DEFAULT_RETENTION)
    }
}

impl Workspaces {
    pub fn is_empty(&self) -> bool {
        self.badges.is_empty()
    }
}

/// How a run's holder behaves between Relay and its child: how long it
/// waits for a Relay that may never return, and how eagerly it reads.
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
    /// Having caught up with its child, the holder waits this long before
    /// reading again, so a burst of small writes arrives as one chunk instead
    /// of one wake-up each — and a child that never stops writing meets the
    /// backpressure its terminal would give it anywhere else. It costs the
    /// next chunk at most this much. `off` reads as fast as it can. Unset:
    /// `5ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle: Option<String>,
}

impl Runs {
    pub fn is_empty(&self) -> bool {
        self.reap_after.is_none() && self.settle.is_none()
    }

    /// What the file says, or the default. `Some("never")` is not a duration:
    /// the daemon reads it as "wait forever".
    pub fn reap_after(&self) -> &str {
        self.reap_after.as_deref().unwrap_or(DEFAULT_REAP)
    }

    /// What the file says about settling, or the default.
    pub fn settle(&self) -> &str {
        self.settle.as_deref().unwrap_or(DEFAULT_SETTLE)
    }
}

/// How long a run's journal is kept once it has ended, unset.
pub const DEFAULT_RETENTION: &str = "30d";

/// How long a run waits for a Relay that never comes back, unset.
pub const DEFAULT_REAP: &str = "6h";

/// How long a burst may gather before the holder passes it on, unset.
pub const DEFAULT_SETTLE: &str = "5ms";

/// Whether a `reap_after` value means "wait forever", or a `settle` one
/// "do not wait at all".
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
    /// flow, drafted for your enter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpret: Option<Assistant>,
    /// What an ended run did, in a few sentences (`s`, `rl summary`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Assistant>,
}

impl Assistants {
    pub fn is_empty(&self) -> bool {
        self.interpret.is_none() && self.summary.is_none()
    }

    /// The assistant for a job, by its name in the file.
    pub fn for_job(&self, job: &str) -> Option<Option<&Assistant>> {
        match job {
            "interpret" => Some(self.interpret.as_ref()),
            "summary" => Some(self.summary.as_ref()),
            _ => None,
        }
    }

    /// The jobs there are, in the order `rl config` lists them.
    pub const JOBS: &[&str] = &["interpret", "summary"];
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
    use jiff::SignedDuration;

    #[test]
    fn report_keys_parse_and_default() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c.report.brief_after(), SignedDuration::from_hours(2));
        let c: Config = serde_json::from_str(r#"{"report":{"brief_after":"30m"}}"#).unwrap();
        assert_eq!(c.report.brief_after(), SignedDuration::from_mins(30));
        let c: Config = serde_json::from_str(r#"{"report":{"brief_after":"soon"}}"#).unwrap();
        assert_eq!(c.report.brief_after(), SignedDuration::from_hours(2));
    }

    #[test]
    fn unknown_report_key_is_refused() {
        assert!(serde_json::from_str::<Config>(r#"{"report":{"brief":"2h"}}"#).is_err());
    }

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
    fn an_ended_runs_journal_is_kept_for_thirty_days_unless_told_otherwise() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c.retention_journal(), "30d");
        let c: Config = serde_json::from_str(r#"{"retention":"7d"}"#).unwrap();
        assert_eq!(c.retention_journal(), "7d");
        let c: Config = serde_json::from_str(r#"{"retention":{"journal":"7d"}}"#).unwrap();
        assert_eq!(c.retention_journal(), "7d");
        let c: Config = serde_json::from_str(r#"{"retention":"never"}"#).unwrap();
        assert!(is_never(c.retention_journal()), "nothing ever goes, when asked");
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
    fn a_burst_gathers_for_five_milliseconds_unless_told_otherwise() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c.runs.settle(), "5ms");
        let c: Config = serde_json::from_str(r#"{"runs":{"settle":"20ms"}}"#).unwrap();
        assert_eq!(c.runs.settle(), "20ms");
        let c: Config = serde_json::from_str(r#"{"runs":{"settle":"off"}}"#).unwrap();
        assert!(is_never(c.runs.settle()));
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

    #[test]
    fn catch_up_defaults_and_its_off() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c.catch_up.after(), Some(SignedDuration::from_hours(8)));
        assert!(c.catch_up.auto_summary());
        let c: Config = serde_json::from_str(r#"{"catch_up":{"after":"off","summary":"ask"}}"#).unwrap();
        assert_eq!(c.catch_up.after(), None);
        assert!(!c.catch_up.auto_summary());
        let c: Config = serde_json::from_str(r#"{"catch_up":{"after":"never"}}"#).unwrap();
        assert_eq!(c.catch_up.after(), None);
        let c: Config = serde_json::from_str(r#"{"catch_up":{"after":"90m","summary":"auto"}}"#).unwrap();
        assert_eq!(c.catch_up.after(), Some(SignedDuration::from_mins(90)));
        assert!(c.catch_up.auto_summary());
        let c: Config = serde_json::from_str(r#"{"catch_up":{"after":"soon"}}"#).unwrap();
        assert_eq!(
            c.catch_up.after(),
            Some(SignedDuration::from_hours(8)),
            "a bad value is the default"
        );
        assert!(
            serde_json::from_str::<Config>(r#"{"catch_up":{"afterr":"2h"}}"#).is_err(),
            "a typo is refused"
        );
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(serde_json::to_string(&c).unwrap(), "{}", "unset stays unwritten");
    }
}
