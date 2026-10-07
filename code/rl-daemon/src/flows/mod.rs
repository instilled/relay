//! Flows: loaded from `~/.relay/flows`, fired, and carried out.
//!
//! A flow is a folder: `flow.md` and the flow's own scripts. It runs only
//! once approved, by the hash of its folder less `runs/` ([`store`]).

pub(crate) mod rpc;
mod store;

use crate::decisions::Decisions;
use crate::supervisor::Supervisor;
use rl_core::Home;
use rl_detect::Registry;
use rl_ledger::Ledger;
use rl_proto::flow::{Flow, NotLoaded};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// Flows: loaded from ~/.relay/flows, fired, and carried out.
pub struct Flows {
    pub(crate) home: Home,
    pub(crate) ledger: Ledger,
    // What a firing's steps are carried out with, from when flows fire.
    #[allow(dead_code)]
    pub(crate) supervisor: Arc<Supervisor>,
    #[allow(dead_code)]
    pub(crate) decisions: Arc<Decisions>,
    #[allow(dead_code)]
    pub(crate) registry: Arc<Registry>,
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
            wake: Notify::new(),
            shutdown,
            broken: Mutex::new(Vec::new()),
        })
    }

    /// What did not load at the last reload, each with its file and line.
    pub fn not_loaded(&self) -> Vec<NotLoaded> {
        self.broken.lock().unwrap().clone()
    }

    /// The loop that fires flows and carries their firings out.
    pub async fn serve(self: Arc<Self>) {
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = self.wake.notified() => {}
            }
        }
    }

    /// A flow as a client is shown it: with where its folder is.
    pub(crate) fn shown(&self, mut f: Flow) -> Flow {
        f.source = self.home.flow_dir(&f.name);
        f
    }
}
