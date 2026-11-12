//! Typing one line into one of a harness's own idle sessions, from outside
//! it: its attach (`claude attach <id>`) run on a hidden pty of a fixed size,
//! its screen read until it shows the empty input line its profile describes
//! (`sessions.typing`), the line typed and seen on that input line, `enter`,
//! the line seen taken (the input line empty again, the line above it in the
//! transcript), and the attach hung up on — the session goes on.
//!
//! Careful, and bounded: each wait has its deadline; a screen that says the
//! session is busy, or has something typed on its input line already, is
//! left alone; so is a session you are attached to from Relay's list (which
//! holds a shared lock on the session's lock file meanwhile, `Attached`),
//! and one another typer is typing into (the same file, locked whole). On
//! any doubt nothing is typed, or what was typed is taken back before
//! `enter`, and the error says why.

use crate::sessions::Typing;
use anyhow::{Context, Result, anyhow, bail};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use rl_session::backend::{self, Spawned, Spec};
use rl_session::screen::Screen;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The hidden terminal's size: wide enough that a line of mail fits on the
/// input line whole.
pub const SIZE: (u16, u16) = (120, 40);

/// How long the screen has to show the empty input line.
pub const READY_WAIT: Duration = Duration::from_secs(8);

/// How long the screen has to show what was typed, and then to show it taken.
pub const TAKE_WAIT: Duration = Duration::from_secs(5);

/// How long the screen must keep showing the empty input line, and nothing
/// busy, before anything is typed: what is drawn in one burst has settled.
const STEADY: Duration = Duration::from_millis(400);

/// How long a hung-up attach has to end before it is killed.
const GRACE: Duration = Duration::from_secs(2);

/// The longest a typer holds a session's lock: ready, typed, taken, hung
/// up, and a moment over.
pub const TYPER_MOST: Duration =
    Duration::from_secs(READY_WAIT.as_secs() + 2 * TAKE_WAIT.as_secs() + GRACE.as_secs() + 2);

/// How long a lock held by someone else is tried again before it counts.
const LOCK_GRACE: Duration = Duration::from_millis(300);

/// How often the screen is looked at.
const TICK: Duration = Duration::from_millis(25);

/// `^u`: what takes back what was typed on the input line.
const TAKE_BACK: &[u8] = b"\x15";

/// How much of the line, from its start, is looked for in the transcript.
const SEEN: usize = 40;

/// The lock file of the session `id` of `harness`, in `dir` (Relay's
/// `typing/`): held shared by an attach from Relay's list, whole by a typer.
pub fn lock_path(dir: &Path, harness: &str, id: &str) -> PathBuf {
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    dir.join(format!("{}-{}.lock", safe(harness), safe(id)))
}

fn open(lock: &Path) -> Result<File> {
    if let Some(dir) = lock.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock)
        .with_context(|| format!("opening {}", lock.display()))
}

/// You, attached to a session from Relay's list: while this is held, no
/// line is typed into it. Let go of when dropped.
pub struct Attached(#[allow(dead_code)] File);

impl Attached {
    /// Holds the session's lock shared. None where it cannot be had now (a
    /// typer is at it).
    pub fn hold(lock: &Path) -> Option<Attached> {
        Self::hold_within(lock, LOCK_GRACE)
    }

    /// The same, waiting up to `wait` for a typer at it to be done: an attach
    /// from the list waits `TYPER_MOST` so it never overlaps a line being
    /// typed, and does not attach where the lock is still not to be had.
    pub fn hold_within(lock: &Path, wait: Duration) -> Option<Attached> {
        let f = open(lock).ok()?;
        let until = Instant::now() + wait;
        while f.try_lock_shared().is_err() {
            if Instant::now() >= until {
                return None;
            }
            std::thread::sleep(TICK);
        }
        Some(Attached(f))
    }
}

/// The session's lock, held whole: nobody attached from the list, no other
/// typer.
struct Typer(#[allow(dead_code)] File);

impl Typer {
    fn take(lock: &Path) -> Result<Typer> {
        let f = open(lock)?;
        // A process forking just now holds its copy of a lock's file until
        // it execs: a moment's grace before it counts as held.
        let until = Instant::now() + LOCK_GRACE;
        let mut tried = f.try_lock();
        while matches!(tried, Err(TryLockError::WouldBlock)) && Instant::now() < until {
            std::thread::sleep(TICK);
            tried = f.try_lock();
        }
        match tried {
            Ok(()) => Ok(Typer(f)),
            Err(TryLockError::WouldBlock) => {
                // Shared: an attach holds it. Not even shared: a typer.
                let other = open(lock)?;
                if other.try_lock_shared().is_ok() {
                    bail!("you are attached to it")
                }
                bail!("a line is being typed into it already")
            }
            Err(TryLockError::Error(e)) => Err(anyhow!(e).context(format!("locking {}", lock.display()))),
        }
    }
}

/// Types `line` into the session that `attach` (its harness's attach argv)
/// opens, run in `cwd`, as `typing` reads its screen; `lock` is the
/// session's lock file (`lock_path`). Done once the line was taken; an
/// error says why nothing was, or why it is not known to have been.
pub fn type_line(attach: &[String], cwd: &Path, typing: &Typing, line: &str, lock: &Path) -> Result<()> {
    type_within(attach, cwd, typing, line, lock, READY_WAIT)
}

/// `type_line`, the screen given `ready` to show its empty input line.
fn type_within(attach: &[String], cwd: &Path, typing: &Typing, line: &str, lock: &Path, ready: Duration) -> Result<()> {
    let line = line.trim();
    if line.is_empty() || line.contains(['\r', '\n']) {
        bail!("only one line of words is typed");
    }
    let _typer = Typer::take(lock)?;
    let sp = backend::spawn(&Spec {
        argv: attach.to_vec(),
        cwd: cwd.to_path_buf(),
        env: Vec::new(),
        inherit_env: true,
        cols: SIZE.0,
        rows: SIZE.1,
        settle: Duration::ZERO,
    })
    .with_context(|| format!("running `{}`", attach.join(" ")))?;
    let mut t = Term {
        screen: Screen::without_history(SIZE.0, SIZE.1),
        sp,
        ended: false,
    };
    let said = t.type_in(typing, line, ready);
    t.hang_up();
    said
}

/// The hidden terminal: the attach's pty and the screen it draws.
struct Term {
    screen: Screen,
    sp: Spawned,
    ended: bool,
}

impl Term {
    /// Reads what it drew, until `until` says the screen is so (`Ok`) or the
    /// deadline: then `Err` with the screen as it stands.
    fn wait(&mut self, within: Duration, mut until: impl FnMut(&str) -> bool) -> Result<(), String> {
        let deadline = Instant::now() + within;
        loop {
            while let Ok(b) = self.sp.output.try_recv() {
                self.screen.feed(&b);
            }
            let text = self.screen.text();
            if until(&text) {
                return Ok(());
            }
            if !self.ended && self.sp.exit.try_recv().is_ok() {
                self.ended = true;
            }
            if self.ended && self.sp.output.is_empty() {
                return Err(text);
            }
            if Instant::now() >= deadline {
                return Err(text);
            }
            std::thread::sleep(TICK);
        }
    }

    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        self.sp
            .input
            .try_send(bytes.to_vec())
            .map_err(|_| anyhow!("its attach took no input"))
    }

    fn type_in(&mut self, typing: &Typing, line: &str, within: Duration) -> Result<()> {
        // Ready: its input line empty and nothing busy, steadily.
        let mut since: Option<Instant> = None;
        let ready = self.wait(within, |s| {
            if typing.typed(s).as_deref() == Some("") && !typing.busy(s) {
                let at = *since.get_or_insert_with(Instant::now);
                at.elapsed() >= STEADY
            } else {
                since = None;
                false
            }
        });
        if let Err(screen) = ready {
            if self.ended {
                bail!("its attach ended before its prompt showed");
            }
            if typing.busy(&screen) {
                bail!("it is busy");
            }
            return Err(match typing.typed(&screen) {
                Some(t) if !t.is_empty() => anyhow!("its prompt has something typed on it already"),
                _ => anyhow!("its screen never showed its empty prompt"),
            });
        }
        self.send(line.as_bytes())?;
        let shows = self.wait(TAKE_WAIT, |s| {
            typing
                .typed(s)
                .is_some_and(|t| !t.is_empty() && (line.starts_with(&t) || t.starts_with(line)))
        });
        if shows.is_err() {
            let _ = self.send(TAKE_BACK);
            bail!("what was typed did not show on its prompt; taken back");
        }
        self.send(b"\r")?;
        let head: String = line.chars().take(SEEN).collect();
        let taken = self.wait(TAKE_WAIT, |s| {
            typing.typed(s).as_deref() == Some("") && s.lines().any(|l| l.contains(&head))
        });
        if taken.is_err() {
            bail!("it did not show the line taken");
        }
        Ok(())
    }

    /// Hangs up on the attach, as a closed terminal does; the session goes
    /// on. One that will not end is killed.
    fn hang_up(&mut self) {
        if self.ended || self.sp.exit.try_recv().is_ok() {
            return;
        }
        let group = Pid::from_raw(self.sp.pid as i32);
        let _ = killpg(group, Signal::SIGHUP);
        let _ = killpg(group, Signal::SIGCONT);
        let deadline = Instant::now() + GRACE;
        while Instant::now() < deadline {
            if self.sp.exit.try_recv().is_ok() {
                return;
            }
            while self.sp.output.try_recv().is_ok() {}
            std::thread::sleep(TICK);
        }
        let _ = killpg(group, Signal::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Registry;

    /// Claude's input line, as its profile reads it.
    fn typing() -> Typing {
        Registry::builtin()
            .profile("claude")
            .unwrap()
            .sessions
            .as_ref()
            .unwrap()
            .typing
            .clone()
            .expect("claude says how its input line looks")
    }

    /// A stand-in attach: `fixtures/stand-in-claude-attach`, as a variant
    /// (`idle`, `busy`, `dirty`, `deaf`), recording what it is typed in `dir`.
    fn attach(dir: &Path, variant: &str) -> Vec<String> {
        vec![
            "python3".into(),
            concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-claude-attach").into(),
            variant.into(),
            dir.display().to_string(),
        ]
    }

    fn typed(dir: &Path) -> Option<String> {
        std::fs::read_to_string(dir.join("typed")).ok()
    }

    #[test]
    fn the_input_line_is_the_last_with_its_mark() {
        let t = typing();
        let screen = "❯ Reply with hi\n⏺ hi\n────\n❯\u{a0}\n────\n  ~/x";
        assert_eq!(t.typed(screen).as_deref(), Some(""), "the transcript's is not it");
        assert_eq!(t.typed("────\n❯\u{a0}abc def\n────").as_deref(), Some("abc def"));
        assert_eq!(t.typed("❯ quoted only"), None);
        assert!(t.busy("✻ Thinking… (3s · esc to interrupt)\n❯\u{a0}"));
        assert!(t.busy("✶ Puttering… (running UserPromptSubmit hook · 0s)\n❯\u{a0}"));
        assert!(!t.busy("✻ Worked for 17m 59s · done 12:46 AM\n❯\u{a0}"), "a turn done");
        assert!(!t.busy(screen));
    }

    #[test]
    fn a_line_is_typed_into_an_idle_session_and_seen_taken() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = lock_path(&tmp.path().join("typing"), "claude", "ab12cd34");
        type_line(
            &attach(tmp.path(), "idle"),
            tmp.path(),
            &typing(),
            "/rename the fix",
            &lock,
        )
        .unwrap();
        assert_eq!(typed(tmp.path()).as_deref(), Some("/rename the fix\n"));
        assert!(File::open(&lock).unwrap().try_lock().is_ok(), "let go of");
    }

    #[test]
    fn a_busy_session_or_one_with_something_typed_is_left_alone() {
        for (variant, why) in [
            ("busy", "it is busy"),
            ("dirty", "its prompt has something typed on it already"),
            ("blank", "its screen never showed its empty prompt"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let lock = lock_path(tmp.path(), "claude", "x");
            let at = Instant::now();
            let wait = Duration::from_millis(1500);
            let e = type_within(&attach(tmp.path(), variant), tmp.path(), &typing(), "hi", &lock, wait).unwrap_err();
            assert_eq!(e.to_string(), why, "{variant}");
            assert_eq!(typed(tmp.path()), None, "{variant}: nothing typed");
            assert!(
                at.elapsed() < wait + Duration::from_secs(4),
                "{variant}: {:?}",
                at.elapsed()
            );
        }
    }

    #[test]
    fn a_line_not_taken_is_said_so() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = lock_path(tmp.path(), "claude", "x");
        let e = type_line(&attach(tmp.path(), "deaf"), tmp.path(), &typing(), "hi there", &lock).unwrap_err();
        assert_eq!(e.to_string(), "what was typed did not show on its prompt; taken back");
        assert_eq!(typed(tmp.path()), None);
    }

    #[test]
    fn nothing_is_typed_while_you_are_attached_or_another_types() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = lock_path(tmp.path(), "claude", "ab12cd34");
        let you = Attached::hold(&lock).expect("an attach holds it");
        let e = type_line(&attach(tmp.path(), "idle"), tmp.path(), &typing(), "hi", &lock).unwrap_err();
        assert_eq!(e.to_string(), "you are attached to it");
        drop(you);
        let other = Typer::take(&lock).unwrap();
        let e = type_line(&attach(tmp.path(), "idle"), tmp.path(), &typing(), "hi", &lock).unwrap_err();
        assert_eq!(e.to_string(), "a line is being typed into it already");
        assert!(Attached::hold(&lock).is_none(), "a typer at it: not to be had now");
        drop(other);
        assert_eq!(typed(tmp.path()), None);
        type_line(&attach(tmp.path(), "idle"), tmp.path(), &typing(), "hi", &lock).unwrap();
        assert_eq!(typed(tmp.path()).as_deref(), Some("hi\n"));
    }

    #[test]
    fn an_attach_waits_for_a_typer_to_be_done() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = lock_path(tmp.path(), "claude", "ab12cd34");
        let typer = Typer::take(&lock).unwrap();
        let let_go = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            drop(typer);
        });
        let started = Instant::now();
        let you = Attached::hold_within(&lock, Duration::from_secs(5));
        assert!(you.is_some(), "had once the typer let go");
        assert!(
            started.elapsed() >= Duration::from_millis(400),
            "it waited for the typer"
        );
        let_go.join().unwrap();
        drop(you);

        let _busy = Typer::take(&lock).unwrap();
        assert!(
            Attached::hold_within(&lock, Duration::from_millis(200)).is_none(),
            "still typing when the wait is over: not had"
        );
    }

    #[test]
    fn a_lock_file_is_named_safely() {
        assert_eq!(
            lock_path(Path::new("/r/typing"), "claude", "../x y"),
            Path::new("/r/typing/claude-___x_y.lock")
        );
    }
}
