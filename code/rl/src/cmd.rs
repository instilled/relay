//! The `new | ls | stop | rm` commands.

use crate::sources::{self, Harness, Source};
use crate::spawn::{self, Start};
use crate::store::sessions::{self, Held, Status};
use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rl_core::Home;
use rl_detect::harness::Started;
use rl_session::client::Session;
use rl_session::exit::read_exit_file;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const STOP_WAIT: Duration = Duration::from_secs(10);
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
    if let Some((a, words)) = asked(harness.as_deref(), &argv)
        && new_own(a, &cwd, name_only, words).await?
    {
        return Ok(());
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
/// runs), and the words after its binary.
fn asked<'a>(harness: Option<&str>, argv: &'a [String]) -> Option<(&'static dyn Harness, &'a [String])> {
    let (name, words) = match harness {
        Some(h) => (h.to_string(), argv),
        None => (crate::rows::registry().detect(argv)?.name.clone(), argv.get(1..)?),
    };
    Some((sources::harness(&name)?, words))
}

/// `rl new` as its harness starts it, `words` after its binary
/// (`Harness::start`): in its own background (its id printed), or run here
/// in the foreground, where it begins a session itself (the process exits
/// as it did). False: Relay is to hold it, as asked.
async fn new_own(a: &'static dyn Harness, cwd: &Path, name_only: bool, words: &[String]) -> Result<bool> {
    let what = a.info().name.clone();
    let cwd = std::fs::canonicalize(cwd).with_context(|| format!("cwd {}", cwd.display()))?;
    let start = rl_detect::harness::Start {
        cwd: cwd.clone(),
        words: Some(words.to_vec()),
        ..Default::default()
    };
    let started = tokio::task::spawn_blocking(move || a.start(&start)).await?;
    match started {
        Ok(Started::Held(_)) => Ok(false),
        Ok(Started::Background(id)) => {
            println!("{id}");
            if !name_only {
                let s = rl_detect::harness::Session::started(&what, &id, &cwd);
                match a.attach(&s) {
                    rl_detect::harness::Attach::Argv(argv) => {
                        eprintln!("started {what} {id} in its background · `{}` attaches", argv.join(" "))
                    }
                    _ => eprintln!("started {what} {id} in its background"),
                }
            }
            Ok(true)
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
                Some(0) => Ok(true),
                Some(code) => std::process::exit(code),
                None => bail!("{what} was killed"),
            }
        }
        Err(e) => Err(e.context(format!("start {what}"))),
    }
}

/// What stops and removes the sessions Relay holds, for their harnesses
/// (`Harness::stop`, `Harness::remove`): this store, from a session's
/// folder (`<home>/sessions/<name>`). Safe to ask from any thread (`block`).
pub struct Store;

impl rl_detect::harness::Holder for Store {
    fn stop(&self, dir: &Path, name: &str) -> Result<()> {
        let home = home_of(dir)?;
        block(async move { stop(&home, name).await })
    }

    fn remove(&self, dir: &Path, name: &str) -> Result<()> {
        let home = home_of(dir)?;
        block(async move { rm(&home, name).await })
    }

    fn screen(&self, dir: &Path, name: &str, rows: usize) -> Result<Vec<String>> {
        let meta = sessions::read_meta(dir).with_context(|| format!("{name} is not one of Relay's sessions"))?;
        let lines = block(async move { tokio::time::timeout(SCREEN_WAIT, screen(&meta.socket)).await? })
            .with_context(|| format!("{name} is not answering"))?;
        let from = lines.len().saturating_sub(rows);
        Ok(lines[from..].to_vec())
    }
}

/// How long a held session's holder has to hand over its screen.
const SCREEN_WAIT: Duration = Duration::from_secs(3);

/// The screen of the holder at `socket`, as text (`Screen::lines`): it is
/// asked for its screen drawn afresh, which is read to its end and drawn on
/// a screen of the holder's size; then the connection goes. Nothing is sent
/// after the attach — no size, no input — so the session is as it was.
async fn screen(socket: &Path) -> Result<Vec<String>> {
    use rl_session::proto::Frame;
    let (hello, mut session) = Session::attach_screen(socket).await?;
    let mut drawn = rl_session::screen::Screen::new(hello.cols, hello.rows);
    loop {
        match session.recv().await? {
            Some(Frame::Output(bytes)) => drawn.feed(&bytes),
            Some(Frame::ReplayEnd) | None => break,
            Some(_) => {}
        }
    }
    Ok(drawn.lines())
}

/// The Relay home a session's folder is in.
fn home_of(dir: &Path) -> Result<Home> {
    dir.parent()
        .and_then(Path::parent)
        .map(Home::at)
        .with_context(|| format!("{} is not a session's folder", dir.display()))
}

/// `f` run to its end on a runtime of its own, on a thread of its own: safe
/// from anywhere, a runtime's worker included (where `Handle::block_on`
/// would panic), whichever runtime the caller is on.
fn block<T: Send>(f: impl std::future::Future<Output = Result<T>> + Send) -> Result<T> {
    std::thread::scope(|s| {
        s.spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(f)
        })
        .join()
        .map_err(|_| anyhow::anyhow!("the held session's stop panicked"))?
    })
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

/// How long `rl ls` waits for a harness's own listing.
const LS_WAIT: Duration = Duration::from_secs(5);

/// `rl ls`: what the list shows, once — the sessions Relay holds, the
/// harnesses' own background sessions and those in other terminals their
/// hooks tell of, one row each (`sources::rows`). A harness whose listing
/// fails or takes over `LS_WAIT` is left out, and stderr says so.
pub fn ls(home: &Home, json: bool) -> Result<()> {
    let rows = listing(home, sources::native_listers(), LS_WAIT, &mut |w| {
        eprintln!("rl ls: {w}")
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    for l in lines(&rows) {
        println!("{l}");
    }
    Ok(())
}

/// Every row of the list, as `rl ls --json` gives it: `kind` (`held`,
/// `background`, `terminal`), `id` (a held session's name, a background
/// one's harness id, a hooked one's conversation id), `name`, `harness`,
/// `state`, `status` (`live`, `ended`, `lost`), `folder`, `started` and
/// `holder_pid` (null where it is not Relay's). The listers are asked at
/// once, each on a thread of its own, and only for `wait`: one that fails
/// or has not answered by then is `warn`ed of and its sessions left out.
pub fn listing(
    home: &Home,
    listers: Vec<Box<dyn sources::NativeLister>>,
    wait: Duration,
    warn: &mut dyn FnMut(String),
) -> Vec<serde_json::Value> {
    sessions::sweep(home);
    let held = sessions::list(home);
    let hooked = crate::store::state::list(home, Timestamp::now());
    let listed = sources::list_all(listers.into_iter().map(std::sync::Arc::from), wait, warn);
    let rows = sources::rows(&held, &hooked, &listed.rows, &listed.answered);
    rows.iter()
        .map(|r| {
            let own = r.key.splitn(3, ':').last().unwrap_or_default();
            let (kind, id) = match r.source {
                Source::Held => ("held", r.name.as_str()),
                Source::Native => ("background", own),
                Source::Hooked => ("terminal", own),
            };
            let h = held.iter().find(|h| r.source == Source::Held && h.meta.name == r.name);
            let (status, state) = match h {
                Some(h) => (status_word(&h.status), state_word(h)),
                None if r.state.is_terminal() => ("ended", Some(r.state.to_string())),
                None => ("live", Some(r.state.to_string())),
            };
            serde_json::json!({
                "kind": kind,
                "id": id,
                "name": r.name,
                "harness": r.harness,
                "state": state,
                "status": status,
                "folder": r.cwd,
                "started": r.started,
                "holder_pid": h.and_then(|h| h.meta.holder_pid),
            })
        })
        .collect()
}

/// `rl ls`'s lines: name, harness, kind, state, folder and age, one row a
/// line.
fn lines(rows: &[serde_json::Value]) -> Vec<String> {
    let s = |r: &serde_json::Value, k: &str| r[k].as_str().unwrap_or_default().to_string();
    rows.iter()
        .map(|r| {
            let state = r["state"].as_str().unwrap_or("live");
            let age = r["started"]
                .as_str()
                .and_then(|t| t.parse::<Timestamp>().ok())
                .map_or_else(|| "-".to_string(), age);
            format!(
                "{:<28} {:<9} {:<10} {:<10} {}  {}",
                s(r, "name"),
                s(r, "harness"),
                s(r, "kind"),
                state,
                rl_core::display_path(Path::new(&s(r, "folder"))),
                age
            )
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::{NativeLister, NativeRow, NativeState};
    use crate::store::sessions::Meta;
    use crate::store::state::{self, HookState, Said};
    use rl_core::state::{RunKind, RunState};

    /// A harness's lister: what it says, after `delay`.
    struct Fake {
        harness: &'static str,
        said: Result<Vec<NativeRow>, String>,
        delay: Duration,
    }

    impl NativeLister for Fake {
        fn harness(&self) -> &str {
            self.harness
        }

        fn list(&self) -> anyhow::Result<Vec<NativeRow>> {
            std::thread::sleep(self.delay);
            self.said.clone().map_err(anyhow::Error::msg)
        }
    }

    fn native(harness: &str, id: &str, session_id: &str, cwd: &Path) -> NativeRow {
        NativeRow {
            harness: harness.into(),
            id: id.into(),
            elsewhere: false,
            session_id: session_id.into(),
            name: format!("{harness} fix"),
            cwd: cwd.to_path_buf(),
            state: NativeState {
                words: "ready".into(),
                state: RunState::Idle,
            },
            summary: None,
            started: Some(Timestamp::from_second(100).unwrap()),
            ended: None,
            at: None,
            pr: None,
            color: None,
        }
    }

    fn hooked(home: &Home, agent: &str, id: &str) {
        let now = Timestamp::now();
        let h = HookState {
            agent: agent.into(),
            session_id: id.into(),
            cwd: home.path().to_path_buf(),
            state: Said::Busy,
            detail: None,
            pid: None,
            transcript_path: None,
            started: now,
            at: now,
        };
        state::write(&state::path(home, agent, id).unwrap(), &h).unwrap();
    }

    #[test]
    fn ls_lists_what_the_list_shows_once_each_and_says_which_harness_did_not_answer() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("relay"));
        home.ensure_store().unwrap();
        let dir = home.session_dir("api-sh");
        std::fs::create_dir_all(&dir).unwrap();
        sessions::write_meta(
            &dir,
            &Meta {
                name: "api-sh".into(),
                harness: "sh".into(),
                kind: RunKind::Shell,
                cwd: home.path().to_path_buf(),
                argv: vec!["sh".into()],
                started: Timestamp::now(),
                socket: dir.join("sock"),
                holder_pid: Some(1),
            },
        )
        .unwrap();
        // One in another terminal; one its harness lists too, which stands
        // once, as listed.
        hooked(&home, "gemini", "g-1");
        hooked(&home, "codex", "t-1");
        let listers: Vec<Box<dyn NativeLister>> = vec![
            Box::new(Fake {
                harness: "codex",
                said: Ok(vec![native("codex", "t-1", "t-1", home.path())]),
                delay: Duration::ZERO,
            }),
            Box::new(Fake {
                harness: "claude",
                said: Err("no answer".into()),
                delay: Duration::ZERO,
            }),
            Box::new(Fake {
                harness: "slow",
                said: Ok(vec![native("slow", "s-1", "s-1", home.path())]),
                delay: Duration::from_secs(5),
            }),
        ];
        let mut warned = Vec::new();
        let at = Instant::now();
        let rows = listing(&home, listers, Duration::from_millis(300), &mut |w| warned.push(w));
        assert!(at.elapsed() < Duration::from_secs(3), "{:?}", at.elapsed());
        let keys: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|r| {
                (
                    r["kind"].as_str().unwrap(),
                    r["harness"].as_str().unwrap(),
                    r["id"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            keys,
            [
                ("held", "sh", "api-sh"),
                ("terminal", "gemini", "g-1"),
                ("background", "codex", "t-1"),
            ]
        );
        let bg = &rows[2];
        assert_eq!(bg["name"], "codex fix");
        assert_eq!(bg["state"], "idle");
        assert_eq!(bg["status"], "live");
        assert_eq!(bg["folder"], home.path().to_str().unwrap());
        assert_eq!(rows[0]["status"], "lost");
        assert_eq!(rows[0]["holder_pid"], 1);
        assert_eq!(rows[1]["state"], "working");
        assert!(rows[1]["holder_pid"].is_null());
        let keys_of = |r: &serde_json::Value| {
            let mut k: Vec<String> = r.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert!(
            rows.iter().all(|r| keys_of(r) == keys_of(&rows[0])),
            "the same keys: {rows:?}"
        );
        assert_eq!(warned.len(), 2, "{warned:?}");
        assert!(
            warned.iter().any(|w| w.contains("claude") && w.contains("no answer")),
            "{warned:?}"
        );
        assert!(
            warned
                .iter()
                .any(|w| w.contains("slow") && w.contains("did not answer")),
            "{warned:?}"
        );

        let text = lines(&rows);
        assert_eq!(text.len(), 3);
        assert!(
            text[2].starts_with("codex fix") && text[2].contains(" codex ") && text[2].contains(" background "),
            "{text:?}"
        );
    }
}
