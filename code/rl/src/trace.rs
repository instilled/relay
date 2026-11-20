//! `RL_TRACE=1`: how long the list's phases take, a line each in
//! `$RELAY_HOME/trace.log` — `<phase> <ms>`, in the order they ended. Off,
//! it costs a load of a set-once cell per phase; nothing is timed or written.
//!
//! The phases: `start` (the process born → the list's first frame) and its
//! parts, `start.settled` (→ its input read); `frame.<why>` (what woke the
//! list → its frame drawn: a key, the tick, a harness's lister, rows read
//! off the UI thread, background work); `draw`; `reload` and its parts,
//! `regit`; `attach.held` / `attach.native` (enter → the session's first
//! output); `return.held` / `return.native` (the session let go of → the
//! list's first frame) and their parts.
//!
//! Besides the phases, `note` lines: `@<ms since born> <what>` — every input
//! event the list reads (`key …`, `mouse <kind> <col>,<row>`, `paste`) and
//! when the screen was taken back (`screen.taken`) and its settle done
//! (`screen.settled`), so what came in a handover can be read off.

use rl_core::Home;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static LOG: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
static BORN: OnceLock<Instant> = OnceLock::new();

/// The process is born: what `start` counts from. Called first thing.
pub fn born() {
    BORN.get_or_init(Instant::now);
}

/// Opens the log where `RL_TRACE` is set (and not `0`); else tracing stays
/// off for good.
pub fn open(home: &Home) {
    LOG.get_or_init(|| {
        let on = std::env::var("RL_TRACE").is_ok_and(|v| !v.is_empty() && v != "0");
        if !on {
            return None;
        }
        let _ = std::fs::create_dir_all(home.path());
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(home.path().join("trace.log"))
            .ok()
            .map(Mutex::new)
    });
}

/// Whether phases are being written.
#[inline]
pub fn on() -> bool {
    matches!(LOG.get(), Some(Some(_)))
}

/// Now, where tracing is on.
#[inline]
pub fn now() -> Option<Instant> {
    on().then(Instant::now)
}

/// `phase` took from `at` until now.
pub fn since(phase: &str, at: Option<Instant>) {
    if let (Some(at), Some(Some(log))) = (at, LOG.get()) {
        let ms = at.elapsed().as_secs_f64() * 1000.0;
        let mut f = log.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writeln!(f, "{phase} {ms:.2}");
    }
}

/// `phase` took from the process's birth until now.
pub fn since_born(phase: &str) {
    since(phase, BORN.get().copied());
}

/// A line saying what happened now: `@<ms since born> <what>`.
pub fn note(what: std::fmt::Arguments) {
    if let Some(Some(log)) = LOG.get() {
        let ms = BORN.get().map_or(0.0, |b| b.elapsed().as_secs_f64() * 1000.0);
        let mut f = log.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writeln!(f, "@{ms:.1} {what}");
    }
}

static OPEN: Mutex<Vec<(&'static str, Instant)>> = Mutex::new(Vec::new());

/// `phase` starts now, to end where `end` says — somewhere else.
pub fn begin(phase: &'static str) {
    if on() {
        let mut open = OPEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        open.retain(|(p, _)| *p != phase);
        open.push((phase, Instant::now()));
    }
}

/// `phase`, begun, ends now; nothing where it was not begun (or has ended).
pub fn end(phase: &'static str) {
    if on() {
        let at = {
            let mut open = OPEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let i = open.iter().position(|(p, _)| *p == phase);
            i.map(|i| open.remove(i).1)
        };
        since(phase, at);
    }
}

/// A phase timed from now until it is dropped.
#[must_use]
pub struct Span {
    phase: &'static str,
    at: Option<Instant>,
}

pub fn span(phase: &'static str) -> Span {
    Span { phase, at: now() }
}

impl Drop for Span {
    fn drop(&mut self) {
        since(self.phase, self.at);
    }
}
