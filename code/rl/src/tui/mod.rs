//! The list: every session — Relay's, those an agent's hooks told of, and
//! the harnesses' own — live from the files under the Relay home, grouped by
//! project or by state (see `rows`). It reads again when a file under
//! `sessions/` or `state/` changes, everything (git and badges too) every few
//! seconds, and asks the harnesses for their own sessions in the background;
//! all of it only while the list is open.
//!
//! Layout: the header, the list, the toggles line (where a search is typed),
//! and the footer's hints; how things stand rides the window's title too.

mod act;
mod app;
mod draw;
mod form;
mod input;
mod keys;
mod line;
mod live;
mod mark;
mod remove;
mod select;
mod shortcuts;
mod theme;

#[cfg(test)]
mod tests;

pub use app::App;
use app::{Hand, To};

use crate::store::sessions;
use crate::{attach, badges, sources};
use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, EventStream,
    KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::supports_keyboard_enhancement;
use futures::StreamExt;
use rl_core::Home;
use std::io::Write;
use std::time::Duration;

/// How often everything is read again, the git checkouts and badges too.
const REFRESH: Duration = Duration::from_secs(3);
/// How often the harnesses are asked for their own sessions.
const NATIVE_EVERY: Duration = Duration::from_secs(5);

/// Opens the list on this terminal, until `^c ^c`.
pub async fn run(home: &Home) -> Result<()> {
    home.ensure_store()?;
    sessions::sweep(home);
    if let Err(e) = badges::seed(home) {
        tracing::debug!("badges not seeded: {e:#}");
    }
    let mut app = App::new(home.clone())?;
    let mut terminal = enter_screen();
    let result = app.main_loop(&mut terminal).await;
    leave_screen();
    // The title back to the terminal's own.
    let _ = write!(std::io::stdout(), "{}", crate::attach::title(""));
    let _ = std::io::stdout().flush();
    result
}

impl App {
    async fn main_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let mut input = EventStream::new();
        let mut watch = match live::Watch::start(&self.home) {
            Ok(w) => Some(w),
            Err(e) => {
                self.say(format!("not watching the files ({e}) · the list reads them every 3s"));
                None
            }
        };
        let mut native = live::Native::start(sources::native_listers(), NATIVE_EVERY);
        let mut tick = tokio::time::interval(REFRESH);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        // A change under the watched folders reads them again once the burst
        // has had `DEBOUNCE` to settle.
        let mut due: Option<tokio::time::Instant> = None;
        let mut title = String::new();
        while !self.quit {
            terminal.draw(|f| self.draw(f))?;
            let now = self.title();
            if now != title {
                let _ = write!(std::io::stdout(), "{}", crate::attach::title(&now));
                let _ = std::io::stdout().flush();
                title = now;
            }
            tokio::select! {
                ev = input.next() => match ev {
                    Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => self.key(input::typed(k)),
                    Some(Ok(Event::Mouse(m))) => self.mouse(m),
                    Some(Ok(Event::Paste(s))) => self.paste(&s),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => self.quit = true,
                },
                () = changed(&mut watch) => {
                    due.get_or_insert_with(|| tokio::time::Instant::now() + live::DEBOUNCE);
                }
                () = until(due) => {
                    due = None;
                    self.reload();
                }
                _ = tick.tick() => {
                    self.reload();
                    self.regit();
                }
                () = native.changed() => {
                    let rows = native.rows();
                    self.set_native(rows);
                }
                Some(done) = self.work.join_next(), if !self.work.is_empty() => match done {
                    Ok(d) => self.done(d),
                    Err(e) => self.say(format!("{e}")),
                },
            }
            if std::mem::take(&mut self.ask_native) {
                native.ask_now();
            }
            // A key, or a conversation carried on, gives the terminal away:
            // the session gets it, passed through; the list comes back when
            // it lets go.
            if let Some(hand) = self.hand.take() {
                drop(input);
                leave_screen();
                let words = hand_over(&self.home, &hand).await;
                *terminal = enter_screen();
                input = EventStream::new();
                title.clear();
                self.back(&hand.key, words);
            }
        }
        Ok(())
    }
}

/// The terminal handed to what `enter` asked for, until it is let go of or
/// ends; then what came of it, in words for the footer.
async fn hand_over(home: &Home, hand: &Hand) -> String {
    let title = &hand.title;
    match &hand.to {
        To::Held(meta) => match attach::held::attach(home, meta).await {
            Ok(ended) => back_words(title, &ended),
            Err(e) => format!("attach {title}: {e:#}"),
        },
        To::Argv(argv) => match attach::native::hold(argv, title).await {
            Ok(attach::native::Back::Detached) => back_words(title, &attach::Ended::Detached),
            Ok(attach::native::Back::Ended(exit)) => back_words(title, &attach::held::exit_words(title, &exit).0),
            Err(e) => format!("attach {title}: {e:#}"),
        },
    }
}

/// What the footer says once an attach is over.
pub fn back_words(title: &str, ended: &attach::Ended) -> String {
    match ended {
        attach::Ended::Detached | attach::Ended::Left => format!("let go of {title} · it keeps going"),
        attach::Ended::Exit { how, .. } => format!("{title} ended ({how})"),
        attach::Ended::Closed => format!("lost the connection to {title} · it may still be going"),
    }
}

/// The watch's next change; never, with no watch.
async fn changed(watch: &mut Option<live::Watch>) {
    match watch {
        Some(w) => w.changed().await,
        None => std::future::pending().await,
    }
}

/// When `due` comes; never, with nothing due.
async fn until(due: Option<tokio::time::Instant>) {
    match due {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Whether this terminal speaks the kitty keyboard protocol — asked once, the
/// first time we take the screen, and remembered. The query is a terminal
/// round-trip (it writes an escape sequence and reads the reply); asking it
/// again at every screen handoff would fire that query just as we hand stdin
/// to a session, and its reply would leak into it — the stray `^[[?…c` an
/// attached agent would otherwise see. So we ask exactly once.
fn kitty_supported() -> bool {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(|| supports_keyboard_enhancement().unwrap_or(false))
}

/// Take the screen, and — where the terminal speaks the kitty keyboard
/// protocol — ask it to report modifiers on keys the base protocol folds
/// away (shift on the arrows), and to say what shift made of a key (see
/// `input::typed`). Terminals that don't (Terminal.app, older tmux) simply
/// see no flags.
fn enter_screen() -> ratatui::DefaultTerminal {
    // The input is whatever a session left: answers to questions it put to
    // the terminal just before letting go, which were never its keys and are
    // not ours (see `input`).
    input::discard_pending(std::io::stdin());
    // Coming back from a session, the screen is whatever it left: a
    // full-screen agent keeps its own alternate screen, its scroll region and
    // its colours when it is let go of mid-session, and every mode it asked
    // the terminal for — focus reports, mouse tracking. Leave that screen so
    // ours starts fresh, and put the region, the colours and the modes back;
    // ours are asked for again below.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::style::Print("\x1b[?1049l\x1b[r\x1b[0m\x1b[?1004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l")
    );
    let mut term = ratatui::init();
    // A fresh terminal diffs against an empty buffer, so it would never write
    // the cells it means to leave blank — and whatever a session drew there
    // would stay. Clearing makes the first draw paint every cell.
    let _ = term.clear();
    if kitty_supported() {
        // A session's own keyboard flags would change what every key sends
        // us: none, then ours.
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::style::Print("\x1b[=0;1u"),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        );
    }
    // The wheel and a click reach the list. This also takes the terminal's own
    // drag-to-select, so hold shift (option, on macOS) to copy text as before.
    // A paste comes whole.
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    term
}

/// Hand the screen back, popping any enhancement flags we pushed and giving
/// the mouse back to whoever has the terminal next.
///
/// And the caret. Drawing hides the terminal's cursor, and `ratatui::restore`
/// leaves raw mode and the alternate screen but never shows it again; a
/// session attached next would get a terminal with its cursor switched off,
/// and most never switch it back on. The next draw hides it again.
fn leave_screen() {
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    if kitty_supported() {
        let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
}
