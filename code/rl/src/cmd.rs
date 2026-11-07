//! The `new | ls | stop | rm` commands.

use crate::spawn::{self, Start};
use crate::store::sessions::{self, Held, Status};
use anyhow::{Result, bail};
use jiff::Timestamp;
use rl_core::Home;
use rl_session::client::Session;
use rl_session::exit::read_exit_file;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const STOP_WAIT: Duration = Duration::from_secs(6);
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 32;

pub async fn new(
    home: &Home,
    harness: Option<String>,
    cwd: Option<PathBuf>,
    name_only: bool,
    argv: Vec<String>,
) -> Result<()> {
    let cwd = match cwd {
        Some(c) => c,
        None => std::env::current_dir()?,
    };
    let (cols, rows) = terminal_size();
    let meta = spawn::start(
        home,
        Start {
            harness,
            cwd,
            argv,
            cols,
            rows,
        },
    )
    .await?;
    println!("{}", meta.name);
    if !name_only {
        eprintln!("started {} ({})", meta.name, meta.harness);
    }
    Ok(())
}

/// The caller's terminal when it is one, else a default.
fn terminal_size() -> (u16, u16) {
    use nix::libc::{TIOCGWINSZ, ioctl, winsize};
    let mut ws = winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes a `winsize` into the pointer we give it.
    let ok = unsafe { ioctl(1, TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_col > 0 && ws.ws_row > 0 {
        (ws.ws_col, ws.ws_row)
    } else {
        (DEFAULT_COLS, DEFAULT_ROWS)
    }
}

/// What `ls` shows for a session: the state word the holder wrote, or how it ended.
fn state_word(h: &Held) -> Option<String> {
    match &h.status {
        Status::Live => h.state.as_ref().and_then(|s| {
            serde_json::to_value(s.state)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
        }),
        Status::Ended(e) => Some(if e.code == 0 { "finished" } else { "failed" }.into()),
        Status::Lost => Some("lost".into()),
    }
}

fn status_word(s: &Status) -> &'static str {
    match s {
        Status::Live => "live",
        Status::Ended(_) => "ended",
        Status::Lost => "lost",
    }
}

fn age(since: Timestamp) -> String {
    let secs = Timestamp::now().duration_since(since).as_secs().max(0);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

pub fn ls(home: &Home, json: bool) -> Result<()> {
    sessions::sweep(home);
    let held = sessions::list(home);
    if json {
        let rows: Vec<_> = held
            .iter()
            .map(|h| {
                serde_json::json!({
                    "name": h.meta.name,
                    "harness": h.meta.harness,
                    "status": status_word(&h.status),
                    "state": state_word(h),
                    "folder": h.meta.cwd,
                    "started": h.meta.started,
                    "holder_pid": h.meta.holder_pid,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    for h in &held {
        let state = state_word(h).unwrap_or_else(|| "live".into());
        println!(
            "{:<28} {:<9} {:<10} {}  {}",
            h.meta.name,
            h.meta.harness,
            state,
            rl_core::display_path(&h.meta.cwd),
            age(h.meta.started)
        );
    }
    Ok(())
}

/// Asks the holder to close its session and waits for the exit record.
pub async fn stop(home: &Home, name: &str) -> Result<()> {
    sessions::sweep(home);
    let Some(held) = sessions::list(home).into_iter().find(|h| h.meta.name == name) else {
        bail!("no such session: {name}");
    };
    match held.status {
        Status::Ended(_) => return Ok(()),
        Status::Lost => bail!("{name} is lost: its holder is not answering"),
        Status::Live => {}
    }
    let (_, mut session) = Session::attach(&held.meta.socket, false).await?;
    session.close().await?;
    drop(session);
    let exit = held.dir.join("exit");
    let deadline = Instant::now() + STOP_WAIT;
    while !matches!(read_exit_file(&exit), Ok(Some(_))) {
        if Instant::now() >= deadline {
            bail!("{name} did not stop within {}s", STOP_WAIT.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

pub fn rm(home: &Home, name: &str) -> Result<()> {
    sessions::remove(home, name)
}
