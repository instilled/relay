//! `rl flow state`: what a step leaves for the steps after it, and reads of what theirs left,
//! through `rld`. The step is the run asking (`$RELAY_RUN`), while it runs — any other run, or
//! one over, is "not in a flow step" — so an agent allowed `Bash(rl flow state:*)` reaches its own
//! firing's state and nothing else: it cannot approve, run or edit a flow.

use super::Flows;
use super::engine::def_of;
use super::facts::Facts;
use super::work::{Work, write_atomic};
use anyhow::{Context, Result};
use rl_core::Home;
use rl_flow::def::FlowDef;
use rl_flow::state;
use rl_flow::template::{Lookup, Ref};
use rl_ledger::{firings, runs};
use rusqlite::Connection;
use std::fmt;
use std::io::Read;
use std::path::Path;

/// What every ask from outside a running step is told.
pub(crate) const NOT_IN_A_STEP: &str = "not in a flow step";

/// What `rl flow state` was refused, said to the step as it is.
#[derive(Debug)]
pub(crate) struct Refusal(pub(crate) String);

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

/// The step asking: the definition its firing runs on, the firing, the step's name.
struct Caller {
    def: FlowDef,
    firing: String,
    step: String,
}

impl Caller {
    fn steps(&self) -> Vec<&str> {
        self.def.steps.iter().map(|s| s.name.as_str()).collect()
    }

    fn work(&self, home: &Home) -> Work {
        Work::of(home, &self.def.name, &self.firing, &self.step)
    }
}

/// The step `run` is, while it runs; `None` for any other run, or one over.
fn caller(c: &Connection, run: &str) -> Result<Option<Caller>> {
    let Some((firing, step)) = firings::by_run(c, run)? else {
        return Ok(None);
    };
    if runs::get(c, run)?.is_none_or(|r| r.state.is_terminal()) {
        return Ok(None);
    }
    Ok(def_of(c, &firing)?.map(|def| Caller { def, firing, step }))
}

impl Flows {
    async fn caller(&self, run: &str) -> Result<Caller> {
        let run = run.to_string();
        match self.ledger.call(move |c| caller(c, &run)).await? {
            Some(who) => Ok(who),
            None => Err(Refusal(NOT_IN_A_STEP.to_string()).into()),
        }
    }

    /// `rl flow state set k=v…`: values of the asking step, each over any it set before.
    pub(crate) async fn state_set(&self, run: &str, values: Vec<(String, String)>) -> Result<()> {
        let who = self.caller(run).await?;
        for (k, v) in &values {
            state::value(k, v).map_err(Refusal)?;
        }
        self.ledger
            .call(move |c| firings::set_values(c, &who.firing, &who.step, &values))
            .await
    }

    /// `rl flow state get probe.signature`: what a step of this firing left — a value, its out,
    /// an ask's choice or note; a bare key is the asking step's own. `None`: not set.
    pub(crate) async fn state_get(&self, run: &str, name: &str) -> Result<Option<String>> {
        let who = self.caller(run).await?;
        let r = match name.split_once('.') {
            Some((head, rest)) => Ref {
                head: head.to_string(),
                rest: rest.to_string(),
            },
            None => Ref {
                head: who.step.clone(),
                rest: name.to_string(),
            },
        };
        if !who.steps().contains(&r.head.as_str()) {
            return Err(Refusal(format!("`{}` is no step of `{}`", r.head, who.def.name)).into());
        }
        self.ledger
            .call(move |c| {
                let f = firings::get(c, &who.firing)?.context("the firing")?;
                Ok(Facts::new(c, &f, None).get(&r))
            })
            .await
    }

    /// `rl flow state write <name>`: a file in the firing's folder, for the steps after it,
    /// written whole or not at all.
    pub(crate) async fn state_write(&self, run: &str, name: &str, bytes: Vec<u8>) -> Result<()> {
        let who = self.caller(run).await?;
        state::file(name, bytes.len(), &who.steps()).map_err(Refusal)?;
        let path = who.work(&self.home).dir.join(name);
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(path.parent().context("a firing's folder")?)?;
            write_atomic(&path, &bytes)
        })
        .await?
    }

    /// `rl flow state read <name>`: a file in the firing's folder, or what a step printed
    /// (`probe.out`).
    pub(crate) async fn state_read(&self, run: &str, name: &str) -> Result<Vec<u8>> {
        let who = self.caller(run).await?;
        if let Some(step) = state::out_of(name, &who.steps()).map(String::from) {
            let firing = who.firing.clone();
            let out = self
                .ledger
                .call(move |c| {
                    let f = firings::get(c, &firing)?.context("the firing")?;
                    Ok(Facts::new(c, &f, None).out(&step))
                })
                .await?;
            return Ok(out.unwrap_or_default().into_bytes());
        }
        state::file_name(name, &who.steps()).map_err(Refusal)?;
        let path = who.work(&self.home).dir.join(name);
        let name = name.to_string();
        tokio::task::spawn_blocking(move || read_capped(&path, &name)).await?
    }
}

/// A file of the firing's, 10 MB at most: one bigger is the step's to read from `$RELAY_WORK`.
fn read_capped(path: &Path, name: &str) -> Result<Vec<u8>> {
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Refusal(format!("no `{name}` in this firing")).into());
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut bytes = Vec::new();
    f.take(state::FILE_MAX as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > state::FILE_MAX {
        return Err(Refusal(format!("`{name}` is over 10 MB: read it from $RELAY_WORK")).into());
    }
    Ok(bytes)
}
