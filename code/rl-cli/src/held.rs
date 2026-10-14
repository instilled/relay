//! A command run under a pty of the panel's, this terminal passed through to
//! it: how `claude attach` gets the screen, so `ctrl-\` lets go of it as it
//! does of a joined run. Keys are bytes, never parsed, as for `rl join`; the
//! command's screen is its own, and only the window title is ours.
//!
//! Letting go hangs up its pty, as closing a terminal window does: the
//! command ends, and what it was attached to goes on without it.

use crate::attach::{AbortOnDrop, AltScreen, DETACH, RawMode, title};
use anyhow::{Context, Result};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use rl_session::backend::{self, Spawned, Spec};
use rl_session::exit::Exit;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

/// How long a hung-up command has to end before it is killed.
const GRACE: Duration = Duration::from_secs(2);

/// How long output still on its way is waited for, once the command ended.
const LAST_WORDS: Duration = Duration::from_millis(200);

/// How the terminal came back.
#[derive(Debug, PartialEq)]
pub enum Back {
    /// `ctrl-\`: let go of; what it was attached to goes on.
    Detached,
    /// It ended by itself, and how.
    Ended(Exit),
}

/// Starts `argv` on a pty of `size` (columns, rows), here, with the panel's
/// environment.
pub fn start(argv: &[String], size: (u16, u16)) -> Result<Spawned> {
    backend::spawn(&Spec {
        argv: argv.to_vec(),
        cwd: std::env::current_dir().context("this directory")?,
        env: Vec::new(),
        inherit_env: true,
        cols: size.0,
        rows: size.1,
        tty: true,
        input: true,
        settle: Duration::ZERO,
    })
}

/// Runs `argv`, by `name` (`claude attach a1`), with this terminal passed
/// through, until it ends or is let go of. A banner line says how to let
/// go, and the window title says it while it has the screen; the terminal
/// is handed back as it was found.
pub async fn hold(argv: &[String], name: &str) -> Result<Back> {
    let size = crossterm::terminal::size().unwrap_or((80, 24));
    let spawned = start(argv, size)?;
    let mut out = tokio::io::stdout();
    let banner = format!("\x1b[2m[relay] {name} · ctrl-\\ to detach\x1b[0m\r\n");
    out.write_all(format!("{banner}{}", title(&format!("relay · {name} · ^\\ lets go"))).as_bytes())
        .await?;
    out.flush().await?;
    let _raw = RawMode::enable().context("entering raw mode")?;

    let (keys_tx, keys) = mpsc::channel::<Vec<u8>>(64);
    // As in `attach()`: aborted on the way out, so it never races the
    // panel's own reader for stdin.
    let _keys = AbortOnDrop(tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = stdin.read(&mut buf).await {
            if keys_tx.send(buf[..n].to_vec()).await.is_err() {
                break;
            }
        }
    }));
    let (sizes_tx, sizes) = mpsc::channel::<(u16, u16)>(4);
    let _sizes = AbortOnDrop(tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut winch) = signal(SignalKind::window_change()) else {
            return;
        };
        while winch.recv().await.is_some() {
            if let Ok(size) = crossterm::terminal::size()
                && sizes_tx.send(size).await.is_err()
            {
                break;
            }
        }
    }));

    let mut alt = AltScreen::default();
    let back = pass(spawned, keys, sizes, &mut out, &mut alt).await;
    // However it went, the terminal is handed back.
    let _ = out.write_all(&alt.leave()).await;
    let _ = out.write_all(title("").as_bytes()).await;
    let _ = out.flush().await;
    back
}

/// The passing through: what `sp` writes goes to `out`, watched by `alt`;
/// `keys` go to it up to a `ctrl-\`, which lets go; `sizes` resize its pty.
pub async fn pass(
    mut sp: Spawned,
    mut keys: mpsc::Receiver<Vec<u8>>,
    mut sizes: mpsc::Receiver<(u16, u16)>,
    out: &mut (impl AsyncWrite + Unpin),
    alt: &mut AltScreen,
) -> Result<Back> {
    let mut keys_open = true;
    let mut sizes_open = true;
    loop {
        tokio::select! {
            chunk = sp.output.recv() => match chunk {
                Some(b) => {
                    alt.watch(&b);
                    out.write_all(&b).await?;
                    out.flush().await?;
                }
                // Its pty closed: it has ended, or is about to.
                None => return Ok(Back::Ended((&mut sp.exit).await.context("how it ended")?)),
            },
            exit = &mut sp.exit => {
                let exit = exit.context("how it ended")?;
                // What it wrote last may still be on its way.
                while let Ok(Some(b)) = tokio::time::timeout(LAST_WORDS, sp.output.recv()).await {
                    alt.watch(&b);
                    out.write_all(&b).await?;
                }
                out.flush().await?;
                return Ok(Back::Ended(exit));
            }
            k = keys.recv(), if keys_open => match k {
                None => keys_open = false,
                Some(b) => match b.iter().position(|&c| c == DETACH) {
                    Some(at) => {
                        if at > 0 {
                            let _ = sp.input.send(b[..at].to_vec()).await;
                        }
                        hang_up(sp.pid, &mut sp.exit).await;
                        return Ok(Back::Detached);
                    }
                    None => {
                        let _ = sp.input.send(b).await;
                    }
                },
            },
            size = sizes.recv(), if sizes_open => match size {
                Some((cols, rows)) => {
                    let _ = (sp.resize)(cols, rows);
                }
                None => sizes_open = false,
            },
        }
    }
}

/// Hangs up on `pid`'s process group, as a closed terminal does, and waits
/// for it to end; one that will not in `GRACE` is killed.
async fn hang_up(pid: u32, exit: &mut oneshot::Receiver<Exit>) {
    // The pty made it the leader of its own session and group.
    let group = Pid::from_raw(pid as i32);
    let _ = killpg(group, Signal::SIGHUP);
    if tokio::time::timeout(GRACE, &mut *exit).await.is_err() {
        let _ = killpg(group, Signal::SIGKILL);
        let _ = exit.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Spawned {
        start(&["/bin/sh".into(), "-c".into(), script.into()], (80, 24)).unwrap()
    }

    /// Passes `sp` through with `sizes` given, then `keys` typed, each after a
    /// moment so what it runs is ready for them; what it wrote, and how it
    /// came back.
    async fn through(sp: Spawned, keys: &[&[u8]], sizes: &[(u16, u16)]) -> (String, Back) {
        let (ktx, krx) = mpsc::channel(8);
        let (stx, srx) = mpsc::channel(8);
        let keys: Vec<Vec<u8>> = keys.iter().map(|k| k.to_vec()).collect();
        let sizes = sizes.to_vec();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            for s in sizes {
                stx.send(s).await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
            for k in keys {
                ktx.send(k).await.unwrap();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            // Held open, as a terminal's would be.
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop((ktx, stx));
        });
        let mut out = Vec::new();
        let back = tokio::time::timeout(
            Duration::from_secs(10),
            pass(sp, krx, srx, &mut out, &mut AltScreen::default()),
        )
        .await
        .expect("came back")
        .unwrap();
        (String::from_utf8_lossy(&out).into_owned(), back)
    }

    fn gone(pid: u32) -> bool {
        nix::sys::signal::kill(Pid::from_raw(pid as i32), None).is_err()
    }

    #[tokio::test]
    async fn one_that_ends_by_itself_says_how() {
        let (out, back) = through(sh("printf hello; exit 3"), &[], &[]).await;
        assert!(out.contains("hello"), "{out}");
        match back {
            Back::Ended(e) => assert_eq!(e.code, 3),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn keys_reach_it_and_ctrl_backslash_lets_go_of_it() {
        let sp = sh("read line; echo \"got $line\"; sleep 30");
        let pid = sp.pid;
        let (out, back) = through(sp, &[b"hi\r", b"\x1c"], &[]).await;
        assert_eq!(back, Back::Detached);
        assert!(out.contains("got hi"), "{out}");
        assert!(gone(pid), "hung up, it ended");
    }

    #[tokio::test]
    async fn one_that_will_not_hang_up_is_killed() {
        let sp = sh("trap '' HUP; sleep 30");
        let pid = sp.pid;
        let (_, back) = through(sp, &[b"\x1c"], &[]).await;
        assert_eq!(back, Back::Detached);
        assert!(gone(pid));
    }

    #[tokio::test]
    async fn a_new_size_reaches_its_pty() {
        let (out, _) = through(sh("read line; stty size"), &[b"\r"], &[(100, 40)]).await;
        assert!(out.contains("40 100"), "{out}");
    }

    #[test]
    fn one_not_there_will_not_start() {
        assert!(start(&["/nowhere/claude".into(), "attach".into()], (80, 24)).is_err());
    }
}
