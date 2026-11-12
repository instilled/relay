//! `rl setup` as a walk through six steps, on the terminal it was started
//! on: what is found, what setup changes (each harness on or off), the
//! changes being made, what is left for you, the keys you will use, and
//! what you can do now. Or the quick way: `tab` on the first step applies
//! everything and leaves a short summary.
//!
//! It draws in an inline viewport, under whatever the terminal showed, not
//! on the alternate screen, and when it is over leaves its summary in the
//! scrollback (`view::summary`). Raw mode only while it runs. It is a view
//! on `setup`: `check` per harness to start, `apply` per part to change
//! (`engine`, off this thread).
//!
//! It runs for `rl setup` on a terminal, the list's first start, and the
//! list's `/setup` (the list gives the terminal up for it as it does for an
//! attach). `--yes`, `--check`, `--remove`, `--harness`, no terminal, and a
//! held session keep the plain output.

mod engine;
pub mod model;
pub mod view;

#[cfg(test)]
mod tests;

use crate::setup::{self, Host};
use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind, MouseButton, MouseEventKind};
use model::{Came, Effect, Ended, Wizard};
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};
use rl_core::Home;
use std::io::Write;
use std::time::Duration;

/// The viewport's height, where the terminal is that tall.
const VIEW_H: u16 = 24;
/// How often it draws again while nothing happens: the spinner's pace.
const FRAME: Duration = Duration::from_millis(80);

/// What the wizard came to.
pub struct Outcome {
    pub ended: Ended,
    /// In a few words, for the list's footer.
    pub said: String,
}

/// The wizard over `home`, from `came`, on this terminal; its summary
/// printed once it is over.
pub fn run(home: &Home, host: &Host, came: Came) -> Result<Outcome> {
    let harns = model::gather(home, host)?;
    let mut wz = Wizard::new(came, harns);
    let shown = show(&mut wz, home, host);
    let ended = wz.ended.unwrap_or(Ended::Skipped);
    if came == Came::FirstRun {
        setup::keep_config(home)?;
    }
    shown?;
    let mut out = std::io::stdout();
    for l in view::summary(&wz) {
        writeln!(out, "{}", view::ansi(&l))?;
    }
    out.flush()?;
    Ok(Outcome { ended, said: wz.said() })
}

/// `rl setup` on a terminal.
pub fn command(home: &Home) -> Result<()> {
    run(home, &Host::real()?, Came::Command).map(|_| ())
}

/// The list's first start: how it ended (`ctrl+c`: no list after it).
pub fn first_run(home: &Home) -> Result<Ended> {
    let host = Host::real()?;
    match run(home, &host, Came::FirstRun) {
        Ok(o) => Ok(o.ended),
        Err(e) => {
            setup::keep_config(home)?;
            Err(e)
        }
    }
}

/// The wizard on screen until it ends; the viewport cleared after, the
/// cursor where it began and the terminal as it was.
fn show(wz: &mut Wizard, home: &Home, host: &Host) -> Result<()> {
    super::guard_panics();
    crossterm::terminal::enable_raw_mode()?;
    // A click ticks a change: clicks, without the pointer's every move, as
    // the list has them.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::style::Print(super::MOUSE_ON));
    let res = drive(wz, home, host);
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::cursor::Show
    );
    let _ = crossterm::terminal::disable_raw_mode();
    res
}

fn drive(wz: &mut Wizard, home: &Home, host: &Host) -> Result<()> {
    let (_, rows) = crossterm::terminal::size()?;
    let mut term = Terminal::with_options(
        CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(rows.clamp(1, VIEW_H)),
        },
    )?;
    let (tx, rx) = std::sync::mpsc::channel();
    let mut drawn = view::Drawn::default();
    while wz.ended.is_none() {
        term.draw(|f| drawn = view::draw(wz, f))?;
        if let Some(r) = drawn.link {
            // The words the frame drew there, made a link where the
            // terminal makes them.
            let text = view::ansi(&ratatui::text::Line::from(ratatui::text::Span::styled(
                view::LINK_TEXT,
                crate::tui::theme::accent().add_modifier(ratatui::style::Modifier::UNDERLINED),
            )));
            let text = text.replace(
                view::LINK_TEXT,
                &view::link(crate::guide::MESSAGING_GUIDE, view::LINK_TEXT),
            );
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::cursor::MoveTo(r.x, r.y),
                crossterm::style::Print(text)
            );
        }
        while let Ok(p) = rx.try_recv() {
            wz.progress(p);
        }
        if wz.ended.is_some() {
            break;
        }
        if event::poll(FRAME)? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    if let Some(e) = wz.key(k) {
                        effect(e, home, host, &tx);
                    }
                }
                Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(row) = drawn.row_at(m.row) {
                        wz.click(row);
                    }
                }
                Event::Resize(..) => term.autoresize()?,
                _ => {}
            }
        }
        wz.tick = wz.tick.wrapping_add(1);
    }
    term.clear()?;
    Ok(())
}

/// What a key asked for: what was declined kept, the rest applied off this
/// thread.
fn effect(e: Effect, home: &Home, host: &Host, tx: &std::sync::mpsc::Sender<model::Progress>) {
    let Effect::Apply { jobs, decline, accept } = e;
    if let Err(e) = setup::set_declined(home, &decline, &accept) {
        tracing::debug!("declined not kept: {e:#}");
    }
    if !jobs.is_empty() {
        engine::spawn(home.clone(), host.clone(), jobs, tx.clone());
    }
}
