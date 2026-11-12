//! A command run under a pty of the panel's, this terminal passed through to
//! it: how `claude attach` gets the screen, so `ctrl+\` lets go of it as it
//! does of a joined run. Keys are bytes, never parsed, as for `rl join`; the
//! command's screen is its own, and only the window title is ours.
//!
//! Letting go hangs up its pty, as closing a terminal window does: the
//! command ends, and what it was attached to goes on without it. Its screen
//! is never read: what it draws is passed through, as it drew it.

use super::{AbortOnDrop, AltScreen, DETACH, RawMode, title};
use anyhow::{Context, Result};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use rl_session::backend::{self, Spawned, Spec};
use rl_session::exit::Exit;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

/// How long a hung-up command has to end before it is killed.
const GRACE: Duration = Duration::from_secs(2);

/// How long output still on its way is waited for, once the command ended.
const LAST_WORDS: Duration = Duration::from_millis(200);

/// How long what was typed just before a `ctrl+\` has to be read.
const TYPED_FIRST: Duration = Duration::from_millis(20);

/// How the terminal came back.
#[derive(Debug, PartialEq)]
pub enum Back {
    /// `ctrl+\`: let go of; what it was attached to goes on.
    Detached,
    /// It ended by itself, and how.
    Ended(Exit),
}

/// Starts `argv` on a pty of `size` (columns, rows), in `cwd` (else here),
/// with the panel's environment.
pub fn start(argv: &[String], cwd: Option<&Path>, size: (u16, u16)) -> Result<Spawned> {
    backend::spawn(&Spec {
        argv: argv.to_vec(),
        cwd: match cwd {
            Some(dir) => dir.to_path_buf(),
            None => std::env::current_dir().context("this directory")?,
        },
        env: Vec::new(),
        inherit_env: true,
        cols: size.0,
        rows: size.1,
        settle: Duration::ZERO,
    })
}

/// Runs `argv`, by `name` (`claude attach a1`), with this terminal passed
/// through, until it ends or is let go of. A banner line says how to let
/// go, and the window title says it while it has the screen; the terminal
/// is handed back as it was found.
///
/// The terminal is raw before anything starts and until it is handed back,
/// so a ^C or ^\ typed meanwhile is a key, never a signal to the panel.
pub async fn hold(argv: &[String], cwd: Option<&Path>, name: &str) -> Result<Back> {
    use tokio::signal::unix::{SignalKind, signal};
    // Listened for before the size is read, so no resize falls between.
    let mut winch = signal(SignalKind::window_change())?;
    let mut stop = super::Stop::new()?;
    let _raw = RawMode::enable().context("entering raw mode")?;
    let mut out = tokio::io::stdout();
    let banner = format!("\x1b[2m[relay] {name} · ctrl+\\ to detach\x1b[0m\r\n");
    out.write_all(format!("{banner}{}", title(&format!("relay · {name} · ^\\ lets go"))).as_bytes())
        .await?;
    out.flush().await?;
    let size = crossterm::terminal::size().unwrap_or((80, 24));
    let spawned = start(argv, cwd, size)?;

    let (keys_tx, keys) = mpsc::channel::<Vec<u8>>(64);
    let stop_keys = keys_tx.clone();
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
        while winch.recv().await.is_some() {
            if let Ok(size) = crossterm::terminal::size()
                && sizes_tx.send(size).await.is_err()
            {
                break;
            }
        }
    }));

    // A kill or a closed terminal lets go as a `ctrl+\` does: the command is
    // hung up on, and the raw mode and screen below are given back. The list
    // that called hears the same signal and stops.
    let _stop = AbortOnDrop(tokio::spawn({
        async move {
            stop.recv().await;
            let _ = stop_keys.send(vec![DETACH]).await;
        }
    }));
    let mut alt = AltScreen::default();
    let back = pass(spawned, keys, sizes, &mut out, &mut alt).await;
    // However it went, the terminal is handed back: what it had on when it
    // was let go of was never turned off by it, keyboard protocols too.
    let _ = out.write_all(&alt.leave()).await;
    let _ = out.write_all(KEYBOARD_BACK).await;
    let _ = out.write_all(title("").as_bytes()).await;
    let _ = out.flush().await;
    back
}

/// Turns off what a command may have asked of the terminal's keyboard and
/// that `AltScreen::leave` does not: kitty's keyboard flags, xterm's
/// modifyOtherKeys, synchronized output. A terminal without them ignores it.
const KEYBOARD_BACK: &[u8] = b"\x1b[<99u\x1b[>4;0m\x1b[?2026l";

/// How the passing through went, before the hang-up.
enum Went {
    Ended(Exit),
    Detached,
    /// Writing to the terminal failed, or it would not say how it ended.
    Failed(anyhow::Error),
}

/// The passing through: what `sp` writes goes to `out`, watched by `alt`;
/// `keys` go to it up to a `ctrl+\`, which lets go; `sizes` resize its pty.
/// However it comes back, it has ended: let go of, or gone wrong here, it
/// is hung up on.
pub async fn pass(
    mut sp: Spawned,
    keys: mpsc::Receiver<Vec<u8>>,
    sizes: mpsc::Receiver<(u16, u16)>,
    out: &mut (impl AsyncWrite + Unpin),
    alt: &mut AltScreen,
) -> Result<Back> {
    match through(&mut sp, keys, sizes, out, alt).await {
        Went::Ended(exit) => Ok(Back::Ended(exit)),
        Went::Detached => {
            hang_up(sp.pid, &mut sp.exit).await;
            Ok(Back::Detached)
        }
        Went::Failed(e) => {
            hang_up(sp.pid, &mut sp.exit).await;
            Err(e)
        }
    }
}

async fn through(
    sp: &mut Spawned,
    mut keys: mpsc::Receiver<Vec<u8>>,
    mut sizes: mpsc::Receiver<(u16, u16)>,
    out: &mut (impl AsyncWrite + Unpin),
    alt: &mut AltScreen,
) -> Went {
    let mut keys_open = true;
    let mut sizes_open = true;
    loop {
        tokio::select! {
            chunk = sp.output.recv() => match chunk {
                Some(b) => {
                    alt.watch(&b);
                    if let Err(e) = write(out, &b).await {
                        return Went::Failed(e.into());
                    }
                }
                // Its pty closed: it has ended, or is about to. One that
                // goes on without its terminal is not waited for.
                None => {
                    return match tokio::time::timeout(GRACE, &mut sp.exit).await {
                        Ok(Ok(exit)) => Went::Ended(exit),
                        Ok(Err(e)) => Went::Failed(anyhow::Error::new(e).context("how it ended")),
                        Err(_) => Went::Failed(anyhow::anyhow!("its terminal closed and it went on")),
                    };
                }
            },
            exit = &mut sp.exit => {
                // What it wrote last may still be on its way; only a moment
                // of it, whatever keeps writing.
                let until = tokio::time::Instant::now() + LAST_WORDS;
                while let Ok(Some(b)) = tokio::time::timeout_at(until, sp.output.recv()).await {
                    alt.watch(&b);
                    let _ = out.write_all(&b).await;
                }
                let _ = out.flush().await;
                return match exit {
                    Ok(exit) => Went::Ended(exit),
                    Err(e) => Went::Failed(anyhow::Error::new(e).context("how it ended")),
                };
            }
            k = keys.recv(), if keys_open => match k {
                None => keys_open = false,
                Some(b) => match b.iter().position(|&c| c == DETACH) {
                    Some(at) => {
                        if at > 0 {
                            let _ = sp.input.send(b[..at].to_vec()).await;
                            // A moment for it to read what came before.
                            tokio::time::sleep(TYPED_FIRST).await;
                        }
                        return Went::Detached;
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

async fn write(out: &mut (impl AsyncWrite + Unpin), b: &[u8]) -> std::io::Result<()> {
    out.write_all(b).await?;
    out.flush().await
}

/// Hangs up on `pid`'s process group, as a closed terminal does, and waits
/// for it to end; one that will not in `GRACE` is killed. One that has
/// already ended is left be: its pid may be someone else's by now.
async fn hang_up(pid: u32, exit: &mut oneshot::Receiver<Exit>) {
    use tokio::sync::oneshot::error::TryRecvError;
    if !matches!(exit.try_recv(), Err(TryRecvError::Empty)) {
        return;
    }
    // The pty made it the leader of its own session and group. A stopped
    // group hears a hang-up only once it goes on, as the kernel's does.
    let group = Pid::from_raw(pid as i32);
    let _ = killpg(group, Signal::SIGHUP);
    let _ = killpg(group, Signal::SIGCONT);
    if tokio::time::timeout(GRACE, &mut *exit).await.is_err() {
        let _ = killpg(group, Signal::SIGKILL);
        let _ = exit.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Spawned {
        start(&["/bin/sh".into(), "-c".into(), script.into()], None, (80, 24)).unwrap()
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
        let (out, back) = through(sh("read line; stty size"), &[b"\r"], &[(100, 40)]).await;
        assert!(out.contains("40 100"), "{out}");
        assert!(matches!(back, Back::Ended(Exit { code: 0, .. })), "{back:?}");
    }

    #[test]
    fn one_not_there_will_not_start() {
        assert!(start(&["/nowhere/claude".into(), "attach".into()], None, (80, 24)).is_err());
    }
}
