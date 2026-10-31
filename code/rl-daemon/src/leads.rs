//! The leads service: sweeps expired user leads, and is the seat the push hook
//! (step 4) will sit in. Correctness does not depend on it: the report read
//! path filters expiry itself; this is housekeeping.

use anyhow::Result;
use jiff::Timestamp;
use rl_ledger::Ledger;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// How often the sweeper wakes on its own when nothing nudges it.
const TICK: Duration = Duration::from_secs(60);

pub struct Leads {
    ledger: Ledger,
    /// Rung when a lead is posted, so the loop looks again.
    pub wake: Notify,
    shutdown: CancellationToken,
}

impl Leads {
    pub fn new(ledger: Ledger, shutdown: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            ledger,
            wake: Notify::new(),
            shutdown,
        })
    }

    /// Delete expired leads once.
    pub async fn sweep(&self) -> Result<()> {
        let now = Timestamp::now();
        self.ledger
            .call(move |c| rl_ledger::leads::sweep_expired(c, now))
            .await?;
        Ok(())
    }

    pub async fn serve(self: Arc<Self>) {
        loop {
            if let Err(e) = self.sweep().await {
                tracing::warn!(error = %format!("{e:#}"), "leads: sweep");
            }
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(TICK) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use rl_ledger::Ledger;
    use rl_proto::lead::{Band, Lead, LeadStatus};

    #[tokio::test]
    async fn sweep_once_drops_expired() {
        let ledger = Ledger::open_in_memory().unwrap();
        let now = Timestamp::now();
        let l = Lead {
            source: "cron".into(),
            key: "n".into(),
            band: Band::Happened,
            title: "t".into(),
            summary: None,
            scope: None,
            action: None,
            status: LeadStatus::Open,
            at: now,
            expires_at: Some(now - std::time::Duration::from_secs(1)),
            facts: None,
            rest: None,
        };
        ledger.call(move |c| rl_ledger::leads::post(c, &l)).await.unwrap();
        let token = tokio_util::sync::CancellationToken::new();
        let svc = Leads::new(ledger.clone(), token.clone());
        svc.sweep().await.unwrap();
        let left = ledger.call(move |c| rl_ledger::leads::list(c, now)).await.unwrap();
        assert!(left.is_empty());
    }
}
