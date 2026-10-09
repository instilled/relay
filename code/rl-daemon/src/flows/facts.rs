//! What a firing's steps left, read one way wherever it is asked for — placeholders, `if:`,
//! `group:`, `rl flow state get`, a command's environment: `{x.out}` from the step's run,
//! `{x.choice}` and `{x.note}` from its question, any other `{x.key}` from the values it set;
//! and the event an `on` firing fired on.

use super::triggers::vars;
use rl_flow::def::FlowDef;
use rl_flow::plan;
use rl_flow::template::{Lookup, Ref};
use rl_ledger::{decisions, firings, session_io};
use rl_proto::flow::{Firing, StepState};
use rl_proto::types::{Event, Run};
use rusqlite::Connection;
use std::collections::BTreeMap;
use tracing::warn;

/// How much of a step's output is its `out`: the last of it.
pub(super) const OUT_MAX: usize = 64 << 10;

/// What a step is handed of the steps it comes after, for its environment and `$RELAY_IN`.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct Handed {
    /// `(step, out)`.
    pub(super) outs: Vec<(String, String)>,
    /// `(step, key, value)`.
    pub(super) values: Vec<(String, String, String)>,
}

pub(super) struct Facts<'a> {
    conn: &'a Connection,
    firing: &'a str,
    steps: &'a [StepState],
    event: Option<BTreeMap<String, String>>,
}

impl<'a> Facts<'a> {
    pub(super) fn new(conn: &'a Connection, firing: &'a Firing, event: Option<&(Event, Option<Run>)>) -> Self {
        let event = event.map(|(e, run)| vars(e, run.as_ref()));
        Self {
            conn,
            firing: &firing.id,
            steps: &firing.steps,
            event,
        }
    }

    /// What `step` printed: the last 64 KB of its run, ANSI stripped and trimmed.
    pub(super) fn out(&self, step: &str) -> Option<String> {
        self.get(&Ref {
            head: step.to_string(),
            rest: "out".to_string(),
        })
    }

    /// What `step` is handed of the steps it comes after: each one's out, and every value it
    /// left — an ask's choice and note among them.
    pub(super) fn handed(&self, def: &FlowDef, step: &str) -> anyhow::Result<Handed> {
        let mut h = Handed::default();
        for up in plan::upstream(def, step) {
            if let Some(out) = self.out(&up) {
                h.outs.push((up.clone(), out));
            }
            for key in ["choice", "note"] {
                let r = Ref {
                    head: up.clone(),
                    rest: key.to_string(),
                };
                if let Some(v) = self.get(&r) {
                    h.values.push((up.clone(), key.to_string(), v));
                }
            }
            for (key, v) in firings::values(self.conn, self.firing, &up)? {
                h.values.push((up.clone(), key, v));
            }
        }
        Ok(h)
    }
}

impl Lookup for Facts<'_> {
    fn get(&self, r: &Ref) -> Option<String> {
        if r.head == "event" {
            return self.event.as_ref()?.get(&r.rest).cloned();
        }
        let s = self.steps.iter().find(|s| s.name == r.head)?;
        let found = match r.rest.as_str() {
            "out" => {
                let run = s.run.as_deref()?;
                session_io::tail(self.conn, run, OUT_MAX).map(|b| Some(rl_detect::ansi::strip(&b).trim().to_string()))
            }
            "choice" | "note" => decisions::get(self.conn, s.decision.as_deref()?).map(|d| {
                let d = d?;
                match r.rest.as_str() {
                    "choice" => (!d.chosen.is_empty()).then(|| d.chosen.join(", ")),
                    _ => d.note,
                }
            }),
            key => firings::value(self.conn, self.firing, &s.name, key),
        };
        found.unwrap_or_else(|e| {
            warn!(step = s.name, error = %format!("{e:#}"), "reading what a step left");
            None
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use rl_flow::cond;
    use rl_ledger::Ledger;
    use rl_ledger::firings::NewFiring;

    fn r(head: &str, rest: &str) -> Ref {
        Ref {
            head: head.into(),
            rest: rest.into(),
        }
    }

    #[test]
    fn a_value_a_step_set_is_read_as_its_placeholder_and_its_if() {
        let ledger = Ledger::open_in_memory().unwrap();
        let (sig, found, missing, other, holds, not) = ledger
            .call_blocking(|c| {
                let steps = ["probe", "triage"].map(String::from);
                firings::insert(
                    c,
                    &NewFiring {
                        id: "f1",
                        flow: "prod-logs",
                        def_json: "{}",
                        hash: "h",
                        trigger: "hand",
                        event: None,
                        at: Timestamp::UNIX_EPOCH,
                        steps: &steps,
                    },
                )?;
                firings::set_values(
                    c,
                    "f1",
                    "probe",
                    &[("signature".into(), "abc".into()), ("found".into(), "3".into())],
                )?;
                let f = firings::get(c, "f1")?.unwrap();
                let facts = Facts::new(c, &f, None);
                Ok((
                    facts.get(&r("probe", "signature")),
                    facts.get(&r("probe", "found")),
                    facts.get(&r("probe", "missing")),
                    facts.get(&r("nope", "signature")),
                    cond::parse("probe.found").unwrap().holds(&facts),
                    cond::parse("triage.found").unwrap().holds(&facts),
                ))
            })
            .unwrap();
        assert_eq!(sig.as_deref(), Some("abc"));
        assert_eq!(found.as_deref(), Some("3"));
        assert_eq!(missing, None);
        assert_eq!(other, None, "no such step");
        assert!(holds, "if: probe.found");
        assert!(!not, "a step that set nothing has nothing set");
    }
}
