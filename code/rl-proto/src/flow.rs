//! A flow file's shape: `flow.yaml` in a flow's folder.
//!
//! ```yaml
//! when: every 30m          # or `on: run.failed`, or neither (by hand only)
//! steps:
//!   probe:
//!     run: new-log-errors.sh
//!   triage:
//!     after: probe
//!     if: probe.out
//!     run: { agent: claude }
//!   review:
//!     after: triage
//!     ask: "{triage.out}"
//!     options:
//!       apply: { run: { agent: claude } }
//!       ignore: {}
//!     default: ignore
//!     expires: 4h
//! ```
//!
//! Steps and options are maps whose order matters, so they are read into
//! lists of pairs in the order they are written ([`ordered`]).

use crate::action::{Action, action_struct};
use crate::reaction::Filter;
use crate::schedule::{Missed, Overlap};
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::marker::PhantomData;
use std::path::PathBuf;

/// What a flow file says. How a flow starts is `when` (a time) or `on` (an
/// event kind); with neither it runs only when asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowSpec {
    /// The workspace the steps run in, a path; `~` allowed.
    #[serde(default, rename = "in", skip_serializing_if = "Option::is_none")]
    pub in_: Option<PathBuf>,
    /// A time to fire at: `every 30m`, or a cron line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Event kinds to fire on, one or a list.
    #[serde(
        default,
        deserialize_with = "crate::flow::kinds",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub on: Vec<String>,
    /// What else has to be true of the run an event is about.
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    pub filter: Option<Filter>,
    /// What to do about a time that passed while asleep or down.
    #[serde(default)]
    pub missed: Missed,
    /// What to do when the last firing is still going.
    #[serde(default)]
    pub overlap: Overlap,
    /// The least time between two firings: `10m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throttle: Option<String>,
    /// The steps, in the order they are written.
    #[serde(with = "crate::flow::ordered")]
    pub steps: Vec<(String, StepSpec)>,
}

action_struct! {
    #[serde(deny_unknown_fields)]
    struct StepSpec {
        before: {
            /// The steps that must finish first.
            #[serde(default, deserialize_with = "crate::flow::one_or_many", skip_serializing_if = "Vec::is_empty")]
            pub after: Vec<String>,
            /// Runs only if this holds of an earlier step: `probe.out`.
            #[serde(default, rename = "if", skip_serializing_if = "Option::is_none")]
            pub if_: Option<String>,
            /// Where it runs, instead of the flow's workspace.
            #[serde(default, rename = "in", skip_serializing_if = "Option::is_none")]
            pub in_: Option<PathBuf>,
            /// A question for the person, in place of (or before) a run.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub ask: Option<String>,
            /// The answers, each with what it starts, in the order written.
            #[serde(default, with = "crate::flow::ordered", skip_serializing_if = "Vec::is_empty")]
            pub options: Vec<(String, Action)>,
            /// The answer taken when it expires.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub default: Option<String>,
            /// How long the question waits: `4h`.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub expires: Option<String>,
            /// Questions with the same group are asked as one.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub group: Option<String>,
            /// More than one answer may be chosen.
            #[serde(default, skip_serializing_if = "std::ops::Not::not")]
            pub multi: bool,
        }
        after: {}
    }
}

/// A map read into pairs, in the order it is written. A key given twice is
/// an error, which a plain map would swallow.
pub mod ordered {
    use super::*;

    pub fn serialize<S: Serializer, T: Serialize>(pairs: &[(String, T)], s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(pairs.len()))?;
        for (k, v) in pairs {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<(String, T)>, D::Error> {
        struct Pairs<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for Pairs<T> {
            type Value = Vec<(String, T)>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a map")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut pairs: Vec<(String, T)> = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if pairs.iter().any(|(k, _)| *k == key) {
                        return Err(de::Error::custom(format!("step `{key}` is named twice")));
                    }
                    pairs.push((key, map.next_value()?));
                }
                Ok(pairs)
            }
        }

        d.deserialize_map(Pairs(PhantomData))
    }
}

/// `after: probe` and `after: [probe, triage]` both mean a list.
pub fn one_or_many<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

/// `on: run.failed` and `on: [run.failed, run.blocked]` both mean a list,
/// which is never empty and never holds a blank.
pub fn kinds<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let kinds = one_or_many(d)?;
    if kinds.is_empty() || kinds.iter().any(|k| k.trim().is_empty()) {
        return Err(de::Error::custom("`on` needs an event kind, or a list of them"));
    }
    Ok(kinds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_keep_the_order_they_are_written_in() {
        // Fed as text: `json!` would sort the keys before they were read.
        let s: FlowSpec = serde_json::from_str(
            r#"{
                "when": "every 30m",
                "steps": {
                    "probe": { "run": "new-log-errors.sh" },
                    "triage": { "after": "probe", "if": "probe.out", "run": { "agent": "claude" } },
                    "review": { "after": ["triage"], "ask": "{triage.out}",
                                "options": { "apply": { "run": { "agent": "claude" } }, "ignore": {} },
                                "default": "ignore", "expires": "4h" }
                }
            }"#,
        )
        .unwrap();
        let names: Vec<_> = s.steps.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["probe", "triage", "review"]);
        let review = &s.steps[2].1;
        assert_eq!(review.after, ["triage"]);
        assert_eq!(
            review.options.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            ["apply", "ignore"]
        );
        assert!(review.options[1].1.is_empty());
    }

    #[test]
    fn an_unknown_key_or_a_step_named_twice_is_refused() {
        let e = serde_json::from_value::<FlowSpec>(serde_json::json!({ "steps": {}, "cron": "x" }));
        assert!(e.unwrap_err().to_string().contains("cron"));
        // json! cannot repeat a key, so the text is fed as it is.
        let e = serde_json::from_str::<FlowSpec>(r#"{"steps":{"a":{"run":"x"},"a":{"run":"y"}}}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("named twice"), "{e}");
    }

    #[test]
    fn a_flow_writes_back_as_it_was_written() {
        let text = r#"{
            "in": "~/work/api",
            "on": ["run.failed", "run.blocked"],
            "where": { "tag": "nightly" },
            "throttle": "10m",
            "steps": {
                "b": { "run": ["echo", "b"] },
                "a": { "after": ["b"], "run": { "agent": "claude" } }
            }
        }"#;
        let s: FlowSpec = serde_json::from_str(text).unwrap();
        assert_eq!(s.in_, Some(PathBuf::from("~/work/api")));
        assert_eq!(s.on, ["run.failed", "run.blocked"]);
        let out = serde_json::to_string(&s).unwrap();
        assert!(out.find("\"b\":{").unwrap() < out.find("\"a\":{").unwrap(), "{out}");
        assert_eq!(serde_json::from_str::<FlowSpec>(&out).unwrap(), s);
    }

    #[test]
    fn on_takes_one_kind_or_many_but_never_none() {
        let on = |v: serde_json::Value| serde_json::from_value::<FlowSpec>(serde_json::json!({ "on": v, "steps": {} }));
        assert_eq!(on(serde_json::json!("run.failed")).unwrap().on, ["run.failed"]);
        assert_eq!(on(serde_json::json!(["a", "b"])).unwrap().on, ["a", "b"]);
        assert!(on(serde_json::json!([])).is_err());
        assert!(on(serde_json::json!(" ")).is_err());
    }
}
