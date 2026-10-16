//! Retention: the one thing Relay deletes. The journal — a run's terminal
//! bytes, its log and sidecar files — goes once the run has ended longer ago
//! than `retention` says, `30d` unless the file says otherwise; the run and
//! its events stay, and an event says the journal went. A firing's folder,
//! `runs/<firing>/`, goes with its runs: when the firing ended as long ago.
//! `retention: never` keeps every byte. `rl prune` does it now, `rld` does it daily.

use crate::config::Loaded;
use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rl_core::Home;
use rl_core::time::parse_duration;
use rl_flow::def::valid_name;
use rl_ledger::events::NewEvent;
use rl_ledger::{Ledger, events, firings, meta, runs, session_io};
use rl_proto::methods::GcReport;
use rl_proto::types::event_kind;
use std::path::Path;
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
        (None, None) => (rl_proto::config::DEFAULT_RETENTION.to_string(), "default"),
    };
    if rl_proto::config::is_never(&spec) {
        bail!("retention is `never`: nothing goes. `rl prune --older-than 30d` to let some go anyway");
    }
    let d = parse_duration(&spec).ok_or_else(|| anyhow::anyhow!("`{spec}` is not a duration (30d, 12h)"))?;
    let now = Timestamp::now();
    let before = now - d;
    let dropped = ledger
        .call(move |c| {
            let rows = session_io::drop_ended_before(c, before, dry_run)?;
            if !dry_run {
                // Clear any journal dir a crash left behind for a run that is
                // wholly gone from the ledger.
                session_io::orphan_sweep(c)?;
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
    // The ledger rows for this whole batch are already gone (committed above),
    // so a run's id can never be re-selected by a later sweep: one file that
    // won't remove must not stop us from trying the rest of the batch, or its
    // siblings' sidecars are orphaned on disk forever too.
    let mut failed = 0usize;
    for (id, _) in &dropped {
        for f in [
            home.session_log(id),
            home.session_exit_file(id),
            home.session_env_file(id),
        ] {
            let Ok(m) = std::fs::metadata(&f) else { continue };
            files += 1;
            bytes += m.len();
            if !dry_run && let Err(e) = std::fs::remove_file(&f).with_context(|| format!("removing {}", f.display())) {
                warn!(error = %format!("{e:#}"), "gc: journal file not removed; orphaned on disk");
                failed += 1;
            }
        }
    }
    if failed > 0 {
        warn!(
            failed,
            "gc: some journal files could not be removed; see prior warnings"
        );
    }
    let (firings, firing_bytes) = firing_folders(home, ledger, before, dry_run).await?;
    bytes += firing_bytes;
    Ok(GcReport {
        older_than: spec,
        from: from.into(),
        dry_run,
        runs: dropped.len(),
        bytes,
        files,
        firings,
    })
}

/// The folders of firings that ended before `before` — `runs/<firing>/` in each flow's folder,
/// all their steps left — gone with their runs' journals, unless `dry_run`. Only a folder named
/// for a firing of that flow the ledger knows has ended goes: anything else under `runs/` — a
/// file or a folder the flow keeps there itself, a firing still open — stays. How many went, and
/// their bytes.
async fn firing_folders(home: &Home, ledger: &Ledger, before: Timestamp, dry_run: bool) -> Result<(usize, u64)> {
    let mut seen: Vec<(String, Vec<String>)> = Vec::new();
    if let Ok(flows) = std::fs::read_dir(home.flows_dir()) {
        for flow in flows.flatten() {
            let name = flow.file_name().to_string_lossy().into_owned();
            if !valid_name(&name) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(flow.path().join("runs")) else {
                continue;
            };
            // A link is not followed: only a folder that is one.
            let ids = entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            seen.push((name, ids));
        }
    }
    let gone = ledger
        .call_read(move |c| {
            seen.into_iter()
                .map(|(flow, ids)| {
                    let ended = firings::ended_before(c, &flow, &ids, before)?;
                    Ok((flow, ended))
                })
                .collect::<Result<Vec<_>>>()
        })
        .await?;
    let (mut count, mut bytes) = (0usize, 0u64);
    for (flow, ids) in gone {
        for id in ids {
            let dir = home.firing_dir(&flow, &id);
            let size = size_of(&dir);
            // One that could not go is not counted: it goes at a later sweep.
            if !dry_run && let Err(e) = std::fs::remove_dir_all(&dir) {
                warn!(flow, firing = id, error = %e, "gc: a firing's folder not removed");
                continue;
            }
            bytes += size;
            count += 1;
        }
    }
    Ok((count, bytes))
}

/// The bytes of the files under `dir`; a link is not followed.
fn size_of(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => size_of(&e.path()),
            _ => e.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
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

/// Once a day, unless the config says the journal is kept for good.
pub async fn daily(home: Home, ledger: Ledger, config: Arc<RwLock<Loaded>>, shutdown: CancellationToken) {
    loop {
        // Unset is not "never": a journal is kept 30d unless the file says
        // otherwise, so an old run's terminal bytes do not pile up for good.
        let keep = config.read().unwrap().config.retention_journal().to_string();
        if !rl_proto::config::is_never(&keep) {
            let due = match ledger.call_read(|c| meta::get_i64(c, LAST_AT)).await {
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
