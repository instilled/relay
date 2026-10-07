//! Flows: loaded from `~/.relay/flows`, fired, and carried out.
//!
//! A flow is a folder: `flow.md` and the flow's own scripts. It runs only
//! once approved, by the hash of its folder less `runs/` ([`store`]).

mod ask;
pub mod engine;
mod restart;
pub(crate) mod rpc;
mod store;

use crate::decisions::Decisions;
use crate::supervisor::Supervisor;
use anyhow::Result;
use rl_core::Home;
use rl_detect::Registry;
use rl_ledger::{Ledger, events, meta};
use rl_proto::flow::{Flow, NotLoaded};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Where the loop has read the events to, in `meta`.
const CURSOR: &str = "flows.cursor";
/// Events read at a time.
const BATCH: u32 = 200;
/// How long the loop naps when nothing wakes it.
const TICK: Duration = Duration::from_secs(1);

/// Flows: loaded from ~/.relay/flows, fired, and carried out.
pub struct Flows {
    pub(crate) home: Home,
    pub(crate) ledger: Ledger,
    pub(crate) supervisor: Arc<Supervisor>,
    /// What a firing's questions are raised with.
    pub(crate) decisions: Arc<Decisions>,
    pub(crate) registry: Arc<Registry>,
    /// Held while a firing is moved on, so a run's end is never read before
    /// the run is recorded as its step's.
    pub(crate) going: tokio::sync::Mutex<()>,
    /// Rung when what is loaded changes, so the loop looks again.
    pub(crate) wake: Notify,
    pub(crate) shutdown: CancellationToken,
    /// What did not load at the last reload.
    pub(crate) broken: Mutex<Vec<NotLoaded>>,
}

impl Flows {
    pub fn new(
        home: Home,
        ledger: Ledger,
        supervisor: Arc<Supervisor>,
        decisions: Arc<Decisions>,
        registry: Arc<Registry>,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            home,
            ledger,
            supervisor,
            decisions,
            registry,
            going: tokio::sync::Mutex::new(()),
            wake: Notify::new(),
            shutdown,
            broken: Mutex::new(Vec::new()),
        })
    }

    /// What did not load at the last reload, each with its file and line.
    pub fn not_loaded(&self) -> Vec<NotLoaded> {
        self.broken.lock().unwrap().clone()
    }

    /// The loop that fires flows and carries their firings out: each event,
    /// once, to the firings it is about.
    pub async fn serve(self: Arc<Self>) {
        loop {
            if let Err(e) = self.drain().await {
                warn!(error = %format!("{e:#}"), "flows");
            }
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(TICK) => {}
            }
        }
    }

    /// Reads what is new and hands it on, until nothing is.
    async fn drain(&self) -> Result<()> {
        loop {
            let after = self.cursor().await?;
            let batch = self.ledger.call(move |c| events::after(c, after, BATCH)).await?;
            let Some(last) = batch.last().map(|e| e.id) else {
                return Ok(());
            };
            for e in &batch {
                if let Err(err) = self.on_event(e).await {
                    warn!(event = e.id, kind = e.kind, error = %format!("{err:#}"), "flows: event");
                }
            }
            self.ledger.call(move |c| meta::set_i64(c, CURSOR, last)).await?;
        }
    }

    /// Where the loop has read the events to. On a first start, now: not the
    /// whole history.
    pub(crate) async fn cursor(&self) -> Result<i64> {
        if let Some(n) = self.ledger.call(|c| meta::get_i64(c, CURSOR)).await? {
            return Ok(n);
        }
        self.ledger
            .call(|c| {
                let last = events::last_id(c)?;
                meta::set_i64(c, CURSOR, last)?;
                Ok(last)
            })
            .await
    }

    /// A flow as a client is shown it: with where its folder is.
    pub(crate) fn shown(&self, mut f: Flow) -> Flow {
        f.source = self.home.flow_dir(&f.name);
        f
    }
}
