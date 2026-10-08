//! Firings across a restart, and a lost step picked back up.
//!
//! At start, before anything can fire, every open firing is carried on from what happened
//! while Relay was away. A lost step's run, resumed, starts as its step would, and is the
//! step's run from then on.

use super::Flows;
use super::engine::{CHANGED, Ready, def_of, laid_out};
use anyhow::{Result, anyhow, bail};
use rl_flow::advance::{Do, Happened};
use rl_flow::def::Does;
use rl_ledger::{decisions, firings, runs};
use rl_proto::flow::{Firing, StepState, StepStatus};
use rl_proto::methods::RunSpec;
use rl_proto::types::{Cause, Run, RunState};
use rusqlite::Connection;
use tracing::warn;

/// What an open firing is owed at a restart.
enum Owed {
    /// To move on from what happened.
    Happened(Happened),
    /// A step saved running whose run never started: said, and moved on as if it failed.
    NeverStarted(String),
    /// An ask step's question, never raised.
    Unasked(String),
}

impl Flows {
    /// Carries every open firing on from what happened while Relay was not running: one
    /// written but never started starts; a step whose run ended moves on as that end says,
    /// one whose run was lost waits to be resumed, and one whose run never started moves on
    /// as if it failed; a question never raised is raised. The events loop seeing the same
    /// ends afterwards changes nothing.
    pub async fn reconcile(&self) -> Result<()> {
        let _going = self.going.lock().await;
        let owed = self
            .ledger
            .call(|c| {
                let mut owed = Vec::new();
                for f in firings::open(c)? {
                    owed.extend(owed_to(c, &f)?.into_iter().map(|o| (f.id.clone(), o)));
                }
                Ok(owed)
            })
            .await?;
        for (firing, o) in owed {
            let done = match o {
                Owed::Happened(h) => self.step(&firing, h).await,
                Owed::NeverStarted(step) => self.never_started(&firing, step).await,
                Owed::Unasked(step) => self.ask_again(&firing, &step).await,
            };
            if let Err(e) = done {
                warn!(firing, error = %format!("{e:#}"), "carrying a firing on after a restart");
            }
        }
        Ok(())
    }

    /// Says a step's run never started, and moves its firing on as if it failed. The caller
    /// holds `going`.
    async fn never_started(&self, firing: &str, step: String) -> Result<()> {
        let id = firing.to_string();
        let Some(def) = self.ledger.call(move |c| def_of(c, &id)).await? else {
            return Ok(());
        };
        let why = anyhow!("Relay stopped before it started");
        self.not_started(&def, firing, &step, &why).await;
        self.step(firing, Happened::Ended { step, ok: false }).await
    }

    /// Raises the question of an ask step left waiting with none, as moving on to it would
    /// have; one already raised for it, and still open, is linked instead. The caller holds
    /// `going`.
    async fn ask_again(&self, firing: &str, step: &str) -> Result<()> {
        let (id, step, home) = (firing.to_string(), step.to_string(), self.home.clone());
        let m = self
            .ledger
            .call(move |c| {
                let Some(f) = firings::get(c, &id)? else {
                    return Ok(None);
                };
                // What was carried on before may have moved it.
                let unasked = |s: &StepState| s.name == step && s.status == StepStatus::Waiting && s.decision.is_none();
                let Some(def) = def_of(c, &id)?.filter(|_| f.steps.iter().any(unasked)) else {
                    return Ok(None);
                };
                if let Some(d) = decisions::open_by_step(c, &format!("{id}/{step}"))? {
                    firings::set_step_decision(c, &id, &step, &d)?;
                    return Ok(None);
                }
                laid_out(c, &home, &id, def, vec![Do::Ask(step)]).map(Some)
            })
            .await?;
        let Some(m) = m else { return Ok(()) };
        let next = self.carry_out(&m).await;
        self.follow(next.into()).await
    }

    /// What step run `run` was started with, made again for picking it up; `None` when it is
    /// no firing's step. Refused as a step's start is: on a folder changed since its yes, or a yes to
    /// another version than the firing's.
    pub(crate) async fn spec_again(&self, run: &Run) -> Result<Option<RunSpec>> {
        let (id, home) = (run.id.clone(), self.home.clone());
        let found = self
            .ledger
            .call(move |c| {
                let Some((firing, step)) = firings::by_run(c, &id)? else {
                    return Ok(None);
                };
                let Some(def) = def_of(c, &firing)? else {
                    return Ok(None);
                };
                let todo = match def.steps.iter().find(|s| s.name == step).map(|s| &s.does) {
                    Some(Does::Run(_)) => Do::Start(step.clone()),
                    Some(Does::Ask(_)) => Do::RunOption(step.clone()),
                    None => return Ok(None),
                };
                laid_out(c, &home, &firing, def, vec![todo]).map(|m| Some((m, step)))
            })
            .await?;
        let Some((m, step)) = found else { return Ok(None) };
        if let Some(why) = self.why_not_run(&m.def.name, m.began.as_deref()).await {
            if why == CHANGED {
                self.changed_since_yes(&m.def.name).await;
            }
            bail!(why);
        }
        match m.todo.first() {
            Some((_, Ready::Run(Ok(a)))) => self.spec(&m, &step, a).map(Some),
            Some((_, Ready::Run(Err(e)))) => Err(anyhow!("{e:#}")),
            _ => Ok(None),
        }
    }

    /// Run `old` picked back up as `new`: when `old` was a lost step's, `new` is that step's
    /// run now, and its firing goes on. The caller holds `going`, taken before `new` started.
    pub(crate) async fn resumed(&self, old: &str, new: &str) -> Result<()> {
        let (old, new) = (old.to_string(), new.to_string());
        let lost = self
            .ledger
            .call(move |c| {
                let Some((firing, step)) = firings::by_run(c, &old)? else {
                    return Ok(None);
                };
                if firings::steps(c, &firing)?.get(&step) != Some(&StepStatus::Lost) {
                    return Ok(None);
                }
                firings::set_step_run(c, &firing, &step, &new)?;
                Ok(Some((firing, step)))
            })
            .await?;
        match lost {
            Some((firing, step)) => self.step(&firing, Happened::Resumed { step }).await,
            None => Ok(()),
        }
    }
}

/// What open firing `f` is owed. A step saved running with no run takes the run started for
/// it, if one was, before its run was recorded.
fn owed_to(c: &Connection, f: &Firing) -> Result<Vec<Owed>> {
    if f.steps.iter().all(|s| s.status == StepStatus::Pending) {
        return Ok(vec![Owed::Happened(Happened::Started)]);
    }
    let mut owed = Vec::new();
    for s in &f.steps {
        match s.status {
            StepStatus::Running => {
                let run = match &s.run {
                    Some(r) => runs::get(c, r)?,
                    None => {
                        let cause = Cause::Flow {
                            flow: f.flow.clone(),
                            step: s.name.clone(),
                        };
                        let found = runs::newest_in_chain(c, &f.id, &cause.to_string())?;
                        if let Some(r) = &found {
                            firings::set_step_run(c, &f.id, &s.name, &r.id)?;
                        }
                        found
                    }
                };
                match run {
                    Some(r) => owed.extend(ended_as(c, &r, &s.name)?.map(Owed::Happened)),
                    None => owed.push(Owed::NeverStarted(s.name.clone())),
                }
            }
            StepStatus::Waiting if s.decision.is_none() => owed.push(Owed::Unasked(s.name.clone())),
            _ => {}
        }
    }
    Ok(owed)
}

/// What a step's run having ended says: `None` while it goes on. A run marked lost is lost,
/// whatever it ended as.
fn ended_as(c: &Connection, r: &Run, step: &str) -> Result<Option<Happened>> {
    if !r.state.is_terminal() {
        return Ok(None);
    }
    let step = step.to_string();
    Ok(Some(if runs::was_lost(c, &r.id)? {
        Happened::Lost { step }
    } else {
        match r.state {
            RunState::Finished => Happened::Ended { step, ok: true },
            RunState::Stopped => Happened::Stopped { step },
            _ => Happened::Ended { step, ok: false },
        }
    }))
}
