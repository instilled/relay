//! What fires a flow by itself: `when`, a cadence, and `on`, an event it hears.
//!
//! Only an approved flow that is not paused fires. A `when` flow keeps its next
//! time in `next_at`; none means plan it again. An `on` flow hears each event
//! once, as the loop reads it. An event older than five minutes is history, and
//! a flow never hears what it did itself.

use super::Flows;
use super::engine::Fired;
use anyhow::{Context, Result};
use jiff::{SignedDuration, Timestamp};
use rl_flow::cron::Cron;
use rl_flow::def::{FlowDef, When};
use rl_ledger::{decisions, firings, flows, runs};
use rl_proto::flow::Flow;
use rl_proto::schedule::{Missed, Overlap};
use rl_proto::types::{Cause, Event, Run, event_kind};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;
use tracing::{debug, info, warn};

/// A firing this far past due was missed (asleep, or the daemon was down), not merely late.
pub const MISSED_AFTER: Duration = Duration::from_secs(120);
/// Events older than this are history: a daemon that was down does not come
/// back and fire everything it missed.
const HORIZON: SignedDuration = SignedDuration::from_secs(300);

/// When `def` fires next after `after`, given when it last did. `every` counts
/// from the last firing; a cron line from the clock.
pub fn next_fire(def: &FlowDef, after: Timestamp, last: Option<Timestamp>) -> Option<Timestamp> {
    match def.trigger.when.as_ref()? {
        When::Cron(line) => Cron::parse(line).ok()?.next_after(after),
        When::Every(secs) => {
            let every = SignedDuration::from_secs(i64::try_from(*secs).ok()?);
            last.unwrap_or(after).checked_add(every).ok()
        }
    }
}

impl Flows {
    /// Fires every approved, unpaused flow whose `next_at` is due; plans those with none.
    /// A firing missed by more than MISSED_AFTER follows `missed`. Returns when to look again.
    pub(crate) async fn due(&self, now: Timestamp) -> Result<Option<Timestamp>> {
        let all = self.ledger.call(|c| flows::list(c, false)).await?;
        let mut soonest: Option<Timestamp> = None;
        for f in all.iter().filter(|f| !f.paused && f.approved()) {
            let Some(def) = def_of(f) else { continue };
            if def.trigger.when.is_none() {
                continue;
            }
            let next = match f.next_at {
                None => self.plan(&def, next_fire(&def, now, f.last_fired_at)).await,
                Some(at) if at <= now => self.on_time(&def, now, at).await,
                at => Ok(at),
            };
            match next {
                Ok(Some(at)) => soonest = Some(soonest.map_or(at, |s| s.min(at))),
                Ok(None) => {}
                Err(e) => warn!(flow = f.name, error = %format!("{e:#}"), "flows: when"),
            }
        }
        Ok(soonest)
    }

    /// A `when` flow whose time has come: fired, or said missed, and planned again. While
    /// it waits on its last firing (`overlap: queue`) it stays due.
    async fn on_time(&self, def: &FlowDef, now: Timestamp, due: Timestamp) -> Result<Option<Timestamp>> {
        let late = now.duration_since(due).unsigned_abs() > MISSED_AFTER;
        if late && def.missed == Missed::Skip {
            info!(flow = def.name, %due, "flow missed its time");
            let data = json!({ "flow": def.name, "reason": "missed", "due": due });
            let summary = format!("flow `{}` missed its time", def.name);
            self.record(def, event_kind::FLOW_SKIPPED, summary, data).await?;
            return self.plan(def, next_fire(def, now, None)).await;
        }
        match self.fire(&def.name, Fired::When, None).await {
            Ok(Some(_)) => self.plan(def, next_fire(def, now, Some(now))).await,
            Ok(None) if def.overlap == Overlap::Queue => Ok(Some(due)),
            Ok(None) => self.plan(def, next_fire(def, now, None)).await,
            Err(e) => {
                warn!(flow = def.name, error = %format!("{e:#}"), "flow did not fire on time");
                self.plan(def, next_fire(def, now, None)).await
            }
        }
    }

    /// Writes when `def` fires next, and says it.
    async fn plan(&self, def: &FlowDef, next: Option<Timestamp>) -> Result<Option<Timestamp>> {
        let name = def.name.clone();
        self.ledger.call(move |c| flows::set_next(c, &name, next)).await?;
        Ok(next)
    }

    /// Plans `name` afresh from now, as a resumed flow is: what it slept through while
    /// paused is not made up.
    pub(crate) async fn replan(&self, name: &str) -> Result<()> {
        let n = name.to_string();
        let Some(f) = self.ledger.call(move |c| flows::get(c, &n)).await? else {
            return Ok(());
        };
        if let Some(def) = def_of(&f).filter(|d| d.trigger.when.is_some()) {
            self.plan(&def, next_fire(&def, Timestamp::now(), None)).await?;
        }
        self.wake.notify_one();
        Ok(())
    }

    /// Fires the flows whose `on` hears `e`.
    pub(crate) async fn heard(&self, e: &Event) -> Result<()> {
        if Timestamp::now().duration_since(e.at) > HORIZON {
            return Ok(());
        }
        let all = self.ledger.call(|c| flows::list(c, false)).await?;
        let listening: Vec<(&Flow, FlowDef)> = all
            .iter()
            .filter_map(|f| def_of(f).map(|d| (f, d)))
            .filter(|(_, d)| hears(d, &e.kind))
            .collect();
        if listening.is_empty() {
            return Ok(());
        }
        let (run, by_step) = self.about(e).await?;
        for (f, def) in &listening {
            let v = verdict(f, def, e, run.as_ref(), by_step.as_deref(), Timestamp::now());
            if v != Verdict::Fires {
                debug!(flow = f.name, event = e.id, verdict = ?v, "not fired");
                continue;
            }
            if let Err(err) = self.fire(&f.name, Fired::On, Some(e)).await {
                warn!(flow = f.name, event = e.id, error = %format!("{err:#}"), "flow did not fire");
            }
        }
        Ok(())
    }

    /// The run an event is about, and, for a decision's event, the flow whose step asked.
    async fn about(&self, e: &Event) -> Result<(Option<Run>, Option<String>)> {
        let run = e.run.clone();
        let decision = e
            .kind
            .starts_with("decision.")
            .then(|| e.data.as_ref()?.get("decision")?.as_str().map(String::from))
            .flatten();
        self.ledger
            .call(move |c| {
                let run = match run {
                    Some(id) => runs::get(c, &id)?,
                    None => None,
                };
                let by_step = match decision {
                    Some(d) => match decisions::by_step(c, &d)? {
                        Some(by) => {
                            let firing = by.split_once('/').map_or(by.as_str(), |(f, _)| f);
                            firings::get(c, firing)?.map(|f| f.flow)
                        }
                        None => None,
                    },
                    None => None,
                };
                Ok((run, by_step))
            })
            .await
            .context("what the event is about")
    }
}

/// The flow's definition as loaded; a row that does not read is passed over.
fn def_of(f: &Flow) -> Option<FlowDef> {
    serde_json::from_value(f.def.clone())
        .inspect_err(|e| warn!(flow = f.name, error = %e, "the flow as loaded"))
        .ok()
}

/// Whether `def`'s `on` takes an event of this kind.
fn hears(def: &FlowDef, kind: &str) -> bool {
    def.trigger.on.iter().any(|p| kind_matches(p, kind))
}

/// Whether an `on` pattern takes an event of this kind: `run.failed`, `run.*`, `*`.
pub(crate) fn kind_matches(pattern: &str, kind: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => kind.starts_with(prefix),
        None => pattern == kind,
    }
}

/// Why a flow did or did not fire on an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Fires,
    /// Its `on` does not take this kind of event.
    NotThisKind,
    /// `where` said no; which field is in the string.
    Filtered(String),
    NotApproved,
    Paused,
    /// It fired too recently.
    Throttled,
    /// The event came from the flow itself: one of its runs, questions or own events.
    ItsOwnDoing,
}

/// Whether flow `f` fires on event `e`, and why not when it does not. `run` is the run the
/// event is about; `by_step`, for a decision's event, the flow whose step asked.
pub fn verdict(
    f: &Flow,
    def: &FlowDef,
    e: &Event,
    run: Option<&Run>,
    by_step: Option<&str>,
    now: Timestamp,
) -> Verdict {
    if !hears(def, &e.kind) {
        return Verdict::NotThisKind;
    }
    if !f.approved() {
        return Verdict::NotApproved;
    }
    if f.paused || f.removed_at.is_some() {
        return Verdict::Paused;
    }
    let its_run = run.is_some_and(|r| matches!(&r.cause, Cause::Flow { flow, .. } if *flow == f.name));
    let its_event =
        e.kind.starts_with("flow.") && e.data.as_ref().and_then(|d| d.get("flow")?.as_str()) == Some(f.name.as_str());
    if its_run || its_event || by_step == Some(f.name.as_str()) {
        return Verdict::ItsOwnDoing;
    }
    if let Some(w) = &def.trigger.filter {
        if let Some(tag) = &w.tag
            && run.is_none_or(|r| !r.tags.iter().any(|t| t == tag))
        {
            return Verdict::Filtered("tag".into());
        }
        if let Some(cause) = &w.cause
            && run.is_none_or(|r| !r.cause.to_string().starts_with(cause))
        {
            return Verdict::Filtered("cause".into());
        }
        if let Some(cli) = &w.cli
            && run.is_none_or(|r| r.cli.as_deref() != Some(cli))
        {
            return Verdict::Filtered("cli".into());
        }
        if let Some(group) = &w.group
            && &e.group_key != group
        {
            return Verdict::Filtered("group".into());
        }
        for (field, want) in &w.data {
            if event_field(e, field).is_none_or(|got| !want.matches(&got)) {
                return Verdict::Filtered(format!("data.{field}"));
            }
        }
    }
    let gap = SignedDuration::from_secs(i64::try_from(def.throttle_secs).unwrap_or(i64::MAX));
    if !gap.is_zero()
        && let Some(last) = f.last_fired_at
        && last.checked_add(gap).is_ok_and(|next| next > now)
    {
        return Verdict::Throttled;
    }
    Verdict::Fires
}

/// One field of an event's `data`, as text: what `where.data` matches and
/// `{event.data.<field>}` fills, rendered the same way in both.
fn event_field(e: &Event, field: &str) -> Option<String> {
    e.data.as_ref()?.get(field).map(text)
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// What an event says, by the name `{event.<name>}` gives it.
pub(super) fn vars(e: &Event, run: Option<&Run>) -> BTreeMap<String, String> {
    let of_run = |f: fn(&Run) -> Option<String>| run.and_then(f).unwrap_or_default();
    let mut v = BTreeMap::from([
        ("kind".to_string(), e.kind.clone()),
        ("summary".into(), e.summary.clone()),
        ("group".into(), e.group_key.clone()),
        ("workspace".into(), e.workspace.to_string_lossy().into_owned()),
        ("run".into(), e.run.clone().unwrap_or_default()),
        ("cwd".into(), of_run(|r| Some(r.cwd.to_string_lossy().into_owned()))),
        ("title".into(), of_run(|r| Some(r.title.clone()))),
        ("cause".into(), of_run(|r| Some(r.cause.to_string()))),
        ("cli".into(), of_run(|r| r.cli.clone())),
    ]);
    if let Some(Value::Object(map)) = &e.data {
        v.extend(map.iter().map(|(k, val)| (format!("data.{k}"), text(val))));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::types::RunKind;
    use std::path::PathBuf;

    fn def(yaml: &str) -> FlowDef {
        let text = format!("---\n{yaml}steps:\n  a:\n    run: x\n---\n");
        rl_flow::def::check("f", rl_flow::doc::read(&text).unwrap(), &|_| true).unwrap()
    }

    fn flow(def: &FlowDef) -> Flow {
        Flow {
            name: "f".into(),
            hash: "h".into(),
            def: serde_json::to_value(def).unwrap(),
            source: PathBuf::new(),
            folder: None,
            paused: false,
            approved_hash: Some("h".into()),
            last_fired_at: None,
            next_at: None,
            removed_at: None,
        }
    }

    fn event(kind: &str, data: Value) -> Event {
        Event {
            id: 1,
            at: Timestamp::UNIX_EPOCH,
            workspace: "/w".into(),
            run: Some("r1".into()),
            group_key: "/w".into(),
            kind: kind.into(),
            summary: "it broke".into(),
            data: Some(data),
        }
    }

    fn run(cause: Cause) -> Run {
        let mut r = Run::new(
            "r1",
            RunKind::Command,
            "probe",
            vec![],
            "/w/sub",
            "/w",
            Timestamp::UNIX_EPOCH,
        );
        r.cause = cause;
        r
    }

    #[test]
    fn every_counts_from_the_last_firing_and_cron_from_the_clock() {
        let now: Timestamp = "2026-09-09T10:00:00Z".parse().unwrap();
        let every = def("when: every 30m\n");
        assert_eq!(
            next_fire(&every, now, None),
            Some("2026-09-09T10:30:00Z".parse().unwrap())
        );
        let last: Timestamp = "2026-09-09T09:50:00Z".parse().unwrap();
        assert_eq!(
            next_fire(&every, now, Some(last)),
            Some("2026-09-09T10:20:00Z".parse().unwrap())
        );
        let cron = def("when: \"*/15 * * * *\"\n");
        let next = next_fire(&cron, now, Some(last)).unwrap();
        assert!(next > now && next <= now + SignedDuration::from_mins(15), "{next}");
        assert_eq!(next_fire(&def(""), now, None), None, "no `when`, no time");
    }

    #[test]
    fn kinds_where_approval_pause_and_throttle_decide() {
        let now = Timestamp::UNIX_EPOCH + SignedDuration::from_secs(100);
        let d = def("on: [run.*, logs.issue]\nwhere: { cause: \"flow:nightly\", data: { sev: [high, critical] } }\n");
        let nightly = run(Cause::Flow {
            flow: "nightly".into(),
            step: "a".into(),
        });
        let f = flow(&d);
        let high = event("run.failed", json!({ "sev": "high" }));
        assert_eq!(verdict(&f, &d, &high, Some(&nightly), None, now), Verdict::Fires);
        let other = event("deploy.done", json!({ "sev": "high" }));
        assert_eq!(verdict(&f, &d, &other, Some(&nightly), None, now), Verdict::NotThisKind);
        let low = event("logs.issue", json!({ "sev": "low" }));
        assert_eq!(
            verdict(&f, &d, &low, Some(&nightly), None, now),
            Verdict::Filtered("data.sev".into())
        );
        assert_eq!(
            verdict(&f, &d, &high, Some(&run(Cause::Manual)), None, now),
            Verdict::Filtered("cause".into())
        );
        assert_eq!(
            verdict(&f, &d, &high, None, None, now),
            Verdict::Filtered("cause".into()),
            "no run, no cause"
        );

        let unapproved = Flow {
            approved_hash: Some("old".into()),
            ..f.clone()
        };
        assert_eq!(
            verdict(&unapproved, &d, &high, Some(&nightly), None, now),
            Verdict::NotApproved
        );
        let paused = Flow {
            paused: true,
            ..f.clone()
        };
        assert_eq!(verdict(&paused, &d, &high, Some(&nightly), None, now), Verdict::Paused);
        let lately = Flow {
            last_fired_at: Some(now - SignedDuration::from_secs(5)),
            ..f.clone()
        };
        assert_eq!(
            verdict(&lately, &d, &high, Some(&nightly), None, now),
            Verdict::Throttled
        );
        let a_while_ago = Flow {
            last_fired_at: Some(now - SignedDuration::from_secs(11)),
            ..f.clone()
        };
        assert_eq!(
            verdict(&a_while_ago, &d, &high, Some(&nightly), None, now),
            Verdict::Fires
        );
    }

    #[test]
    fn a_flow_does_not_hear_its_own_runs_questions_or_events() {
        let now = Timestamp::UNIX_EPOCH;
        let d = def("on: \"*\"\nthrottle: 0\n");
        let f = flow(&d);
        let mine = run(Cause::Flow {
            flow: "f".into(),
            step: "a".into(),
        });
        let theirs = run(Cause::Flow {
            flow: "g".into(),
            step: "a".into(),
        });
        let ended = event("run.finished", json!({}));
        assert_eq!(verdict(&f, &d, &ended, Some(&mine), None, now), Verdict::ItsOwnDoing);
        assert_eq!(verdict(&f, &d, &ended, Some(&theirs), None, now), Verdict::Fires);
        let taken = event("decision.taken", json!({ "decision": "d1" }));
        assert_eq!(verdict(&f, &d, &taken, None, Some("f"), now), Verdict::ItsOwnDoing);
        assert_eq!(verdict(&f, &d, &taken, None, Some("g"), now), Verdict::Fires);
        for kind in ["flow.fired", "flow.ended", "flow.skipped"] {
            let own = event(kind, json!({ "flow": "f" }));
            assert_eq!(verdict(&f, &d, &own, None, None, now), Verdict::ItsOwnDoing, "{kind}");
            let other = event(kind, json!({ "flow": "g" }));
            assert_eq!(verdict(&f, &d, &other, None, None, now), Verdict::Fires, "{kind}");
        }
    }

    #[test]
    fn an_event_is_read_by_name() {
        let mut r = run(Cause::Manual);
        r.cli = Some("claude".into());
        let v = vars(&event("logs.issue", json!({ "sev": "high", "n": 3 })), Some(&r));
        let want = [
            ("kind", "logs.issue"),
            ("summary", "it broke"),
            ("group", "/w"),
            ("workspace", "/w"),
            ("run", "r1"),
            ("cwd", "/w/sub"),
            ("title", "probe"),
            ("cause", "manual"),
            ("cli", "claude"),
            ("data.sev", "high"),
            ("data.n", "3"),
        ];
        for (k, val) in want {
            assert_eq!(v.get(k).map(String::as_str), Some(val), "{k}");
        }
        assert_eq!(vars(&event("x.y", json!({})), None)["cwd"], "");
    }
}
