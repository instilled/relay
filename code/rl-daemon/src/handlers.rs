//! Every method `rld` answers, registered in one place.

use crate::router::Router;
use crate::server::Daemon;
use rl_core::time;
use rl_ledger::{events, graph, hidden, order, presence, runs};
use rl_proto::PROTOCOL_VERSION;
use rl_proto::methods::*;
use rl_proto::rpc::RpcError;
use tracing::{info, warn};

pub fn register(r: &mut Router<Daemon>) {
    crate::flows::rpc::register(r);
    r.register::<DaemonPing, _, _>(|d, ()| async move {
        Ok(DaemonInfo {
            protocol: PROTOCOL_VERSION,
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
            started_at: d.started_at,
            home: d.home.path().to_path_buf(),
        })
    });
    r.register::<DaemonShutdown, _, _>(|d, ()| async move {
        info!("shutdown requested");
        d.shutdown.cancel();
        Ok(())
    });

    r.register::<RunList, _, _>(|d, filter| async move {
        d.ledger
            .call(move |c| runs::list(c, &filter))
            .await
            .map_err(RpcError::internal)
    });
    r.register::<RunGet, _, _>(|d, RunId { id }| async move {
        let found = d
            .ledger
            .call({
                let id = id.clone();
                move |c| runs::get(c, &id)
            })
            .await
            .map_err(RpcError::internal)?;
        found.ok_or_else(|| RpcError::not_found("run", &id))
    });
    r.register::<rl_proto::methods::WorkspaceUp, _, _>(|d, p| async move {
        crate::workspace::up(&d, &p.path).await.map_err(internal)
    });
    r.register::<rl_proto::methods::WorkspaceList, _, _>(|d, ()| async move {
        crate::workspace::list(&d).await.map_err(internal)
    });
    r.register::<rl_proto::methods::WorkspaceRename, _, _>(|d, p| async move {
        crate::workspace::rename(&d, &p.path, &p.name).await.map_err(internal)
    });
    r.register::<rl_proto::methods::WorkspaceRemove, _, _>(|d, p| async move {
        crate::workspace::remove(&d, p)
            .await
            .map_err(|e| RpcError::invalid_params(format!("{e:#}")))
    });
    r.register::<AgentList, _, _>(|d, ()| async move {
        let config = d.config.read().unwrap().config.clone();
        Ok(crate::config::agents(&d.registry, &config))
    });
    r.register::<ImportList, _, _>(|d, p: ImportListParams| async move { Ok(crate::import::list(&d, p).await) });
    r.register::<ImportMove, _, _>(|d, p: ImportMoveParams| async move {
        crate::import::move_one(&d, p)
            .await
            .map_err(|e| RpcError::invalid_params(format!("{e:#}")))
    });
    r.register::<SetupGet, _, _>(|d, ()| async move {
        let seen = d
            .ledger
            .call(|c| rl_ledger::meta::get_i64(c, crate::setup::SEEN))
            .await
            .map_err(internal)?
            .unwrap_or(0);
        let loaded = crate::config::load(&d.home);
        let config = crate::config::report(&loaded, &d.registry);
        let agents = crate::config::agents(&d.registry, &loaded.config);
        *d.config.write().unwrap() = loaded;
        Ok(SetupState {
            seen: seen.clamp(0, u32::MAX as i64) as u32,
            config,
            agents,
        })
    });
    r.register::<SetupDone, _, _>(|d, p: SetupSave| async move {
        if p.agents.is_some() || p.assistant.is_some() {
            crate::setup::write(&d.home, p.agents.as_deref(), p.assistant.as_ref())
                .map_err(|e| RpcError::invalid_params(format!("{e:#}")))?;
        }
        let seen = p.seen as i64;
        d.ledger
            .call(move |c| rl_ledger::meta::set_i64(c, crate::setup::SEEN, seen))
            .await
            .map_err(internal)?;
        let loaded = crate::config::load(&d.home);
        let report = crate::config::report(&loaded, &d.registry);
        *d.config.write().unwrap() = loaded;
        Ok(report)
    });
    r.register::<AssistantRun, _, _>(|d, p: AssistantParams| async move {
        crate::assistant::run(&d, p)
            .await
            .map_err(|e| RpcError::invalid_params(format!("{e:#}")))
    });
    r.register::<SummaryMake, _, _>(|d, p: SummaryParams| async move {
        crate::summary::make(&d, p)
            .await
            .map_err(|e| RpcError::invalid_params(format!("{e:#}")))
    });
    r.register::<SummaryGet, _, _>(|d, p: SummaryParams| async move {
        crate::summary::get(&d, &p)
            .await
            .map_err(|e| RpcError::invalid_params(format!("{e:#}")))
    });
    r.register::<GcRun, _, _>(|d, p: GcParams| async move {
        let retention = d
            .config
            .read()
            .unwrap()
            .config
            .retention
            .as_ref()
            .and_then(|r| r.journal.clone());
        crate::gc::run(
            &d.home,
            &d.ledger,
            retention.as_deref(),
            p.older_than.as_deref(),
            p.dry_run,
        )
        .await
        .map_err(|e| RpcError::invalid_params(format!("{e:#}")))
    });
    r.register::<ConfigGet, _, _>(|d, ()| async move {
        let loaded = crate::config::load(&d.home);
        let report = crate::config::report(&loaded, &d.registry);
        *d.config.write().unwrap() = loaded;
        Ok(report)
    });
    r.register::<DecisionRaise, _, _>(|d, spec| async move {
        d.decisions
            .raise(spec)
            .await
            .map_err(|e| RpcError::invalid_params(format!("{e:#}")))
    });
    r.register::<DecisionList, _, _>(|d, f| async move { d.decisions.list(f).await.map_err(RpcError::internal) });
    r.register::<DecisionGet, _, _>(|d, DecisionId { id }| async move {
        d.decisions
            .get(&id)
            .await
            .map_err(RpcError::internal)?
            .ok_or_else(|| RpcError::not_found("decision", &id))
    });
    r.register::<DecisionTake, _, _>(|d, p: DecisionTakeParams| async move {
        d.decisions
            .take(&p.id, &p.options, p.note)
            .await
            .map_err(|e| RpcError::invalid_params(format!("{e:#}")))
    });
    r.register::<DecisionDismiss, _, _>(|d, p: DecisionNote| async move {
        d.decisions
            .dismiss(&p.id, p.note)
            .await
            .map_err(|e| RpcError::invalid_params(format!("{e:#}")))
    });

    r.register::<RunStart, _, _>(|d, spec| async move { d.supervisor.start(spec).await.map_err(internal) });
    r.register::<RunStop, _, _>(|d, RunStopParams { id, force }| async move {
        if d.supervisor.live(&id).is_none() {
            return Err(RpcError::not_found("live run", &id));
        }
        d.supervisor.stop(&id, force).await.map_err(internal)
    });
    r.register::<rl_proto::methods::RunInput, _, _>(|d, p: rl_proto::methods::RunInputParams| async move {
        if d.supervisor.live(&p.id).is_none() {
            return Err(RpcError::not_found("live run", &p.id));
        }
        d.supervisor.input(&p.id, p.bytes).await.map_err(internal)
    });

    r.register::<RunHook, _, _>(|d, p| async move {
        if p.dialect != rl_detect::claude::DIALECT {
            return Err(RpcError::invalid_params(format!(
                "unknown hook dialect `{}`",
                p.dialect
            )));
        }
        // Every Claude hook names the session's transcript: where its own recap is.
        if let Some(path) = p.payload.get("transcript_path").and_then(|v| v.as_str()) {
            let (id, path) = (p.id.clone(), path.to_string());
            d.ledger
                .call(move |c| rl_ledger::summaries::set_transcript(c, &id, &path))
                .await
                .map_err(internal)?;
        }
        if let Some(sig) = rl_detect::claude::signal(&p.event, &p.payload) {
            d.supervisor.apply(&p.id, sig).await.map_err(internal)?;
        }
        // An agent you did not name is named for what you first asked it.
        if p.event == "UserPromptSubmit"
            && let Some(prompt) = p.payload.get("prompt").and_then(|v| v.as_str())
        {
            d.supervisor
                .name_from_prompt(&p.id, rl_detect::claude::DIALECT, prompt)
                .await
                .map_err(internal)?;
        }
        let run = get_run(&d, &p.id).await?;
        // A turn over, or a wait on you: Claude may have written its recap.
        // Read after the reply, which the hook is waiting on.
        if matches!(p.event.as_str(), "Stop" | "Notification") {
            let (d, run) = (d.clone(), run.clone());
            tokio::spawn(async move {
                if let Err(e) = crate::recap::pick_up(&d, &run).await {
                    warn!(run = %run.id, error = %format!("{e:#}"), "recap not picked up");
                }
            });
        }
        Ok(run)
    });
    r.register::<RunMark, _, _>(|d, RunMarkParams { id, mark, note }| async move {
        let detail = note.or_else(|| Some(format!("marked {mark}")));
        d.supervisor
            .apply(&id, rl_detect::Signal::manual(mark.state(), detail))
            .await
            .map_err(internal)?;
        get_run(&d, &id).await
    });

    r.register::<RunSeen, _, _>(|d, RunIds { ids }| async move {
        let now = time::now();
        d.ledger
            .call(move |c| runs::set_seen(c, &ids, now))
            .await
            .map_err(RpcError::internal)
    });

    r.register::<RunRename, _, _>(|d, p: RunRenameParams| async move {
        // Nothing for a name takes the rename back.
        let back = d
            .ledger
            .call({
                let (id, title) = (p.id.clone(), p.title.trim().to_string());
                move |c| {
                    if title.is_empty() {
                        runs::restore_title(c, &id)
                    } else {
                        runs::set_title(c, &id, &title).map(|()| true)
                    }
                }
            })
            .await
            .map_err(RpcError::internal)?;
        // Read it back rather than report success blindly: an id nobody knows
        // writes no row, and the caller should hear that rather than assume.
        let run = get_run(&d, &p.id).await?;
        if !back {
            return Err(RpcError::invalid_params(format!(
                "{} has no name of Relay's to go back to · type one",
                run.title
            )));
        }
        Ok(run)
    });

    r.register::<rl_proto::methods::RunResume, _, _>(|d, p| async move {
        crate::resume::resume(&d, p).await.map_err(RpcError::internal)
    });

    r.register::<RunArchive, _, _>(|d, p: RunArchiveParams| async move {
        let now = time::now();
        d.ledger
            .call(move |c| {
                if p.all_seen {
                    runs::archive_seen(c, now)
                } else {
                    runs::archive(c, &p.ids, now)
                }
            })
            .await
            .map_err(RpcError::internal)
    });

    r.register::<RunPin, _, _>(|d, p: RunPinParams| async move {
        let at = p.pinned.then(time::now);
        d.ledger
            .call({
                let id = p.id.clone();
                move |c| runs::set_pinned(c, &id, at)
            })
            .await
            .map_err(RpcError::internal)?;
        get_run(&d, &p.id).await
    });

    r.register::<RunHide, _, _>(|d, p: RunHideParams| async move {
        // Known first: hiding a run nobody knows would write nothing and say so
        // only by what came back.
        get_run(&d, &p.id).await?;
        let at = p.hidden.then(time::now);
        d.ledger
            .call({
                let id = p.id.clone();
                move |c| runs::set_hidden(c, &id, at)
            })
            .await
            .map_err(RpcError::internal)?;
        get_run(&d, &p.id).await
    });

    r.register::<DigestHide, _, _>(|d, p: DigestHideParams| async move {
        if !Hidden::valid_key(&p.key) {
            return Err(RpcError::invalid_params(format!(
                "{:?}: a project is g:<folder>, a job j:<kind>:<name>@<group>",
                p.key
            )));
        }
        let at = p.hidden.then(time::now);
        d.ledger
            .call(move |c| {
                hidden::set(c, &p.key, at)?;
                hidden::list(c)
            })
            .await
            .map_err(RpcError::internal)
    });

    r.register::<DigestOrder, _, _>(|d, p: Order| async move {
        if !Order::valid_group(&p.group) {
            return Err(RpcError::invalid_params(format!(
                "{:?}: a project is g:<folder>, or g:everywhere",
                p.group
            )));
        }
        if let Some(bad) = p.rows.iter().find(|k| !Order::valid_row(k)) {
            return Err(RpcError::invalid_params(format!(
                "{bad:?}: a run is r:<id>, a job j:<kind>:<name>@<group>"
            )));
        }
        d.ledger
            .call(move |c| {
                order::set(c, &p.group, &p.rows)?;
                order::list(c)
            })
            .await
            .map_err(RpcError::internal)
    });

    r.register::<GraphGet, _, _>(|d, GraphParams { since }| async move {
        let since = since.unwrap_or_else(|| time::now() - jiff::SignedDuration::from_hours(24 * 7));
        let (flows, runs, events, asks) = d
            .ledger
            .call(move |c| {
                Ok((
                    rl_ledger::flows::list(c, false)?,
                    graph::runs_since(c, since)?,
                    graph::events_since(c, since)?,
                    graph::asks_since(c, since)?,
                ))
            })
            .await
            .map_err(RpcError::internal)?;
        Ok(crate::graph::build(
            since,
            crate::graph::Inputs {
                flows: &flows,
                runs: &runs,
                events: &events,
                asks: &asks,
            },
        ))
    });

    r.register::<DigestGet, _, _>(|d, DigestParams { since }| async move {
        let since = since.unwrap_or(jiff::Timestamp::UNIX_EPOCH);
        let (found, mut recaps, registered) = d
            .ledger
            .call(move |c| {
                let found = runs::list_for_digest(c, since)?
                    .into_iter()
                    .map(|r| {
                        let e = events::latest_for_run(c, &r.id)?;
                        let emitted = events::emitted_for_run(c, &r.id)?;
                        Ok((r, e, emitted))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                // Every row's recap in one query, never a session file read.
                let ids: Vec<String> = found.iter().map(|(r, _, _)| r.id.clone()).collect();
                let recaps = rl_ledger::summaries::lines(c, &ids)?;
                Ok((found, recaps, rl_ledger::workspaces::list(c)?))
            })
            .await
            .map_err(RpcError::internal)?;
        // Every folder the list can show: those registered, those run in.
        let mut folders: Vec<std::path::PathBuf> = registered.into_iter().map(|w| w.path).collect();
        folders.extend(found.iter().map(|(r, _, _)| r.workspace.clone()));
        folders.sort();
        folders.dedup();
        let mut checkouts = tokio::task::spawn_blocking(move || {
            folders
                .iter()
                .filter_map(|f| crate::git::checkout(f))
                .collect::<Vec<_>>()
        })
        .await
        .map_err(RpcError::internal)?;
        let badges = d.config.read().unwrap().config.workspaces.badges.clone();
        for c in &mut checkouts {
            c.badges = d.badges.of(c, &badges);
        }
        let joiners = d.supervisor.joiners();
        let mut cwds = d.supervisor.cwds();
        let items = found
            .into_iter()
            .map(|(run, e, emitted)| {
                let joined_by = joiners.get(&run.id).cloned();
                let cwd = cwds.remove(&run.id);
                let recap = recaps.remove(&run.id);
                crate::digest::item(run, e, joined_by, emitted, cwd, recap)
            })
            .collect();
        let decisions = d
            .decisions
            .list(DecisionFilter::default())
            .await
            .map_err(RpcError::internal)?;
        let (lost, hidden, order) = d
            .ledger
            .call(|c| Ok((runs::lost(c)?, hidden::list(c)?, order::list(c)?)))
            .await
            .map_err(RpcError::internal)?;
        Ok(Digest {
            since,
            items,
            decisions,
            checkouts,
            lost,
            hidden,
            order,
        })
    });

    r.register::<EventList, _, _>(|d, filter| async move {
        d.ledger
            .call(move |c| events::list(c, &filter))
            .await
            .map_err(RpcError::internal)
    });

    r.register::<EventEmit, _, _>(|d, p| async move {
        emit_kind_ok(&p.kind).map_err(RpcError::invalid_params)?;
        let now = time::now();
        // Inside a run, the event belongs to that run's chain and folder;
        // outside one, the caller names the folder.
        let (workspace, group_key, run) = match &p.run {
            Some(id) => {
                let r = get_run(&d, id).await?;
                (r.workspace.clone(), r.group_key(), Some(r.id))
            }
            None => {
                let ws = p
                    .workspace
                    .clone()
                    .ok_or_else(|| RpcError::invalid_params("emit needs a run ($RELAY_RUN) or a folder"))?;
                let group = ws.to_string_lossy().into_owned();
                (ws, group, None)
            }
        };
        let group_key = p.group.clone().unwrap_or(group_key);
        let summary = p.summary.clone().unwrap_or_else(|| p.kind.clone());
        let mut ev = events::NewEvent {
            at: now,
            workspace,
            run,
            group_key,
            kind: p.kind.clone(),
            summary,
            data: None,
        };
        if let Some(data) = p.data.clone() {
            ev = ev.with_data(data);
        }
        d.ledger
            .call(move |c| events::append(c, &ev).map(|_| ()))
            .await
            .map_err(RpcError::internal)
    });

    r.register::<PresenceTouch, _, _>(|d, PresenceClient { client }| async move {
        let now = time::now();
        let previous = d
            .ledger
            .call({
                let client = client.clone();
                move |c| presence::touch(c, &client, now)
            })
            .await
            .map_err(RpcError::internal)?;
        Ok(PresenceInfo {
            client,
            last_seen_at: previous,
        })
    });
}

/// A kind a command may `rl emit`: a dotted, lowercase name of its own, each
/// word starting with a letter and going on in letters, digits, `_` or `-`
/// (`flint.product.new-feedback`, as flows are named). The daemon's own
/// families are refused, so an emit cannot forge a `run.failed` the digest
/// would read as real.
fn emit_kind_ok(kind: &str) -> Result<(), String> {
    if let Some(p) = rl_proto::types::event_kind::OWN.iter().find(|p| kind.starts_with(**p)) {
        return Err(format!("`{kind}`: `{p}` events are the daemon's; pick your own kind"));
    }
    let shaped = kind.contains('.')
        && kind.split('.').all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
                && seg.starts_with(|c: char| c.is_ascii_lowercase())
        });
    if !shaped {
        return Err(format!(
            "`{kind}`: a kind is dotted lowercase words, like `log.issue` or `app.new-feedback`"
        ));
    }
    Ok(())
}

/// The common shape for wrapping an internal failure: the full causal chain,
/// via `{:#}`, rather than just the top-level message.
fn internal(e: anyhow::Error) -> RpcError {
    RpcError::internal(format!("{e:#}"))
}

async fn get_run(d: &Daemon, id: &str) -> Result<rl_proto::types::Run, RpcError> {
    let found = d
        .ledger
        .call({
            let id = id.to_string();
            move |c| runs::get(c, &id)
        })
        .await
        .map_err(RpcError::internal)?;
    found.ok_or_else(|| RpcError::not_found("run", id))
}
