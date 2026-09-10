//! Socket server: bind `rld.sock`, accept, one task per connection, dispatch
//! through the router. Shutdown is cooperative; it waits briefly for in-flight
//! connections so the `daemon.shutdown` reply gets out.

use crate::router::Router;
use crate::schedule::Scheduler;
use crate::supervisor::{self, Supervisor};
use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use relay_core::Home;
use relay_detect::Registry;
use relay_ledger::Ledger;
use relay_proto::codec::{read_message, write_message};
use relay_proto::methods::{AttachParams, Method, RunAttach};
use relay_proto::rpc::{Id, Request, Response, RpcError};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Shared state every handler can reach.
pub struct Daemon {
    pub home: Home,
    pub ledger: Ledger,
    pub supervisor: Arc<Supervisor>,
    pub scheduler: Arc<Scheduler>,
    pub decisions: Arc<crate::decisions::Decisions>,
    pub reactor: Arc<crate::reactions::Reactor>,
    pub started_at: Timestamp,
    pub shutdown: CancellationToken,
}

#[derive(Clone, Default)]
pub struct ServeOptions {
    /// Where `rl-session` is; defaults to `$RELAY_SESSION_BIN` or a sibling of `rld`.
    pub session_bin: Option<PathBuf>,
    /// Where `rl` is, for the hook commands CLIs call back with; defaults to
    /// `$RELAY_RL_BIN` or a sibling of `rld`.
    pub rl_bin: Option<PathBuf>,
    /// Detection registry; defaults to the built-ins merged with `$RELAY_HOME/detect/`.
    pub registry: Option<Arc<Registry>>,
}

pub async fn serve(home: Home, shutdown: CancellationToken) -> Result<()> {
    serve_with(home, shutdown, ServeOptions::default()).await
}

/// Runs until `daemon.shutdown` or `shutdown` is cancelled externally.
/// Removes the socket and pid file on the way out. Sessions are untouched.
pub async fn serve_with(home: Home, shutdown: CancellationToken, opts: ServeOptions) -> Result<()> {
    home.ensure_dirs()?;
    let ledger = Ledger::open(&home.ledger())?;
    let sock_path = home.daemon_socket();
    let listener = bind(&sock_path)?;
    std::fs::write(home.daemon_pid(), std::process::id().to_string())?;
    let session_bin = opts.session_bin.unwrap_or_else(supervisor::locate_session_bin);
    let rl_bin = opts.rl_bin.unwrap_or_else(supervisor::locate_rl_bin);
    info!(socket = %sock_path.display(), pid = std::process::id(), session_bin = %session_bin.display(), "rld listening");

    let registry = match opts.registry {
        Some(r) => r,
        None => match Registry::load(&home) {
            Ok(r) => Arc::new(r),
            Err(e) => {
                warn!(error = %format!("{e:#}"), "detection overrides ignored; using built-in registry");
                Arc::new(Registry::builtin())
            }
        },
    };
    match relay_detect::claude::write_settings(&home, &rl_bin) {
        Ok(p) => debug!(path = %p.display(), "claude hooks settings ready"),
        Err(e) => warn!(error = %e, "claude hooks settings not written; claude runs get heuristics only"),
    }

    let supervisor = Arc::new(Supervisor::new(
        home.clone(),
        ledger.clone(),
        session_bin,
        registry.clone(),
        shutdown.child_token(),
    ));
    match supervisor.reconcile().await {
        Ok(r) => info!(
            reattached = r.reattached,
            ended = r.ended,
            lost = r.lost,
            "sessions reconciled"
        ),
        Err(e) => warn!(error = %e, "reconciling sessions"),
    }
    let registry2 = registry.clone();
    let registry3 = registry.clone();
    let scheduler = Arc::new(Scheduler::new(
        home.clone(),
        ledger.clone(),
        supervisor.clone(),
        registry,
        shutdown.child_token(),
    ));
    // Loaded before the socket answers, so `rl schedule ls` right after
    // `rl daemon start` already knows when things fire.
    match scheduler.reload().await {
        Ok(r) => {
            for e in &r.errors {
                warn!(schedule = %e, "schedule not loaded");
            }
        }
        Err(e) => warn!(error = %format!("{e:#}"), "schedules not loaded"),
    }
    tokio::spawn(scheduler.clone().serve());
    let decisions = Arc::new(crate::decisions::Decisions::new(
        ledger.clone(),
        supervisor.clone(),
        registry2,
        shutdown.child_token(),
    ));
    tokio::spawn(decisions.clone().serve());
    let reactor = Arc::new(crate::reactions::Reactor::new(
        home.clone(),
        ledger.clone(),
        supervisor.clone(),
        decisions.clone(),
        registry3,
        shutdown.child_token(),
    ));
    // Read before the socket answers, so a rule is live as soon as `rld` is.
    match reactor.reload().await {
        Ok(r) => {
            for e in &r.errors {
                warn!(reaction = %e, "reaction not loaded");
            }
        }
        Err(e) => warn!(error = %format!("{e:#}"), "reactions not loaded"),
    }
    tokio::spawn(reactor.clone().serve());
    let daemon = Arc::new(Daemon {
        home: home.clone(),
        ledger,
        supervisor,
        scheduler,
        decisions,
        reactor,
        started_at: Timestamp::now(),
        shutdown: shutdown.clone(),
    });
    let mut router: Router<Daemon> = Router::new();
    crate::handlers::register(&mut router);
    let router = Arc::new(router);
    let mut conns = JoinSet::new();

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    conns.spawn(handle_conn(stream, daemon.clone(), router.clone()));
                }
                Err(e) => warn!(error = %e, "accept failed"),
            },
            Some(_) = conns.join_next() => {}
        }
    }

    // Let in-flight replies (notably the shutdown ack) drain, and let follow
    // tasks flush their journals. Runs themselves are left exactly as they are.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while conns.join_next().await.is_some() {}
    })
    .await;
    daemon.supervisor.drain().await;

    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(home.daemon_pid());
    info!("rld stopped");
    Ok(())
}

/// Binds the socket, clearing a stale file left by a crashed daemon. Refuses
/// if something is actually listening there.
fn bind(path: &std::path::Path) -> Result<UnixListener> {
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => bail!("rld already running on {}", path.display()),
        Err(e) if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound) => {
            if path.exists() {
                debug!(path = %path.display(), "removing stale socket");
                std::fs::remove_file(path)?;
            }
        }
        Err(e) => {
            // Not a socket at all (someone left a file there): clear it too.
            use std::os::unix::fs::FileTypeExt;
            let is_socket = std::fs::symlink_metadata(path)
                .map(|m| m.file_type().is_socket())
                .unwrap_or(false);
            if is_socket {
                return Err(e).context("probing existing socket");
            }
            warn!(path = %path.display(), "socket path held by a non-socket file; replacing it");
            std::fs::remove_file(path)?;
        }
    }
    UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))
}

async fn handle_conn(stream: UnixStream, daemon: Arc<Daemon>, router: Arc<Router<Daemon>>) {
    let (rd, mut wr) = stream.into_split();
    let mut rd = BufReader::new(rd);
    loop {
        let read = tokio::select! {
            _ = daemon.shutdown.cancelled() => return,
            r = read_message(&mut rd) => r,
        };
        let req: Request = match read {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(e) if e.kind() == ErrorKind::InvalidData => {
                // Can't reply with an id we couldn't parse; JSON-RPC says id: null.
                let res = Response::err(Id::Num(0), RpcError::new(RpcError::PARSE, e.to_string()));
                if write_message(&mut wr, &res).await.is_err() {
                    return;
                }
                continue;
            }
            Err(e) => {
                debug!(error = %e, "connection read failed");
                return;
            }
        };
        debug!(method = %req.method, "request");
        if req.method == RunAttach::NAME {
            // The one method that takes the connection over: after `ok`, the
            // stream speaks holder frames until the viewer hangs up.
            let params: Result<AttachParams, _> = serde_json::from_value(req.params.unwrap_or(serde_json::Value::Null));
            let attached = match params {
                Ok(p) => crate::attach::attach(&daemon, p).await,
                Err(e) => Err(RpcError::invalid_params(e)),
            };
            match attached {
                Ok(a) => {
                    let info = serde_json::to_value(a.info()).unwrap_or_default();
                    if write_message(&mut wr, &Response::ok(req.id, info)).await.is_err() {
                        return;
                    }
                    if let Err(e) = crate::attach::stream(&daemon, a, &mut rd, &mut wr).await {
                        debug!(error = %e, "viewer stream ended");
                    }
                    return;
                }
                Err(e) => {
                    if write_message(&mut wr, &Response::err(req.id, e)).await.is_err() {
                        return;
                    }
                    continue;
                }
            }
        }
        let res = match router.dispatch(daemon.clone(), &req.method, req.params).await {
            Ok(v) => Response::ok(req.id, v),
            Err(e) => Response::err(req.id, e),
        };
        if write_message(&mut wr, &res).await.is_err() {
            return;
        }
        if daemon.shutdown.is_cancelled() {
            return;
        }
    }
}
