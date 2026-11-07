//! `rl-session`: the process that owns one run's terminal.
//!
//! `rld` spawns one per run and is only ever a *client* of it (principle 6).
//! The holder detaches (`fork` + `setsid`), holds the PTY master (or pipes with
//! `--no-tty`), keeps a ring buffer of recent output (and, for a tty run, the
//! screen it draws, for an attach that asks), and serves a tiny
//! framed protocol on a Unix socket: attach, input, resize, signal, close.
//! It also works out its own run state (the [`detect`] heuristics, kept in a
//! state file), so nothing needs a daemon to know whether a run is working or
//! waiting on input.
//! It depends on nothing else in Relay so a daemon crash — or upgrade — never
//! costs a session.
//!
//! The library half ([`proto`], [`client`], [`exit`], [`screen`], [`detect`]) is what `rld`
//! links; the binary half ([`holder`], [`backend`]) is the process itself.

pub mod backend;
pub mod client;
pub mod detect;
pub mod exit;
pub mod holder;
pub mod proto;
pub mod ring;
pub mod screen;
pub mod state;
