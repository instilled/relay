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
