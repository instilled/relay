//! Socket server: bind `rld.sock`, accept, one task per connection, dispatch
//! through the router. Shutdown is cooperative; it waits briefly for in-flight
//! connections so the `daemon.shutdown` reply gets out.

use crate::router::Router;
use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use relay_core::Home;
use relay_ledger::Ledger;
use relay_proto::codec::{read_message, write_message};
use relay_proto::rpc::{Id, Request, Response, RpcError};
use std::io::ErrorKind;
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
    pub started_at: Timestamp,
    pub shutdown: CancellationToken,
}

/// Runs until `daemon.shutdown` or `shutdown` is cancelled externally.
/// Removes the socket and pid file on the way out.
pub async fn serve(home: Home, shutdown: CancellationToken) -> Result<()> {
    home.ensure_dirs()?;
    let ledger = Ledger::open(&home.ledger())?;
    let sock_path = home.daemon_socket();
    let listener = bind(&sock_path)?;
    std::fs::write(home.daemon_pid(), std::process::id().to_string())?;
    info!(socket = %sock_path.display(), pid = std::process::id(), "rld listening");

    let daemon = Arc::new(Daemon {
        home: home.clone(),
        ledger,
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

    // Let in-flight replies (notably the shutdown ack) drain.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while conns.join_next().await.is_some() {}
    })
    .await;

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
