//! `rl daemon start|stop|status|restart`.
//!
//! `rld` is spawned detached — own process group, stdio to `log/rld.log` — so it
//! outlives the shell that started it. Session holders are separate processes
//! again, which is why restart here is always safe for running work.

use crate::client::{Client, NotRunning};
use anyhow::{Context, Result, bail};
use relay_core::Home;
use relay_proto::methods::{DaemonInfo, DaemonPing, DaemonShutdown};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// Env override for the daemon binary; used by tests and `cargo run` setups.
pub const RLD_BIN_ENV: &str = "RELAY_RLD_BIN";

pub async fn status(home: &Home) -> Result<Option<DaemonInfo>> {
    match Client::connect(home).await {
        Ok(mut c) => Ok(Some(c.call::<DaemonPing>(()).await?)),
        Err(e) if e.downcast_ref::<NotRunning>().is_some() => Ok(None),
        Err(e) => Err(e),
    }
}

/// Returns the daemon info and whether this call started it.
pub async fn start(home: &Home) -> Result<(DaemonInfo, bool)> {
    if let Some(info) = status(home).await? {
        return Ok((info, false));
    }
    home.ensure_dirs()?;
    let bin = locate_rld()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.daemon_log())
        .with_context(|| format!("opening {}", home.daemon_log().display()))?;

    let mut cmd = std::process::Command::new(&bin);
    cmd.env(relay_core::HOME_ENV, home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().with_context(|| format!("spawning {}", bin.display()))?;
    // Not waited on: rld is meant to outlive us. Reaping is the init's job.
    std::mem::forget(child);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(info) = status(home).await? {
            return Ok((info, true));
        }
        if Instant::now() > deadline {
            bail!("rld did not come up within 5s; see {}", home.daemon_log().display());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Returns whether a daemon was running to stop.
pub async fn stop(home: &Home) -> Result<bool> {
    let mut c = match Client::connect(home).await {
        Ok(c) => c,
        Err(e) if e.downcast_ref::<NotRunning>().is_some() => return Ok(false),
        Err(e) => return Err(e),
    };
    c.call::<DaemonShutdown>(()).await?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while home.daemon_socket().exists() {
        if Instant::now() > deadline {
            bail!("rld acknowledged shutdown but the socket is still there after 5s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(true)
}

pub async fn restart(home: &Home) -> Result<DaemonInfo> {
    stop(home).await?;
    Ok(start(home).await?.0)
}

/// `$RELAY_RLD_BIN`, else `rld` next to this executable, else `rld` on PATH.
fn locate_rld() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os(RLD_BIN_ENV) {
        return Ok(PathBuf::from(p));
    }
    if let Ok(me) = std::env::current_exe() {
        let sibling = me.with_file_name("rld");
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    Ok(PathBuf::from("rld"))
}
