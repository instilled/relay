//! Projects: `workspace.up` registers a folder so flows can run in it,
//! `workspace.list` and `workspace.rename` read and name them, and
//! `workspace.remove` takes one away — the runs still going there, the ended
//! ones that keep its header on the list, the questions it asked, and the
//! flows `in` it, which no longer load. Each goes only as far as the caller
//! asks; nothing is deleted.

use crate::server::Daemon;
use anyhow::{Context, Result, anyhow, bail};
use rl_core::time;
use rl_ledger::{Ledger, decisions, runs, workspaces};
use rl_proto::methods::{WorkspaceRemoveParams, WorkspaceRemoved};
use rl_proto::types::Workspace;
use rl_proto::types::{Run, RunKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// How long the runs asked to stop get to end before they are killed.
const GRACE: Duration = Duration::from_secs(3);
/// How long killed runs get to be seen ending before the removal goes on
/// without them.
const KILLED: Duration = Duration::from_secs(3);
/// The note a question dismissed this way carries.
const DISMISS_NOTE: &str = "its project was removed";

/// The project folder something started in `cwd` (canonical) is filed
/// under: the registered folder containing it wins, the deepest where they
/// nest; then its checkout's root (`toplevel`, as `git::placement` found
/// it); then `cwd` itself. A run's, and an agent's own session's.
pub async fn filed_under(ledger: &Ledger, cwd: &Path, toplevel: Option<PathBuf>) -> Result<PathBuf> {
    let registered = ledger
        .call_read({
            let cwd = cwd.to_path_buf();
            move |c| workspaces::containing(c, &cwd)
        })
        .await?
        .map(|w| w.path);
    Ok(registered.or(toplevel).unwrap_or_else(|| cwd.to_path_buf()))
}

/// A folder as the ledger keys it: its canonical path, or — once it is gone
/// from disk — its canonical parent with its own name, or else just as given.
/// Registered paths are canonical, so a folder deleted since can still be
/// named the way it was named then.
fn resolve(path: &Path) -> PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return p;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(p) = std::fs::canonicalize(parent)
    {
        return p.join(name);
    }
    path.to_path_buf()
}

/// Registers a folder, named for itself; one registered already is as it was.
/// The flows are read again, so one `in` it loads.
pub async fn up(d: &Daemon, path: &Path) -> Result<Workspace> {
    let path = std::fs::canonicalize(path).with_context(|| format!("workspace {}", path.display()))?;
    if !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let now = time::now();
    let w = {
        let path = path.clone();
        d.ledger
            .call(move |c| workspaces::register(c, &path, &name, now))
            .await?
    };
    info!(workspace = %path.display(), "workspace up");
    // Flows `in` it load now, not at the next reload.
    if let Err(e) = d.flows.reload().await {
        warn!(error = %format!("{e:#}"), "flows not reloaded");
    }
    Ok(w)
}

pub async fn list(d: &Daemon) -> Result<Vec<Workspace>> {
    d.ledger.call_read(workspaces::list).await
}

/// Renames a registered folder (any spelling of its path), keeping
/// everything else. Fails when the folder was never `rl up`'d.
pub async fn rename(d: &Daemon, path: &Path, name: &str) -> Result<Workspace> {
    let (path, name) = (resolve(path), name.to_string());
    d.ledger
        .call(move |c| workspaces::rename(c, &path, &name))
        .await?
        .ok_or_else(|| anyhow!("not a registered workspace; `rl up` there first"))
}

pub async fn remove(d: &Daemon, p: WorkspaceRemoveParams) -> Result<WorkspaceRemoved> {
    let path = resolve(&p.path);
    let removed = {
        let p2 = path.clone();
        d.ledger.call(move |c| workspaces::remove(c, &p2)).await?
    };
    // Flows `in` it no longer load, so nothing new starts there while the rest goes.
    if let Err(e) = d.flows.reload().await {
        warn!(error = %format!("{e:#}"), "flows not reloaded");
    }

    let asked = {
        let p2 = path.clone();
        d.ledger.call_read(move |c| decisions::open_in(c, &p2)).await?
    };
    let mut dismissed = 0;
    for q in asked {
        match d.decisions.dismiss(&q.id, Some(DISMISS_NOTE.into())).await {
            Ok(_) => dismissed += 1,
            Err(e) => warn!(decision = q.id, error = %format!("{e:#}"), "not dismissed"),
        }
    }

    let (stopped, still_running) = if p.stop_runs { stop_all(d, &path).await? } else { (0, 0) };

    let archived = if p.archive_runs {
        let (p2, now) = (path.clone(), time::now());
        d.ledger.call(move |c| runs::archive_workspace(c, &p2, now)).await?
    } else {
        0
    };

    info!(
        workspace = %path.display(),
        removed, stopped, still_running, archived, dismissed,
        "workspace removed"
    );
    Ok(WorkspaceRemoved {
        path,
        removed,
        stopped,
        still_running,
        archived,
        dismissed,
    })
}

/// Stops every run in the folder still going and waits for them to end:
/// a shell is hung up on, anything else is asked to terminate, and whatever
/// is still there after [`GRACE`] is killed. Returns how many ended and how
/// many did not — a run no holder of ours answers for (imported, or its
/// holder gone) cannot be told, and counts among the latter.
async fn stop_all(d: &Daemon, path: &Path) -> Result<(usize, usize)> {
    let live: Vec<Run> = {
        let p2 = path.to_path_buf();
        d.ledger.call_read(move |c| runs::live_in(c, &p2)).await?
    };
    if live.is_empty() {
        return Ok((0, 0));
    }
    for r in &live {
        if d.supervisor.live(&r.id).is_none() {
            continue;
        }
        let asked = if r.kind == RunKind::Shell {
            d.supervisor.hang_up(&r.id).await
        } else {
            d.supervisor.stop(&r.id, false).await
        };
        if let Err(e) = asked {
            warn!(id = r.id, error = %format!("{e:#}"), "stop not delivered");
        }
    }
    let ids: Vec<String> = live.iter().map(|r| r.id.clone()).collect();
    let going = see_ended(d, &ids).await?;
    Ok((ids.len() - going.len(), going.len()))
}

/// Gives runs already asked to stop [`GRACE`] to end, kills those that have
/// not, and waits [`KILLED`] more; returns those still going after all that.
async fn see_ended(d: &Daemon, ids: &[String]) -> Result<Vec<String>> {
    let mut going = wait_ended(d, ids, GRACE).await?;
    if !going.is_empty() {
        for id in &going {
            if d.supervisor.live(id).is_some()
                && let Err(e) = d.supervisor.stop(id, true).await
            {
                warn!(id, error = %format!("{e:#}"), "kill not delivered");
            }
        }
        going = wait_ended(d, &going, KILLED).await?;
    }
    Ok(going)
}

/// `run.stop` with `archive`: once the run asked to stop has ended — killed
/// if it takes past [`GRACE`] — it is archived, the way a removed project's
/// runs are. One that never ends stays on the list, to be seen.
pub(crate) async fn archive_once_ended(d: Arc<Daemon>, id: String) {
    let ids = vec![id.clone()];
    match see_ended(&d, &ids).await {
        Ok(going) if going.is_empty() => {
            let now = time::now();
            if let Err(e) = d.ledger.call(move |c| runs::archive(c, &ids, now)).await {
                warn!(id, error = %format!("{e:#}"), "not archived");
            }
        }
        Ok(_) => warn!(id, "did not end; not archived"),
        Err(e) => warn!(id, error = %format!("{e:#}"), "not archived"),
    }
}

/// Polls the ledger until every run in `ids` has ended or `limit` is up;
/// returns those still going.
async fn wait_ended(d: &Daemon, ids: &[String], limit: Duration) -> Result<Vec<String>> {
    let until = Instant::now() + limit;
    loop {
        let going: Vec<String> = {
            let ids = ids.to_vec();
            d.ledger
                .call_read(move |c| {
                    let mut going = Vec::new();
                    for id in ids {
                        if runs::get(c, &id)?.is_some_and(|r| !r.state.is_terminal()) {
                            going.push(id);
                        }
                    }
                    Ok(going)
                })
                .await?
        };
        if going.is_empty() || Instant::now() >= until {
            return Ok(going);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
