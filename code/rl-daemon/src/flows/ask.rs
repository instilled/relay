//! A firing's questions: an ask step raised through the decision engine, and
//! its answer carrying the firing on. The option chosen runs as the step's own
//! run; the decision engine starts nothing for it.
//!
//! One open question per group: a step that restates another's question takes
//! it over, and the step it was is left unanswered.

use super::Flows;
use super::engine::Moved;
use anyhow::{Context, Result, anyhow};
use rl_flow::advance::Happened;
use rl_flow::def::{Ask, Does, FlowDef};
use rl_flow::template::{self, Lookup};
use rl_ledger::{decisions, firings};
use rl_proto::action::Action;
use rl_proto::decision::{Decision, DecisionOption};
use rl_proto::flow::{StepState, StepStatus};
use rl_proto::methods::DecisionSpec;
use rl_proto::types::{Event, event_kind};
use rusqlite::Connection;
use tracing::{info, warn};

impl Flows {
    /// Raises step `step`'s question and records it as the step's. When it restates a
    /// question another step owned, that step, as `(firing, step)`, is returned: it is
    /// no longer asked.
    pub(super) async fn ask(&self, m: &Moved, step: &str, mut spec: DecisionSpec) -> Result<Option<(String, String)>> {
        let at = m.def.steps.iter().find(|s| s.name == step).context("no such step")?;
        spec.cwd = Some(self.cwd(&m.def, at, &m.firing, m.event.as_ref()));
        let d = self
            .decisions
            .raise_for_step(spec, &m.def.name, &m.firing.id, step)
            .await?;
        let (firing, step, id) = (m.firing.id.clone(), step.to_string(), d.id.clone());
        let was = self
            .ledger
            .call(move |c| {
                let was = firings::by_decision(c, &id)?;
                firings::set_step_decision(c, &firing, &step, &id)?;
                Ok(was.filter(|w| *w != (firing, step)))
            })
            .await?;
        info!(
            flow = m.def.name,
            firing = m.firing.id,
            step = at.name,
            decision = d.id,
            "step asked"
        );
        Ok(was)
    }

    /// A step's question answered, dismissed or expired: its firing moves on.
    pub(super) async fn on_answer(&self, e: &Event) -> Result<()> {
        let answered = match e.kind.as_str() {
            event_kind::DECISION_TAKEN => true,
            event_kind::DECISION_EXPIRED | event_kind::DECISION_DISMISSED => false,
            _ => return Ok(()),
        };
        let Some(id) = e.data.as_ref().and_then(|d| d["decision"].as_str()).map(String::from) else {
            return Ok(());
        };
        let _going = self.going.lock().await;
        let found = self
            .ledger
            .call(move |c| {
                let Some((firing, step)) = firings::by_decision(c, &id)? else {
                    return Ok(None);
                };
                let h = if !answered {
                    Happened::Unanswered { step }
                } else if runs(c, &firing, &step, &id)? {
                    Happened::Answered { step }
                } else {
                    Happened::AnsweredNothing { step }
                };
                Ok(Some((firing, h)))
            })
            .await?;
        match found {
            Some((firing, h)) => self.step(&firing, h).await,
            None => Ok(()),
        }
    }

    /// In a firing with a stopped step, its open questions are dismissed, so no option
    /// starts inside it — a step's stopped while it waited too; the steps still waiting are
    /// returned, to be left unanswered.
    pub(super) async fn dismiss_if_stopped(&self, m: &Moved) -> Vec<String> {
        let steps = &m.firing.steps;
        if !steps.iter().any(|s| s.status == StepStatus::Stopped) {
            return Vec::new();
        }
        let mut left = Vec::new();
        for s in steps
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Waiting | StepStatus::Stopped))
        {
            if let Some(d) = s.decision.clone() {
                let (firing, step, i) = (m.firing.id.clone(), s.name.clone(), d.clone());
                let open = self
                    .ledger
                    .call(move |c| {
                        let ours = firings::by_decision(c, &i)? == Some((firing, step));
                        Ok(ours && decisions::get(c, &i)?.is_some_and(|d| d.status.is_open()))
                    })
                    .await;
                if matches!(open, Ok(true))
                    && let Err(e) = self.decisions.dismiss(&d, Some("its firing stopped".into())).await
                {
                    warn!(firing = m.firing.id, step = s.name, decision = d, error = %format!("{e:#}"), "dismissing a stopped firing's question");
                }
            }
            if s.status == StepStatus::Waiting {
                left.push(s.name.clone());
            }
        }
        left
    }
}

/// An ask step's question, its text and group filled from what came before. Its
/// options carry their actions as written; the engine runs the one chosen.
pub(super) fn question(ask: &Ask, facts: &dyn Lookup) -> DecisionSpec {
    let options = ask
        .options
        .iter()
        .map(|(key, a)| DecisionOption {
            key: key.clone(),
            label: key.clone(),
            default: ask.default.as_ref() == Some(key),
            run: a.run.clone(),
            cwd: a.cwd.clone(),
            env: a.env.clone(),
            tags: a.tags.clone(),
            ref_: a.ref_.clone(),
            title: a.title.clone(),
            no_tty: a.no_tty,
        })
        .collect();
    DecisionSpec {
        title: template::fill(&ask.text, facts),
        summary: None,
        options,
        multi: ask.multi,
        group: ask
            .group
            .as_ref()
            .map(|g| template::fill(g, facts))
            .filter(|g| !g.trim().is_empty()),
        run: None,
        cwd: None,
        expires: ask.expires.clone(),
    }
}

/// The chosen option's action for an ask step, with what was chosen and said in its env.
pub(super) fn option_of(c: &Connection, def: &FlowDef, steps: &[StepState], step: &str) -> Result<Option<Action>> {
    let Some(d) = steps
        .iter()
        .find(|s| s.name == step)
        .and_then(|s| s.decision.as_deref())
    else {
        return Ok(None);
    };
    let Some(d) = decisions::get(c, d)? else {
        return Ok(None);
    };
    let Some((key, a)) = chosen(def, step, &d) else {
        return Ok(None);
    };
    let mut a = a.clone();
    a.env.insert("RELAY_DECISION".into(), d.id.clone());
    a.env.insert("RELAY_CHOICE".into(), key.to_string());
    a.env.insert("RELAY_NOTE".into(), d.note.clone().unwrap_or_default());
    Ok(Some(a))
}

/// Whether the answer to `decision` runs something for `firing`'s step.
fn runs(c: &Connection, firing: &str, step: &str, decision: &str) -> Result<bool> {
    let def = firings::def_json(c, firing)?.ok_or_else(|| anyhow!("no firing {firing}"))?;
    let def: FlowDef = serde_json::from_str(&def).context("the firing's definition")?;
    let Some(d) = decisions::get(c, decision)? else {
        return Ok(false);
    };
    Ok(chosen(&def, step, &d).is_some())
}

/// The option chosen that runs something: one at most, since a `multi` ask with more than one
/// that runs is refused at load.
fn chosen<'a>(def: &'a FlowDef, step: &str, d: &Decision) -> Option<(&'a str, &'a Action)> {
    let Some(Does::Ask(ask)) = def.steps.iter().find(|s| s.name == step).map(|s| &s.does) else {
        return None;
    };
    d.chosen.iter().find_map(|k| {
        ask.options
            .iter()
            .find(|(o, a)| o == k && !a.is_empty())
            .map(|(o, a)| (o.as_str(), a))
    })
}
