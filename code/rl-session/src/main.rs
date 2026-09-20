//! `rl-session`: spawned by `rld`, never by hand.
//!
//! Detaches by forking: the child calls `setsid`, binds the socket and starts
//! the command; the parent waits for a one-line readiness message on a pipe
//! and exits 0 — so the spawner knows, synchronously, that the socket is live.

use anyhow::{Context, Result, bail};
use clap::Parser;
use nix::sys::signal::{SigHandler, Signal, signal};
use nix::unistd::{ForkResult, dup2_stdin, dup2_stdout, fork, pipe, setsid};
use rl_session::backend::Spec;
use rl_session::holder::{self, Config};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "rl-session", version, about = "Relay session holder (internal)")]
struct Args {
    /// Run id this session belongs to.
    #[arg(long)]
    id: String,
    /// Unix socket to serve on.
    #[arg(long)]
    socket: PathBuf,
    /// Where to record the child's exit (JSON), for a daemon that was away.
    #[arg(long)]
    exit_file: Option<PathBuf>,
    /// Working directory for the child.
    #[arg(long, default_value = ".")]
    cwd: PathBuf,
    #[arg(long, default_value_t = 80)]
    cols: u16,
    #[arg(long, default_value_t = 24)]
    rows: u16,
    /// Extra environment for the child, KEY=VALUE (repeatable).
    #[arg(long = "env", value_name = "KEY=VALUE")]
    env: Vec<String>,
    /// The child's whole environment, a JSON array of [KEY, VALUE] pairs; the
    /// holder's own is not inherited. `--env` still adds to it.
    #[arg(long, value_name = "FILE")]
    env_file: Option<PathBuf>,
    /// Pipes instead of a pty (headless/scheduled runs).
    #[arg(long)]
    no_tty: bool,
    /// No input for the child: end of input at once (pipes only; a pty
    /// always has input).
    #[arg(long)]
    no_input: bool,
    /// Seconds to stay around after the child exits with no client attached.
    #[arg(long, default_value_t = 86_400)]
    linger: u64,
    /// Seconds with nobody attached — no daemon, no viewer — after which the
    /// holder hangs up its child and leaves. 0 waits forever.
    #[arg(long, default_value_t = 0)]
    reap_after: u64,
    /// Milliseconds a reader that has caught up with the child waits before
    /// reading again, so a burst arrives as one chunk. 0 reads as fast as the
    /// child writes.
    #[arg(long, default_value_t = 5)]
    settle: u64,
    /// Seconds between checks that the socket is still there; gone, the
    /// holder leaves. For tests.
    #[arg(long, default_value_t = 60, hide = true)]
    socket_check: u64,
    /// Do not detach; log to this terminal. For debugging.
    #[arg(long)]
    foreground: bool,
    /// The command to run.
    #[arg(last = true, required = true)]
    argv: Vec<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let cwd = std::fs::canonicalize(&args.cwd).with_context(|| format!("cwd {}", args.cwd.display()))?;
    let mut env: Vec<(String, String)> = match &args.env_file {
        Some(f) => {
            let bytes = std::fs::read(f).with_context(|| format!("reading {}", f.display()))?;
            serde_json::from_slice(&bytes).with_context(|| format!("{}: expected [[key, value], …]", f.display()))?
        }
        None => Vec::new(),
    };
    for kv in &args.env {
        let (k, v) = kv
            .split_once('=')
            .with_context(|| format!("--env {kv}: expected KEY=VALUE"))?;
        env.push((k.to_string(), v.to_string()));
    }
    let cfg = Config {
        id: args.id,
        socket: args.socket,
        exit_file: args.exit_file,
        spec: Spec {
            argv: args.argv,
            cwd,
            env,
            inherit_env: args.env_file.is_none(),
            cols: args.cols,
            rows: args.rows,
            tty: !args.no_tty,
            input: !args.no_input,
            settle: Duration::from_millis(args.settle),
        },
        linger: Duration::from_secs(args.linger),
        socket_check: Duration::from_secs(args.socket_check.max(1)),
        reap_after: (args.reap_after > 0).then(|| Duration::from_secs(args.reap_after)),
    };

    if args.foreground {
        init_logging();
        return run(cfg, None);
    }

    let (ready_r, ready_w) = pipe().context("readiness pipe")?;
    // Safe: single-threaded here; the child re-initialises everything it uses.
    match unsafe { fork() }.context("fork")? {
        ForkResult::Parent { .. } => {
            drop(ready_w);
            let mut line = String::new();
            BufReader::new(std::fs::File::from(ready_r)).read_line(&mut line)?;
            if line.trim() == "ok" {
                Ok(())
            } else {
                let msg = line
                    .trim()
                    .strip_prefix("err: ")
                    .unwrap_or("holder exited before becoming ready");
                bail!("{msg}");
            }
        }
        ForkResult::Child => {
            drop(ready_r);
            setsid().context("setsid")?;
            // Safe: replacing the disposition of a signal we never rely on.
            unsafe { signal(Signal::SIGHUP, SigHandler::SigIgn) }.context("ignore SIGHUP")?;
            // Only stderr stays (the spawner points it at a log). Holding the
            // spawner's stdout would keep its pipes open forever.
            let devnull = std::fs::File::options().read(true).write(true).open("/dev/null")?;
            dup2_stdin(&devnull).context("stdin -> /dev/null")?;
            dup2_stdout(&devnull).context("stdout -> /dev/null")?;
            // Don't pin any directory (a worktree may be removed under us).
            let _ = std::env::set_current_dir("/");
            init_logging();
            run(cfg, Some(std::fs::File::from(ready_w)))
        }
    }
}

fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_writer(std::io::stderr)
        .init();
}

fn run(cfg: Config, mut ready_pipe: Option<std::fs::File>) -> Result<()> {
    // One child, one socket, a handful of watchers: all of it is waiting on
    // I/O, and none of it is work to spread. A worker per core only meant a
    // cross-thread wake-up and a trip through the work-stealing scheduler for
    // every chunk the child wrote, which cost the holder more than relaying
    // the bytes did — and left sixteen parked threads behind at rest.
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async move {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let mut task = tokio::spawn(holder::serve(cfg, ready_tx));
        let outcome = tokio::select! {
            r = ready_rx => r.map_err(|_| ()),
            done = &mut task => {
                // Failed before ready.
                let err = match done {
                    Ok(Err(e)) => e,
                    Ok(Ok(())) => anyhow::anyhow!("holder exited before becoming ready"),
                    Err(e) => anyhow::anyhow!("holder panicked: {e}"),
                };
                if let Some(p) = ready_pipe.as_mut() {
                    let _ = writeln!(p, "err: {err:#}");
                }
                return Err(err);
            }
        };
        if outcome.is_err() {
            // Sender dropped without sending: serve() returned early.
            let err = match task.await {
                Ok(Err(e)) => e,
                _ => anyhow::anyhow!("holder exited before becoming ready"),
            };
            if let Some(p) = ready_pipe.as_mut() {
                let _ = writeln!(p, "err: {err:#}");
            }
            return Err(err);
        }
        if let Some(mut p) = ready_pipe.take() {
            let _ = writeln!(p, "ok");
        }
        task.await.context("holder task")?
    })
}
