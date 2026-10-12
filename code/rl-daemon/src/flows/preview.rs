//! `flow.test`: what firing a flow would do now, told without starting anything — whether it may
//! fire by itself, when it would, whether an event would, and each step: what it does, where, and
//! whether it would run. The folders are read again first, so a flow is told as it is on disk.

use super::Flows;
use super::engine::where_runs;
use super::triggers::{HORIZON, Verdict, next_fire, vars, verdict};
use anyhow::Result;
use jiff::Timestamp;
use rl_flow::def::FlowDef;
use rl_flow::preview;
use rl_flow::template::{Lookup, Ref};
use rl_ledger::{events, firings, flows};
use rl_proto::flow::{EventTest, Flow, FlowStatus, Going, Held, StepTest, Tested};
use rl_proto::types::Event;
use std::collections::BTreeMap;
use std::fmt;

/// Why a flow cannot be tested.
#[derive(Debug)]
pub enum Untested {
    /// No such flow, or it did not load: the caller says which.
    Missing,
    NoEvent(i64),
}

impl fmt::Display for Untested {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Untested::Missing => f.write_str("no such flow"),
            Untested::NoEvent(id) => write!(f, "no event {id}: rl events lists them"),
        }
    }
}

impl std::error::Error for Untested {}

/// An event's names, as an `if:` reads them: `{event.data.sev}`.
struct EventVars(BTreeMap<String, String>);

impl Lookup for EventVars {
    fn get(&self, r: &Ref) -> Option<String> {
        if r.head != "event" {
            return None;
        }
        self.0.get(&r.rest).cloned()
    }
}

impl Flows {
    /// What firing `name` would do now, and with `event`, what that event would make of it.
    pub(crate) async fn test(&self, name: &str, event: Option<i64>) -> Result<Tested> {
        self.reload().await?;
        if self.not_loaded().iter().any(|n| n.flow == name) {
            return Err(Untested::Missing.into());
        }
        let n = name.to_string();
        let (flow, last) = self
            .ledger
            .call(move |c| Ok((flows::get(c, &n)?, firings::latest(c, &n)?)))
            .await?;
        let Some(f) = flow.filter(|f| f.removed_at.is_none()) else {
            return Err(Untested::Missing.into());
        };
        let def: FlowDef = serde_json::from_value(f.def.clone())?;
        let now = Timestamp::now();
        let about = match event {
            None => None,
            Some(id) => {
                let e = self
                    .ledger
                    .call(move |c| events::get(c, id))
                    .await?
                    .ok_or(Untested::NoEvent(id))?;
                let (run, by_step) = self.about(&e).await?;
                Some((e, run, by_step))
            }
        };
        let held = match f.status() {
            FlowStatus::NeedsApproval => Some(Held::NeedsYes),
            FlowStatus::Paused => Some(Held::Paused),
            _ => None,
        };
        let going = last.filter(|l| l.ended_at.is_none()).map(|l| Going {
            firing: l.id,
            status: l.status,
            overlap: def.overlap,
        });
        let event = about.as_ref().map(|(e, run, by)| {
            let v = verdict(&f, &def, e, run.as_ref(), by.as_deref(), now);
            EventTest {
                id: e.id,
                kind: e.kind.clone(),
                summary: e.summary.clone(),
                fires: v == Verdict::Fires,
                why: why(&v, &def, e),
                old: now.duration_since(e.at) > HORIZON,
            }
        });
        let lookup = about.as_ref().map(|(e, run, _)| EventVars(vars(e, run.as_ref())));
        let ran = preview::steps(&def, lookup.as_ref().map(|l| l as &dyn Lookup));
        let on = !def.trigger.on.is_empty();
        let home = self.home.path();
        let steps = def
            .steps
            .iter()
            .zip(ran)
            .map(|(s, (_, runs))| {
                let placed = s.in_.is_some() || def.in_.is_some();
                let folder = match &about {
                    _ if placed || !on => Some(where_runs(&def, s, None, home)),
                    Some((e, run, _)) => Some(where_runs(&def, s, Some((e, run.as_ref())), home)),
                    None => None,
                };
                StepTest {
                    name: s.name.clone(),
                    does: preview::does(s),
                    runs,
                    folder,
                }
            })
            .collect();
        Ok(Tested {
            flow: f.name.clone(),
            when: f.when(),
            on: def.trigger.on.clone(),
            held,
            next: next_times(&def, &f, held.is_some(), now),
            going,
            event,
            steps,
        })
    }
}

/// The next three times `def` fires by itself: from the time planned, when it may fire; else
/// from now, as a yes or a resume plans it.
fn next_times(def: &FlowDef, f: &Flow, held: bool, now: Timestamp) -> Vec<Timestamp> {
    let first = match (held, f.next_at) {
        (false, Some(at)) => Some(at),
        (false, None) => next_fire(def, now, f.last_fired_at),
        (true, _) => next_fire(def, now, None),
    };
    std::iter::successors(first, |t| next_fire(def, *t, Some(*t)).filter(|n| n > t))
        .take(3)
        .collect()
}

/// Why an event does or does not fire the flow, in words.
fn why(v: &Verdict, def: &FlowDef, e: &Event) -> String {
    match v {
        Verdict::Fires => "it fires on it".into(),
        Verdict::NotThisKind if def.trigger.on.is_empty() => "it fires on no event".into(),
        Verdict::NotThisKind => format!("it is on {}, not {}", def.trigger.on.join(", "), e.kind),
        Verdict::Filtered(field) => format!("where.{field} does not match"),
        Verdict::NotApproved => "it needs a yes".into(),
        Verdict::Paused => "it is paused".into(),
        Verdict::Throttled => format!("it fired less than {}s ago (throttle)", def.throttle_secs),
        Verdict::ItsOwnDoing => "the event is the flow's own doing, and a flow never hears itself".into(),
    }
}
