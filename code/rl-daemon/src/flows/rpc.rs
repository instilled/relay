//! The `flow.*` methods.

use super::engine::{Fired, Refused};
use crate::router::Router;
use crate::server::Daemon;
use rl_ledger::{firings, flows};
use rl_proto::flow::{
    FiringFilter, Flow, FlowApproval, FlowApprove, FlowErrors, FlowFilter, FlowFirings, FlowList, FlowName, FlowPause,
    FlowReload, FlowResume, FlowRun,
};
use rl_proto::rpc::RpcError;

/// How many firings `flow.firings` gives when it is not told.
const FIRINGS: u32 = 20;

pub fn register(r: &mut Router<Daemon>) {
    r.register::<FlowList, _, _>(|d, f: FlowFilter| async move {
        let all = d
            .flows
            .ledger
            .call(move |c| flows::list(c, f.all))
            .await
            .map_err(internal)?;
        Ok(all.into_iter().map(|f| d.flows.shown(f)).collect())
    });
    r.register::<FlowErrors, _, _>(|d, ()| async move { Ok(d.flows.not_loaded()) });
    r.register::<FlowReload, _, _>(|d, ()| async move {
        // A reload re-reads the config too: one reload for everything on disk.
        *d.config.write().unwrap() = crate::config::load(&d.home);
        d.flows.reload().await.map_err(internal)
    });
    r.register::<FlowApprove, _, _>(|d, FlowApproval { name, hash }| async move {
        let (n, h) = (name.clone(), hash.clone());
        // Planned from now once it has its yes: marked first, as approving clears `next_at`.
        d.flows.plan_afresh(&name);
        let done = d
            .flows
            .ledger
            .call(move |c| flows::approve(c, &n, &h))
            .await
            .map_err(internal)?;
        if done {
            return get(&d, &name).await;
        }
        d.flows.afresh.lock().unwrap().remove(&name);
        // Not given: the flow is not there, or is not the version shown.
        let now = get(&d, &name).await?;
        if now.removed_at.is_some() {
            return Err(missing(&d, &name));
        }
        Err(RpcError::invalid_params(format!(
            "`{name}` changed since it was shown ({} now, not {}): look at it again",
            short(&now.hash),
            short(&hash)
        )))
    });
    r.register::<FlowPause, _, _>(|d, p: FlowName| async move { set_paused(&d, p.name, true).await });
    r.register::<FlowResume, _, _>(|d, p: FlowName| async move { set_paused(&d, p.name, false).await });
    r.register::<FlowRun, _, _>(|d, FlowName { name }| async move {
        match d.flows.fire(&name, Fired::Hand, None).await {
            Ok(Some(f)) => Ok(f),
            Ok(None) => Err(RpcError::invalid_params(format!(
                "`{name}` is still firing, so this one was skipped (overlap)"
            ))),
            Err(e) => Err(match e.downcast_ref::<Refused>() {
                Some(Refused::NotLoaded(_)) => missing(&d, &name),
                Some(r @ Refused::NeedsYes(_)) => RpcError::invalid_params(r.to_string()),
                None => internal(e),
            }),
        }
    });
    r.register::<FlowFirings, _, _>(|d, f: FiringFilter| async move {
        d.flows
            .ledger
            .call(move |c| firings::list(c, &f.flow, f.limit.unwrap_or(FIRINGS)))
            .await
            .map_err(internal)
    });
}

async fn set_paused(d: &Daemon, name: String, paused: bool) -> Result<Flow, RpcError> {
    let n = name.clone();
    let done = d
        .flows
        .ledger
        .call(move |c| flows::set_paused(c, &n, paused))
        .await
        .map_err(internal)?;
    if !done {
        return Err(missing(d, &name));
    }
    if !paused {
        d.flows.replan(&name).await.map_err(internal)?;
    }
    get(d, &name).await
}

async fn get(d: &Daemon, name: &str) -> Result<Flow, RpcError> {
    let n = name.to_string();
    let found = d
        .flows
        .ledger
        .call(move |c| flows::get(c, &n))
        .await
        .map_err(internal)?;
    found.map(|f| d.flows.shown(f)).ok_or_else(|| missing(d, name))
}

/// No flow by that name to act on: one that did not load says why.
fn missing(d: &Daemon, name: &str) -> RpcError {
    match d.flows.not_loaded().into_iter().find(|n| n.flow == name) {
        Some(n) => RpcError::invalid_params(format!("`{name}` did not load: {n}")),
        None => RpcError::not_found("flow", name),
    }
}

/// A hash as the panel shows it: its first eight.
fn short(hash: &str) -> String {
    hash.chars().take(8).collect()
}

fn internal(e: anyhow::Error) -> RpcError {
    RpcError::internal(format!("{e:#}"))
}
