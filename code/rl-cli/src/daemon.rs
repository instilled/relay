//! `rl daemon start|stop|status|restart`, and the daemon every other command
//! gets without asking: `ensure` starts `rld` when it is not there, and says
//! so when the one that is there is not the same version as this `rl`.
//!
//! `rld` is spawned detached — own process group, stdio to `log/rld.log` — so it
//! outlives the shell that started it. Session holders are separate processes
//! again, which is why restart here is always safe for running work.

use crate::client::{Client, NotRunning};
use anyhow::{Context, Result, bail};
use rl_core::Home;
use rl_proto::methods::{DaemonInfo, DaemonPing, DaemonShutdown};
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
    cmd.env(rl_core::HOME_ENV, home.path())
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

/// How the running daemon compares with this `rl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Same,
    /// Same wire protocol, another release: works, but a restart is due.
    Version {
        theirs: String,
        mine: String,
    },
    /// Another wire protocol: messages could be misread; refuse.
    Protocol {
        theirs: u32,
        mine: u32,
    },
}

pub fn check(info: &DaemonInfo) -> Check {
    let mine = env!("CARGO_PKG_VERSION");
    if info.protocol != rl_proto::PROTOCOL_VERSION {
        Check::Protocol {
            theirs: info.protocol,
            mine: rl_proto::PROTOCOL_VERSION,
        }
    } else if info.version != mine {
        Check::Version {
            theirs: info.version.clone(),
            mine: mine.to_string(),
        }
    } else {
        Check::Same
    }
}

/// A connection to a daemon that is there and current: started if it was
/// not running (one line on stderr says so), and when it is another release
/// than this `rl`, offered a restart — asked on a terminal when `ask`, only
/// noted otherwise. Another protocol is refused: `rl daemon restart` is the
/// way out, and nothing running is lost by it.
pub async fn ensure(home: &Home, ask: bool) -> Result<Client> {
    let (info, started) = start(home).await?;
    if started {
        eprintln!("rld started");
    }
    match check(&info) {
        Check::Same => {}
        Check::Protocol { theirs, mine } => bail!(
            "rld {} speaks protocol {theirs}, this rl {} speaks {mine}: `rl daemon restart` (runs survive it)",
            info.version,
            env!("CARGO_PKG_VERSION")
        ),
        Check::Version { theirs, mine } => {
            let tty = std::io::IsTerminal::is_terminal(&std::io::stdin())
                && std::io::IsTerminal::is_terminal(&std::io::stderr());
            if ask && tty {
                eprint!("rld {theirs} is running, rl is {mine}: restart it? runs survive. [y/N] ");
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                if line.trim().eq_ignore_ascii_case("y") {
                    restart(home).await?;
                    eprintln!("rld restarted");
                }
            } else {
                eprintln!("note: rld {theirs} is running, rl is {mine}; `rl daemon restart` when convenient");
            }
        }
    }
    Client::connect(home).await
}

/// `$RELAY_RLD_BIN`, else `rld` next to this executable, else `rld` on PATH.
fn locate_rld() -> Result<PathBuf> {
    Ok(rl_core::locate_bin("rld", RLD_BIN_ENV))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_reads_protocol_before_version() {
        let mine = env!("CARGO_PKG_VERSION").to_string();
        let info = |protocol: u32, version: &str| DaemonInfo {
            protocol,
            version: version.into(),
            pid: 1,
            started_at: jiff::Timestamp::UNIX_EPOCH,
            home: "/h".into(),
        };
        assert_eq!(check(&info(rl_proto::PROTOCOL_VERSION, &mine)), Check::Same);
        assert_eq!(
            check(&info(rl_proto::PROTOCOL_VERSION, "0.0.1")),
            Check::Version {
                theirs: "0.0.1".into(),
                mine: mine.clone()
            }
        );
        assert_eq!(
            check(&info(rl_proto::PROTOCOL_VERSION + 1, &mine)),
            Check::Protocol {
                theirs: rl_proto::PROTOCOL_VERSION + 1,
                mine: rl_proto::PROTOCOL_VERSION
            }
        );
    }
}
