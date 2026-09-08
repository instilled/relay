//! Socket server: bind `run/rld.sock`, accept, one task per connection,
//! dispatch through the router. Shutdown is cooperative; it waits briefly for
//! in-flight connections so the `daemon.shutdown` reply gets out.

use crate::router::Router;
use anyhow::{Context, Result, bail};
use relay_core::Home;
use relay_proto::PROTOCOL_VERSION;
use relay_proto::codec::{read_message, write_message};
use relay_proto::methods::{DaemonInfo, DaemonPing, DaemonShutdown};
use relay_proto::rpc::{Request, Response, RpcError};
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Shared state every handler can reach.
pub struct Daemon {
    pub home: Home,
    pub started_at: SystemTime,
    pub shutdown: CancellationToken,
}

impl Daemon {
    fn info(&self) -> DaemonInfo {
        DaemonInfo {
            protocol: PROTOCOL_VERSION,
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
            started_at: rfc3339(self.started_at),
            home: self.home.path().to_path_buf(),
        }
    }
}

fn build_router() -> Router<Daemon> {
    let mut r: Router<Daemon> = Router::new();
    r.register::<DaemonPing, _, _>(|d, ()| async move { Ok(d.info()) });
    r.register::<DaemonShutdown, _, _>(|d, ()| async move {
        info!("shutdown requested");
        d.shutdown.cancel();
        Ok(())
    });
    r
}

/// Runs until `daemon.shutdown` or `shutdown` is cancelled externally.
/// Removes the socket and pid file on the way out.
pub async fn serve(home: Home, shutdown: CancellationToken) -> Result<()> {
    home.ensure_dirs()?;
    let sock_path = home.daemon_socket();
    let listener = bind(&sock_path)?;
    std::fs::write(home.daemon_pid(), std::process::id().to_string())?;
    info!(socket = %sock_path.display(), pid = std::process::id(), "rld listening");

    let daemon = Arc::new(Daemon {
        home: home.clone(),
        started_at: SystemTime::now(),
        shutdown: shutdown.clone(),
    });
    let router = Arc::new(build_router());
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
                let res = Response::err(
                    relay_proto::rpc::Id::Num(0),
                    RpcError::new(RpcError::PARSE, e.to_string()),
                );
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
        let res = match router
            .dispatch(daemon.clone(), &req.method, req.params)
            .await
        {
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

fn rfc3339(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Civil-from-days (Howard Hinnant); avoids pulling a date crate for one field.
    let days = (secs / 86_400) as i64;
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::rfc3339;
    use std::time::{Duration, SystemTime};

    #[test]
    fn rfc3339_known_instant() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_757_340_000);
        assert_eq!(rfc3339(t), "2025-09-08T14:00:00Z");
        assert_eq!(rfc3339(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }
}
