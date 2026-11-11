//! The `new | ls | stop | rm` commands.

use crate::sources::{self, Harness};
use crate::spawn::{self, Start};
use crate::store::sessions::{self, Held, Status};
use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rl_core::Home;
use rl_detect::harness::Started;
use rl_session::client::Session;
use rl_session::exit::read_exit_file;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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
    if let Some((a, words)) = never_held(harness.as_deref(), &argv) {
        return new_own(a, &cwd, name_only, words).await;
    }
    let (cols, rows) = terminal_size();
    let meta = spawn::start(
        home,
        Start {
            harness,
            cwd,
            argv,
            shell: false,
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

/// The harness `rl new` was asked for (`--harness`, else what `argv`
/// runs), where Relay never holds it (Codex), and the words after its
/// binary.
fn never_held<'a>(harness: Option<&str>, argv: &'a [String]) -> Option<(&'static dyn Harness, &'a [String])> {
    let (name, words) = match harness {
        Some(h) => (h.to_string(), argv),
        None => (crate::rows::registry().detect(argv)?.name.clone(), argv.get(1..)?),
    };
    Some((sources::harness(&name).filter(|h| h.never_held())?, words))
}

/// `rl new` for a harness Relay never holds: started in its own background
/// with `words` as its prompt (its id printed), or, with none, run here in
/// the foreground, where it begins a session itself (the process exits as
/// it did).
async fn new_own(a: &'static dyn Harness, cwd: &Path, name_only: bool, words: &[String]) -> Result<()> {
    let what = a.name();
    if let Some(flag) = words.iter().find(|w| w.starts_with('-')) {
        bail!("{what} starts in its own background, where `{flag}` can't go: give it a prompt only");
    }
    let cwd = std::fs::canonicalize(cwd).with_context(|| format!("cwd {}", cwd.display()))?;
    let prompt = (!words.is_empty()).then(|| words.join(" "));
    let at = cwd.clone();
    let started = tokio::task::spawn_blocking(move || a.start(&at, None, prompt.as_deref(), &HashSet::new())).await?;
    match started {
        Ok(Started::Id(id)) => {
            println!("{id}");
            if !name_only {
                match a.attach_argv(&id) {
                    Some(argv) => eprintln!("started {what} {id} in its background · `{}` attaches", argv.join(" ")),
                    None => eprintln!("started {what} {id} in its background"),
                }
            }
            Ok(())
        }
        Ok(Started::Here(argv)) => {
            let (bin, args) = argv.split_first().context("nothing to run")?;
            let status = tokio::process::Command::new(bin)
                .args(args)
                .current_dir(&cwd)
                .status()
                .await
                .with_context(|| format!("running {bin}"))?;
            match status.code() {
                Some(0) => Ok(()),
                Some(code) => std::process::exit(code),
                None => bail!("{what} was killed"),
            }
        }
        Err(e) => Err(e.context(format!("start {what}"))),
        Ok(Started::Held(_)) => bail!("{what} has no background to start in"),
    }
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
        Status::Live => h.current().map(|c| c.state.to_string()),
        Status::Ended(e) => Some(h.ended_state(e).to_string()),
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
        // The holder lingers after its session ends; let it go.
        Status::Ended(_) => {
            let_go(&held).await;
            return Ok(());
        }
        Status::Lost => bail!("{name} is lost: its holder is not answering"),
        Status::Live => {}
    }
    // Said before it is asked, so however it exits it reads stopped.
    sessions::mark_stopped(&held.dir)?;
    let closed = async {
        let (_, mut session) = Session::attach(&held.meta.socket, false).await?;
        session.close().await?;
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = closed {
        let _ = std::fs::remove_file(held.dir.join(sessions::STOPPED));
        return Err(e);
    }
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

/// Attaches this terminal to a session; the process exits with how it ended.
pub async fn attach(home: &Home, name: &str) -> Result<()> {
    sessions::sweep(home);
    let Some(held) = sessions::list(home).into_iter().find(|h| h.meta.name == name) else {
        bail!("no such session: {name}");
    };
    if held.status == Status::Lost {
        bail!("{name} is lost: its holder is not answering");
    }
    let ended = crate::attach::held::attach(home, &held.meta).await?;
    match ended.code() {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

/// Asks the holder of an ended session, if its socket still answers, to
/// exit now rather than linger.
async fn let_go(held: &Held) {
    if let Ok((_, mut session)) = Session::attach(&held.meta.socket, false).await {
        let _ = session.close().await;
    }
}

/// Removes an ended session. A holder that still lingers is closed first, so
/// it exits now and not whenever it next notices its socket is gone.
pub async fn rm(home: &Home, name: &str) -> Result<()> {
    if let Some(held) = sessions::list(home).into_iter().find(|h| h.meta.name == name)
        && matches!(held.status, Status::Ended(_))
    {
        let_go(&held).await;
    }
    sessions::remove(home, name)
}

/// The list's key for the session `name` names: a held session's name, else
/// a row's key as the list shows it (`n:<harness>:<id>` for a harness's own,
/// `o:<agent>:<session id>` for one only its hooks have spoken of). A hooked
/// row has to exist; a harness's own cannot be told without asking the
/// harness, so its key is taken as given.
pub fn row_key(home: &Home, name: &str) -> Result<String> {
    if sessions::list(home).iter().any(|h| h.meta.name == name) {
        return Ok(format!("r:{name}"));
    }
    let mut parts = name.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("n"), Some(h), Some(id)) if !h.is_empty() && !id.is_empty() => Ok(name.to_string()),
        (Some("o"), Some(agent), Some(id)) => {
            let hooked = crate::store::state::list(home, Timestamp::now());
            if hooked.iter().any(|s| s.agent == agent && s.session_id == id) {
                Ok(name.to_string())
            } else {
                bail!("no such session: {name}")
            }
        }
        _ => bail!("no such session: {name}"),
    }
}

/// Sets, or with `none` clears, your own colour for a session; says what it
/// did. Written the way the list writes it: the file re-read first, saved
/// atomically.
pub fn color(home: &Home, name: &str, colour: &str) -> Result<String> {
    let clear = colour == "none";
    if !clear && crate::colour::parse(colour).is_none() {
        bail!(
            "not a colour: {colour} (a name — red, blue, green, yellow, purple, orange, pink, cyan — or #rrggbb, or none)"
        );
    }
    let key = row_key(home, name)?;
    let mut config = crate::config::Config::load(home)?;
    if clear {
        config.list.colors.remove(&key);
    } else {
        config.list.colors.insert(key, colour.to_string());
    }
    config.save(home)?;
    Ok(if clear {
        format!("{name}: no colour of your own")
    } else {
        format!("{name}: {colour}")
    })
}
