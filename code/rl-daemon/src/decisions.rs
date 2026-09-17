//! Decisions: the human node.
//!
//! A run raises a question with `rl ask` and carries on; the question waits in
//! the ledger. Choosing an option starts whatever that option carries, as an
//! ordinary run with `cause: decision:<id>` and the decision's `flow`, so a
//! schedule → question → follow-up is one chain. A decision with an `expires`
//! either falls back to its default option or expires unanswered; the sweep
//! that does it is the only clock involved.

use crate::supervisor::Supervisor;
use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use rl_core::time::parse_duration;
use rl_detect::Registry;
use rl_ledger::{Ledger, decisions, events, events::NewEvent, runs};
use rl_proto::decision::{Decision, DecisionStatus};
use rl_proto::methods::{DecisionFilter, DecisionSpec};
use rl_proto::types::{Cause, event_kind};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// How long the sweep sleeps when nothing has a deadline at all.
const IDLE: Duration = Duration::from_secs(300);

pub struct Decisions {
    ledger: Ledger,
    supervisor: Arc<Supervisor>,
    registry: Arc<Registry>,
    /// Rung when a question with a deadline appears, so the sweep re-naps.
    wake: Notify,
    shutdown: CancellationToken,
}

impl Decisions {
    pub fn new(
        ledger: Ledger,
        supervisor: Arc<Supervisor>,
        registry: Arc<Registry>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            ledger,
            supervisor,
            registry,
            wake: Notify::new(),
            shutdown,
        }
    }

    /// Raises a question. With a `group`, an open question with that key is
    /// restated rather than duplicated, keeping its id.
    pub async fn raise(&self, spec: DecisionSpec) -> Result<Decision> {
        self.raise_by(spec, None).await
    }

    /// The same, remembering the reaction behind it so that rule never
    /// answers, or reacts to, its own question.
    pub async fn raise_by(&self, spec: DecisionSpec, by_reaction: Option<&str>) -> Result<Decision> {
        if spec.title.trim().is_empty() {
            bail!("a decision needs a title");
        }
        let mut keys = std::collections::BTreeSet::new();
        for o in &spec.options {
            if o.key.trim().is_empty() {
                bail!("every option needs a key");
            }
            if !keys.insert(o.key.clone()) {
                bail!("option `{}` is in there twice", o.key);
            }
        }
        if spec.options.iter().filter(|o| o.default).count() > 1 {
            bail!("only one option can be the default");
        }
        let now = Timestamp::now();
        let expires_at = match &spec.expires {
            Some(e) => {
                let d = parse_duration(e).ok_or_else(|| anyhow!("expires: `{e}` is not a duration (4h, 2d)"))?;
                Some(now.checked_add(d).context("expires: too far away")?)
            }
            None => None,
        };
        if expires_at.is_some() && spec.options.iter().all(|o| !o.default) && !spec.options.is_empty() {
            debug!("decision expires with no default; it will simply expire");
        }

        // Where it belongs: the asking run's chain and workspace, or the cwd's.
        let (run, flow, workspace) = match &spec.run {
            Some(id) => {
                let id = id.clone();
                let r = self
                    .ledger
                    .call(move |c| runs::get(c, &id))
                    .await?
                    .ok_or_else(|| anyhow!("no run {}", spec.run.as_deref().unwrap_or_default()))?;
                (Some(r.id.clone()), r.flow.clone(), r.workspace.clone())
            }
            None => {
                let cwd = spec.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
                let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
                (None, String::new(), cwd)
            }
        };
        let id = rl_core::id::new_id().context("generating decision id")?;
        let mut d = Decision {
            flow: if flow.is_empty() { id.clone() } else { flow },
            id,
            workspace,
            run,
            group: spec.group.clone(),
            title: spec.title.clone(),
            summary: spec.summary.clone(),
            options: spec.options.clone(),
            multi: spec.multi,
            status: DecisionStatus::Open,
            created_at: now,
            updated_at: now,
            expires_at,
            resolved_at: None,
            chosen: Vec::new(),
            outcome_runs: Vec::new(),
            note: None,
        };

        let existing = match &spec.group {
            Some(g) => {
                let g = g.clone();
                self.ledger.call(move |c| decisions::open_in_group(c, &g)).await?
            }
            None => None,
        };
        let restated = existing.is_some();
        let stored = match existing {
            Some(open) => {
                // Same question, said again: keep the id and the flow it began.
                d.flow = open.flow.clone();
                let (id, d2) = (open.id.clone(), d.clone());
                self.ledger.call(move |c| decisions::restate(c, &id, &d2, now)).await?
            }
            None => {
                let d2 = d.clone();
                self.ledger.call(move |c| decisions::insert(c, &d2)).await?
            }
        };
        if let Some(name) = by_reaction {
            let (id, name) = (stored.id.clone(), name.to_string());
            self.ledger
                .call(move |c| decisions::set_by_reaction(c, &id, &name))
                .await?;
        }
        self.append(
            &stored,
            event_kind::DECISION_RAISED,
            format!("asks: {}", stored.title),
            json!({ "decision": stored.id, "options": stored.options.iter().map(|o| &o.key).collect::<Vec<_>>(), "restated": restated }),
        )
        .await?;
        info!(decision = stored.id, restated, "decision raised");
        if stored.expires_at.is_some() {
            self.wake.notify_one();
        }
        Ok(stored)
    }

    pub async fn list(&self, f: DecisionFilter) -> Result<Vec<Decision>> {
        self.ledger.call(move |c| decisions::list(c, &f)).await
    }

    pub async fn get(&self, id: &str) -> Result<Option<Decision>> {
        let id = id.to_string();
        self.ledger.call(move |c| decisions::get(c, &id)).await
    }

    /// Chooses. Every chosen option that carries an action starts a run.
    pub async fn take(&self, id: &str, keys: &[String], note: Option<String>) -> Result<Decision> {
        let d = self.get(id).await?.ok_or_else(|| anyhow!("no decision {id}"))?;
        if !d.status.is_open() {
            bail!("decision {id} is already {}", d.status);
        }
        if keys.is_empty() {
            bail!("say which option");
        }
        if keys.len() > 1 && !d.multi {
            bail!("decision {id} takes one option, not {}", keys.len());
        }
        for k in keys {
            if d.option(k).is_none() {
                bail!("decision {id} has no option `{k}`; it has {}", d.keys());
            }
        }
        self.resolve(&d, DecisionStatus::Taken, keys, note).await
    }

    /// Answers "not this": nothing runs.
    pub async fn dismiss(&self, id: &str, note: Option<String>) -> Result<Decision> {
        let d = self.get(id).await?.ok_or_else(|| anyhow!("no decision {id}"))?;
        if !d.status.is_open() {
            bail!("decision {id} is already {}", d.status);
        }
        self.resolve(&d, DecisionStatus::Dismissed, &[], note).await
    }

    /// Marks the decision answered first, then starts what the answer says.
    /// That order is what stops a second `take` from starting the runs twice.
    async fn resolve(
        &self,
        d: &Decision,
        status: DecisionStatus,
        keys: &[String],
        note: Option<String>,
    ) -> Result<Decision> {
        let now = Timestamp::now();
        let (id, chosen, note2) = (d.id.clone(), keys.to_vec(), note.clone());
        let mut resolved = self
            .ledger
            .call(move |c| decisions::resolve(c, &id, status, &chosen, &[], note2.as_deref(), now))
            .await?;

        let mut started = Vec::new();
        for key in keys {
            let Some(opt) = d.option(key) else { continue };
            let action = opt.action();
            if action.is_empty() {
                continue;
            }
            let name = format!("{} · {}", d.title, opt.text());
            let base = d.workspace.clone();
            match crate::action::run_spec(
                &action,
                &name,
                &base,
                Some(&base),
                &self.registry,
                Cause::Decision(d.id.clone()),
                Some(d.flow.clone()),
            ) {
                Ok(mut spec) => {
                    // What was asked and answered reaches the option's run, so
                    // the note that came with the answer can steer it.
                    spec.env.push(("RELAY_DECISION".into(), d.id.clone()));
                    spec.env.push(("RELAY_CHOICE".into(), key.clone()));
                    spec.env.push(("RELAY_NOTE".into(), note.clone().unwrap_or_default()));
                    match self.supervisor.start(spec).await {
                        Ok(run) => started.push(run.id),
                        Err(e) => {
                            warn!(decision = d.id, option = key, error = %format!("{e:#}"), "option did not start")
                        }
                    }
                }
                Err(e) => warn!(decision = d.id, option = key, error = %format!("{e:#}"), "option is not runnable"),
            }
        }
        if !started.is_empty() {
            let (id, runs) = (d.id.clone(), started.clone());
            self.ledger
                .call(move |c| decisions::set_outcomes(c, &id, &runs))
                .await?;
            resolved.outcome_runs = started.clone();
        }

        let (kind, summary) = match status {
            DecisionStatus::Taken => (event_kind::DECISION_TAKEN, format!("chose {}", keys.join(", "))),
            DecisionStatus::Dismissed => (event_kind::DECISION_DISMISSED, "dismissed".to_string()),
            DecisionStatus::Expired => (event_kind::DECISION_EXPIRED, "expired unanswered".to_string()),
            DecisionStatus::Open => unreachable!("resolving to open"),
        };
        let summary = match &note {
            Some(n) if !n.is_empty() => format!("{summary}: {n}"),
            _ => summary,
        };
        self.append(
            &resolved,
            kind,
            summary,
            json!({ "decision": resolved.id, "chosen": keys, "runs": started }),
        )
        .await?;
        info!(
            decision = resolved.id,
            status = status.as_str(),
            runs = started.len(),
            "decision answered"
        );
        Ok(resolved)
    }

    /// Questions whose time is up: the default option is taken, or they expire.
    pub async fn sweep(&self, now: Timestamp) -> Result<usize> {
        let due = self.ledger.call(move |c| decisions::due(c, now)).await?;
        let mut n = 0;
        for d in due {
            let outcome = match d.default_option() {
                Some(opt) => {
                    let key = opt.key.clone();
                    self.resolve(
                        &d,
                        DecisionStatus::Taken,
                        &[key],
                        Some("expired; took the default".into()),
                    )
                    .await
                }
                None => self.resolve(&d, DecisionStatus::Expired, &[], None).await,
            };
            match outcome {
                Ok(_) => n += 1,
                Err(e) => warn!(decision = d.id, error = %format!("{e:#}"), "expiring failed"),
            }
        }
        Ok(n)
    }

    /// The sweep loop: asleep until the earliest deadline, or until a
    /// question with one is raised. Nothing to expire, nothing to do.
    pub async fn serve(self: Arc<Self>) {
        loop {
            let now = Timestamp::now();
            let nap = match self.ledger.call(|c| decisions::next_expiry(c)).await {
                Ok(Some(t)) if t > now => t.duration_since(now).unsigned_abs().min(IDLE),
                Ok(Some(_)) => Duration::ZERO,
                Ok(None) => IDLE,
                Err(e) => {
                    warn!(error = %format!("{e:#}"), "reading decision deadlines");
                    IDLE
                }
            };
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(nap) => {}
            }
            if let Err(e) = self.sweep(Timestamp::now()).await {
                warn!(error = %format!("{e:#}"), "decision sweep failed");
            }
        }
    }

    async fn append(&self, d: &Decision, kind: &str, summary: String, data: serde_json::Value) -> Result<()> {
        let e = NewEvent {
            at: Timestamp::now(),
            workspace: d.workspace.clone(),
            run: d.run.clone(),
            group_key: d.workspace.to_string_lossy().into_owned(),
            kind: kind.to_string(),
            summary,
            data: Some(data),
        };
        // An event about a run is grouped the way that run is.
        let e = match &d.run {
            Some(id) => {
                let id = id.clone();
                match self.ledger.call(move |c| runs::get(c, &id)).await? {
                    Some(r) => NewEvent {
                        group_key: r.group_key(),
                        ..e
                    },
                    None => e,
                }
            }
            None => e,
        };
        self.ledger.call(move |c| events::append(c, &e)).await.map(|_| ())
    }
}
