//! Taking a project away: `workspace.remove`. Unregistering is the old half —
//! its rules stop and are marked removed. What a project leaves behind is the
//! new: the runs still going there, the ended ones that keep its header on
//! the list, the questions it asked, the yes given to its rules, and its
//! `.relay/`. Each goes only as far as the caller asks; nothing is deleted.

use crate::rulestore::resolve;
use crate::server::Daemon;
use anyhow::{Context, Result, bail};
use rl_core::rules_dir::{self, RulesDir};
use rl_ledger::{decisions, runs, workspaces};
use rl_proto::methods::{WorkspaceRemoveParams, WorkspaceRemoved};
use rl_proto::schedule::WORKSPACE_DIR;
use rl_proto::types::{Run, RunKind};
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// How long the runs asked to stop get to end before they are killed.
const GRACE: Duration = Duration::from_secs(3);
/// How long killed runs get to be seen ending before the removal goes on
/// without them.
const KILLED: Duration = Duration::from_secs(3);
/// The note a question dismissed this way carries.
const DISMISS_NOTE: &str = "its project was removed";

pub async fn remove(d: &Daemon, p: WorkspaceRemoveParams) -> Result<WorkspaceRemoved> {
    let path = resolve(&p.path);
    // Every check before anything is done, so a refusal leaves all as it was.
    let relay = path.join(WORKSPACE_DIR);
    let movable = if p.move_rules {
        match rules_dir::rules_dir(&relay, &d.home) {
            RulesDir::Symlink => bail!("{} is a symlink; it is left where it is", relay.display()),
            RulesDir::Home => bail!("{} is the Relay home; it is left where it is", relay.display()),
            RulesDir::Nothing => false,
            RulesDir::Movable(_) => true,
        }
    } else {
        false
    };
    let name = {
        let p2 = path.clone();
        d.ledger.call(move |c| workspaces::get(c, &p2)).await?
    }
    .map(|w| w.name)
    .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
    .unwrap_or_else(|| "project".into());

    // Its rules first, so nothing new starts there while the rest goes.
    d.reactor.remove_scope(&path).await?;
    let removed = d.scheduler.remove_workspace(&path).await?;

    let asked = {
        let p2 = path.clone();
        d.ledger.call(move |c| decisions::open_in(c, &p2)).await?
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
        let (p2, now) = (path.clone(), rl_core::time::now());
        d.ledger.call(move |c| runs::archive_workspace(c, &p2, now)).await?
    } else {
        0
    };

    let rules_moved_to = if movable {
        let (relay, home, now) = (relay.clone(), d.home.clone(), rl_core::time::now());
        let n = name.clone();
        Some(
            tokio::task::spawn_blocking(move || rules_dir::move_aside(&relay, &home, &n, now))
                .await
                .context("moving the rules")??,
        )
    } else {
        None
    };

    info!(
        workspace = %path.display(),
        removed, stopped, still_running, archived, dismissed,
        rules_moved = rules_moved_to.is_some(),
        "workspace removed"
    );
    Ok(WorkspaceRemoved {
        path,
        removed,
        stopped,
        still_running,
        archived,
        dismissed,
        rules_moved_to,
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
        d.ledger.call(move |c| runs::live_in(c, &p2)).await?
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
    let mut going = wait_ended(d, &ids, GRACE).await?;
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
    Ok((ids.len() - going.len(), going.len()))
}

/// Polls the ledger until every run in `ids` has ended or `limit` is up;
/// returns those still going.
async fn wait_ended(d: &Daemon, ids: &[String], limit: Duration) -> Result<Vec<String>> {
    let until = Instant::now() + limit;
    loop {
        let going: Vec<String> = {
            let ids = ids.to_vec();
            d.ledger
                .call(move |c| {
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
