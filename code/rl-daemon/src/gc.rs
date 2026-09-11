//! Retention: the one thing Relay deletes, and only when told to. The
//! journal — a run's terminal bytes, its log and sidecar files — goes once
//! the run has ended longer ago than `retention: 30d` says; the run and its
//! events stay, and an event says the journal went. `rl gc` does it now,
//! `rld` does it daily.

use crate::config::Loaded;
use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use relay_core::Home;
use relay_core::time::parse_duration;
use relay_ledger::events::NewEvent;
use relay_ledger::{Ledger, events, meta, runs, session_io};
use relay_proto::methods::GcReport;
use relay_proto::types::event_kind;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// `meta` key: when the daily sweep last ran.
const LAST_AT: &str = "gc.last_at";
/// How often `rld` looks whether a sweep is due, and how often one is.
const LOOK: Duration = Duration::from_secs(60 * 60);
const DAILY_MS: i64 = 24 * 60 * 60 * 1000;

/// Drops the journal of every run that ended before now minus `older_than`
/// (`retention.journal` from the config when not given). `dry_run` only
/// counts.
pub async fn run(
    home: &Home,
    ledger: &Ledger,
    retention: Option<&str>,
    older_than: Option<&str>,
    dry_run: bool,
) -> Result<GcReport> {
    let (spec, from) = match (older_than, retention) {
        (Some(s), _) => (s.to_string(), "asked"),
        (None, Some(s)) => (s.to_string(), "config"),
        (None, None) => bail!("no retention: `retention: 30d` in config.yaml, or --older-than"),
    };
    let d = parse_duration(&spec).ok_or_else(|| anyhow::anyhow!("`{spec}` is not a duration (30d, 12h)"))?;
    let now = Timestamp::now();
    let before = now - d;
    let dropped = ledger
        .call(move |c| {
            let rows = session_io::drop_ended_before(c, before, dry_run)?;
            if !dry_run {
                for (id, bytes) in &rows {
                    if let Some(r) = runs::get(c, id)? {
                        let mut e = NewEvent::for_run(
                            &r,
                            now,
                            event_kind::RUN_JOURNAL_DROPPED,
                            format!("journal dropped · {}", fmt_bytes(*bytes)),
                        );
                        e.data = Some(serde_json::json!({ "bytes": bytes }));
                        events::append(c, &e)?;
                    }
                }
            }
            Ok(rows)
        })
        .await?;
    let mut files = 0usize;
    let mut bytes: u64 = dropped.iter().map(|(_, b)| b).sum();
    for (id, _) in &dropped {
        for f in [
            home.session_log(id),
            home.session_exit_file(id),
            home.session_env_file(id),
        ] {
            let Ok(m) = std::fs::metadata(&f) else { continue };
            files += 1;
            bytes += m.len();
            if !dry_run {
                std::fs::remove_file(&f).with_context(|| format!("removing {}", f.display()))?;
            }
        }
    }
    Ok(GcReport {
        older_than: spec,
        from: from.into(),
        dry_run,
        runs: dropped.len(),
        bytes,
        files,
    })
}

/// `4.1 MB`, `312 kB`, `88 B`.
pub fn fmt_bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} kB", n >> 10),
        n => format!("{n} B"),
    }
}

/// Once a day, when the config says how long the journal is kept.
pub async fn daily(home: Home, ledger: Ledger, config: Arc<RwLock<Loaded>>, shutdown: CancellationToken) {
    loop {
        let retention = config
            .read()
            .unwrap()
            .config
            .retention
            .as_ref()
            .and_then(|r| r.journal.clone());
        if let Some(keep) = retention {
            let due = match ledger.call(|c| meta::get_i64(c, LAST_AT)).await {
                Ok(Some(last)) => Timestamp::now().as_millisecond() - last >= DAILY_MS,
                Ok(None) => true,
                Err(e) => {
                    warn!(error = %format!("{e:#}"), "gc: last run unknown");
                    false
                }
            };
            if due {
                match run(&home, &ledger, Some(&keep), None, false).await {
                    Ok(r) if r.runs > 0 => {
                        info!(runs = r.runs, bytes = r.bytes, files = r.files, keep = %keep, "journal dropped")
                    }
                    Ok(_) => {}
                    Err(e) => warn!(error = %format!("{e:#}"), "gc"),
                }
                let now = Timestamp::now().as_millisecond();
                let _ = ledger.call(move |c| meta::set_i64(c, LAST_AT, now)).await;
            }
        }
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = tokio::time::sleep(LOOK) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_read_as_words() {
        assert_eq!(fmt_bytes(88), "88 B");
        assert_eq!(fmt_bytes(320_000), "312 kB");
        assert_eq!(fmt_bytes(4_300_000), "4.1 MB");
        assert_eq!(fmt_bytes(3 << 30), "3.0 GB");
    }
}
