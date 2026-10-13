//! Socket server: bind `rld.sock`, accept, one task per connection, dispatch
//! through the router. Shutdown is cooperative; it waits briefly for in-flight
//! connections so the `daemon.shutdown` reply gets out.

use crate::router::Router;
use crate::supervisor::{self, Supervisor};
use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rl_core::Home;
use rl_detect::Registry;
use rl_ledger::Ledger;
use rl_proto::codec::{read_message, write_message};
use rl_proto::methods::{AttachParams, Method, RunAttach};
use rl_proto::rpc::{Id, Request, Response, RpcError};
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
    pub registry: Arc<Registry>,
    /// `config.yaml` as last read; `config.get` re-reads it.
    /// Shared with the daily sweep (`gc`).
    pub config: Arc<std::sync::RwLock<crate::config::Loaded>>,
    pub supervisor: Arc<Supervisor>,
    pub decisions: Arc<crate::decisions::Decisions>,
    pub flows: Arc<crate::flows::Flows>,
    /// What the configured badges last said of each folder's branch.
    pub badges: crate::badges::Badges,
    /// Agents' own sessions, as last heard.
    pub outside: Arc<crate::outside::Outsiders>,
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
    /// The `rl` a flow's steps find first on their `PATH`, linked into `run/bin/`; defaults to
    /// the one beside `rld`, and with none there a step's `PATH` is left as it was.
    pub step_rl: Option<PathBuf>,
    /// Detection registry; defaults to the built-ins merged with `$RELAY_HOME/detect/`.
    pub registry: Option<Arc<Registry>>,
    /// How agents that list their own sessions (Claude's) are asked for
    /// them; none, and none is (`RELAY_CLAUDE_SESSIONS=off`).
    pub claude_sessions: Option<crate::outside::Polling>,
}

pub async fn serve(home: Home, shutdown: CancellationToken) -> Result<()> {
    let opts = ServeOptions {
        claude_sessions: crate::outside::Polling::from_env(),
        ..Default::default()
    };
    serve_with(home, shutdown, opts).await
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
    // Refreshed at each start, so a step's `rl` is always the one beside the `rld` running it.
    let step_rl = opts.step_rl.or_else(crate::flows::path::beside_rld);
    let bin = crate::flows::path::link_rl(&home, step_rl.as_deref());
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
    match rl_detect::claude::write_settings(&home, &rl_bin) {
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
    match crate::config::seed(&home) {
        Ok(true) => info!(file = %home.config_file().display(), "config.yaml seeded"),
        Ok(false) => {}
        Err(e) => warn!(error = %format!("{e:#}"), "config.yaml not seeded"),
    }
    if let Err(e) = crate::badges::seed(&home) {
        warn!(error = %format!("{e:#}"), "badges not seeded");
    }
    let config = crate::config::load(&home);
    match &config.error {
        Some(e) => warn!(error = %e, "config.yaml not usable; running on defaults"),
        None if config.present => info!(file = %config.file.display(), "config read"),
        None => {}
    }
    let config = Arc::new(std::sync::RwLock::new(config));
    tokio::spawn(crate::gc::daily(
        home.clone(),
        ledger.clone(),
        config.clone(),
        shutdown.child_token(),
    ));
    let decisions = Arc::new(crate::decisions::Decisions::new(
        ledger.clone(),
        supervisor.clone(),
        registry.clone(),
        shutdown.child_token(),
    ));
    tokio::spawn(decisions.clone().serve());
    let flows = crate::flows::Flows::new(
        home.clone(),
        ledger.clone(),
        supervisor.clone(),
        decisions.clone(),
        registry.clone(),
        bin,
        shutdown.child_token(),
    );
    // Loaded before the socket answers, so `rl flow list` right after
    // `rl daemon start` already knows every flow.
    if let Err(e) = flows.reload().await {
        warn!(error = %format!("{e:#}"), "flows not loaded");
    }
    // Where the loop starts reading, settled before anything can fire.
    if let Err(e) = flows.cursor().await {
        warn!(error = %format!("{e:#}"), "flows: events cursor");
    }
    // Firings carried on from what happened while Relay was away, the runs the
    // supervisor found ended or lost among it, before anything can fire.
    if let Err(e) = flows.reconcile().await {
        warn!(error = %format!("{e:#}"), "flows: carrying firings on");
    }
    tokio::spawn(flows.clone().serve());
    // Served whether or not any agent's list is asked for: every agent's
    // sessions are told by its hooks.
    let outside = Arc::new(crate::outside::Outsiders::new(registry.clone(), opts.claude_sessions));
    tokio::spawn(outside.clone().serve(ledger.clone(), shutdown.child_token()));
    let daemon = Arc::new(Daemon {
        home: home.clone(),
        ledger,
        registry,
        config: config.clone(),
        supervisor,
        decisions,
        flows,
        badges: crate::badges::Badges::new(home.plugins_dir(), crate::badges::EVERY, crate::badges::WAIT),
        outside,
        started_at: Timestamp::now(),
        shutdown: shutdown.clone(),
    });
    tokio::spawn(crate::recap::sweep(daemon.clone()));
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
                // Can't reply with an id we couldn't parse. `Id` has no null
                // variant (it's `Num(u64) | Str(String)`), so `Num(0)` stands
                // in; a client that itself numbers requests from 0 can't tell
                // this apart from a genuine reply to its request 0.
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
