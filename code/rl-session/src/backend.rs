//! Spawning the child: a PTY by default, plain pipes with `--no-tty`. Both
//! produce the same [`Spawned`] handle so the holder never cares which.
//!
//! The PTY and pipe readers are blocking, so each lives on its own thread and
//! hands chunks to the async side over a channel.

use crate::exit::{Exit, signal_name};
use anyhow::{Context, Result};
use jiff::Timestamp;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

const CHUNK: usize = 8192;
/// Output chunks buffered between the reader thread and the holder.
const OUTPUT_QUEUE: usize = 256;

#[derive(Debug, Clone)]
pub struct Spec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    pub tty: bool,
}

pub type Resizer = Arc<dyn Fn(u16, u16) -> Result<()> + Send + Sync>;

pub struct Spawned {
    pub pid: u32,
    pub output: mpsc::Receiver<Vec<u8>>,
    pub input: mpsc::Sender<Vec<u8>>,
    pub exit: oneshot::Receiver<Exit>,
    pub resize: Resizer,
}

pub fn spawn(spec: &Spec) -> Result<Spawned> {
    if spec.tty { spawn_pty(spec) } else { spawn_pipes(spec) }
}

fn spawn_pty(spec: &Spec) -> Result<Spawned> {
    let pty = native_pty_system();
    let size = PtySize {
        rows: spec.rows,
        cols: spec.cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = pty.openpty(size).context("openpty")?;

    let mut cmd = CommandBuilder::new(&spec.argv[0]);
    cmd.args(&spec.argv[1..]);
    cmd.cwd(&spec.cwd);
    if cmd.get_env("TERM").is_none() {
        cmd.env("TERM", "xterm-256color");
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("spawning {:?}", spec.argv))?;
    drop(pair.slave);
    let pid = child.process_id().context("child has no pid")?;

    let mut reader = pair.master.try_clone_reader().context("pty reader")?;
    let mut writer = pair.master.take_writer().context("pty writer")?;
    let master: Arc<Mutex<Box<dyn MasterPty + Send>>> = Arc::new(Mutex::new(pair.master));

    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(OUTPUT_QUEUE);
    std::thread::Builder::new().name("pty-read".into()).spawn(move || {
        let mut buf = vec![0u8; CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                // EIO on the master is the normal "slave closed" on Linux/macOS.
                Err(e) => {
                    debug!(error = %e, "pty read ended");
                    break;
                }
            }
        }
    })?;

    let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(OUTPUT_QUEUE);
    std::thread::Builder::new().name("pty-write".into()).spawn(move || {
        while let Some(bytes) = in_rx.blocking_recv() {
            if let Err(e) = writer.write_all(&bytes).and_then(|_| writer.flush()) {
                warn!(error = %e, "pty write failed");
                break;
            }
        }
    })?;

    let (exit_tx, exit_rx) = oneshot::channel();
    std::thread::Builder::new().name("pty-wait".into()).spawn(move || {
        let exit = match child.wait() {
            Ok(st) => match st.signal().map(pty_signal_number) {
                // portable-pty renders signals as "Terminated: 15"; normalise
                // to the same shape the pipe backend produces.
                Some(Some(sig)) => Exit {
                    code: 128 + sig,
                    signal: Some(signal_name(sig)),
                    at: Timestamp::now(),
                },
                Some(None) => Exit {
                    code: 128,
                    signal: st.signal().map(String::from),
                    at: Timestamp::now(),
                },
                None => Exit {
                    code: st.exit_code() as i32,
                    signal: None,
                    at: Timestamp::now(),
                },
            },
            Err(e) => {
                warn!(error = %e, "wait failed");
                Exit {
                    code: -1,
                    signal: None,
                    at: Timestamp::now(),
                }
            }
        };
        let _ = exit_tx.send(exit);
    })?;

    let resize: Resizer = Arc::new(move |cols, rows| {
        master
            .lock()
            .map_err(|_| anyhow::anyhow!("pty poisoned"))?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("resize")
    });

    Ok(Spawned {
        pid,
        output: out_rx,
        input: in_tx,
        exit: exit_rx,
        resize,
    })
}

/// `"Terminated: 15"` → `15`.
fn pty_signal_number(s: &str) -> Option<i32> {
    s.rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|n| n.parse().ok())
}

fn spawn_pipes(spec: &Spec) -> Result<Spawned> {
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(&spec.argv[0]);
    cmd.args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("spawning {:?}", spec.argv))?;
    let pid = child.id();
    let mut stdin = child.stdin.take().context("stdin")?;
    let stdout = child.stdout.take().context("stdout")?;
    let stderr = child.stderr.take().context("stderr")?;

    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(OUTPUT_QUEUE);
    for (name, mut r) in [
        ("stdout", Box::new(stdout) as Box<dyn Read + Send>),
        ("stderr", Box::new(stderr)),
    ] {
        let tx = out_tx.clone();
        std::thread::Builder::new()
            .name(format!("pipe-{name}"))
            .spawn(move || {
                let mut buf = vec![0u8; CHUNK];
                loop {
                    match r.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.blocking_send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })?;
    }
    drop(out_tx);

    let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(OUTPUT_QUEUE);
    std::thread::Builder::new().name("pipe-write".into()).spawn(move || {
        while let Some(bytes) = in_rx.blocking_recv() {
            if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                break;
            }
        }
    })?;

    let (exit_tx, exit_rx) = oneshot::channel();
    std::thread::Builder::new().name("pipe-wait".into()).spawn(move || {
        use std::os::unix::process::ExitStatusExt;
        let exit = match child.wait() {
            Ok(st) => match (st.code(), st.signal()) {
                (Some(code), _) => Exit {
                    code,
                    signal: None,
                    at: Timestamp::now(),
                },
                (None, Some(sig)) => Exit {
                    code: 128 + sig,
                    signal: Some(signal_name(sig)),
                    at: Timestamp::now(),
                },
                (None, None) => Exit {
                    code: -1,
                    signal: None,
                    at: Timestamp::now(),
                },
            },
            Err(_) => Exit {
                code: -1,
                signal: None,
                at: Timestamp::now(),
            },
        };
        let _ = exit_tx.send(exit);
    })?;

    let resize: Resizer = Arc::new(|_, _| Ok(()));
    Ok(Spawned {
        pid,
        output: out_rx,
        input: in_tx,
        exit: exit_rx,
        resize,
    })
}
