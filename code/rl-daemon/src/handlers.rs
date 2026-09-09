//! Every method `rld` answers, registered in one place.

use crate::router::Router;
use crate::server::Daemon;
use relay_core::time;
use relay_ledger::{events, presence, runs};
use relay_proto::PROTOCOL_VERSION;
use relay_proto::methods::*;
use relay_proto::rpc::RpcError;
use tracing::info;

pub fn register(r: &mut Router<Daemon>) {
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
    r.register::<relay_proto::methods::ScheduleList, _, _>(|d, f| async move {
        d.scheduler
            .list(f.all)
            .await
            .map_err(|e| RpcError::internal(format!("{e:#}")))
    });
    r.register::<relay_proto::methods::ScheduleRun, _, _>(|d, p| async move {
        d.scheduler
            .run_now(&p.name)
            .await
            .map_err(|e| RpcError::internal(format!("{e:#}")))
    });
    r.register::<relay_proto::methods::SchedulePause, _, _>(|d, p| async move {
        d.scheduler
            .set_paused(&p.name, true)
            .await
            .map_err(|e| RpcError::internal(format!("{e:#}")))
    });
    r.register::<relay_proto::methods::ScheduleResume, _, _>(|d, p| async move {
        d.scheduler
            .set_paused(&p.name, false)
            .await
            .map_err(|e| RpcError::internal(format!("{e:#}")))
    });
    r.register::<relay_proto::methods::ScheduleReload, _, _>(|d, ()| async move {
        d.scheduler
            .reload()
            .await
            .map_err(|e| RpcError::internal(format!("{e:#}")))
    });
    r.register::<RunStart, _, _>(|d, spec| async move {
        d.supervisor
            .start(spec)
            .await
            .map_err(|e| RpcError::internal(format!("{e:#}")))
    });
    r.register::<RunStop, _, _>(|d, RunStopParams { id, force }| async move {
        if d.supervisor.live(&id).is_none() {
            return Err(RpcError::not_found("live run", &id));
        }
        d.supervisor
            .stop(&id, force)
            .await
            .map_err(|e| RpcError::internal(format!("{e:#}")))
    });

    r.register::<RunHook, _, _>(|d, p| async move {
        if p.dialect != relay_detect::claude::DIALECT {
            return Err(RpcError::invalid_params(format!(
                "unknown hook dialect `{}`",
                p.dialect
            )));
        }
        if let Some(sig) = relay_detect::claude::signal(&p.event, &p.payload) {
            d.supervisor
                .apply(&p.id, sig)
                .await
                .map_err(|e| RpcError::internal(format!("{e:#}")))?;
        }
        get_run(&d, &p.id).await
    });
    r.register::<RunMark, _, _>(|d, RunMarkParams { id, mark, note }| async move {
        let detail = note.or_else(|| Some(format!("marked {mark}")));
        d.supervisor
            .apply(&id, relay_detect::Signal::manual(mark.state(), detail))
            .await
            .map_err(|e| RpcError::internal(format!("{e:#}")))?;
        get_run(&d, &id).await
    });

    r.register::<RunSeen, _, _>(|d, RunIds { ids }| async move {
        let now = time::now();
        d.ledger
            .call(move |c| runs::set_seen(c, &ids, now))
            .await
            .map_err(RpcError::internal)
    });

    r.register::<DigestGet, _, _>(|d, DigestParams { since }| async move {
        let since = since.unwrap_or(jiff::Timestamp::UNIX_EPOCH);
        let found = d
            .ledger
            .call(move |c| {
                runs::list_for_digest(c, since)?
                    .into_iter()
                    .map(|r| {
                        let e = events::latest_for_run(c, &r.id)?;
                        Ok((r, e))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()
            })
            .await
            .map_err(RpcError::internal)?;
        let joiners = d.supervisor.joiners();
        let items = found
            .into_iter()
            .map(|(run, e)| {
                let joined_by = joiners.get(&run.id).cloned();
                crate::digest::item(run, e, joined_by)
            })
            .collect();
        Ok(Digest { since, items })
    });

    r.register::<EventList, _, _>(|d, filter| async move {
        d.ledger
            .call(move |c| events::list(c, &filter))
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

async fn get_run(d: &Daemon, id: &str) -> Result<relay_proto::types::Run, RpcError> {
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
