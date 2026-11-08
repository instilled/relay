//! `rl-session`: the process that owns one run's terminal.
//!
//! `rl new` spawns one per session and `rl` is only ever a *client* of it
//! (principle 6). The holder detaches (`fork` + `setsid`), holds the PTY
//! master, keeps a ring buffer of recent output and the screen it draws (for
//! an attach that asks), and serves a tiny framed protocol on a Unix socket:
//! attach, input, resize, signal, close. It also works out its own run state
//! (the [`detect`] heuristics, kept in a state file), so nobody needs to be
//! attached to know whether a run is working or waiting on input. It depends
//! on nothing in Relay but `rl-core`, so an `rl` crash — or upgrade — never
//! costs a session.
//!
//! The library half ([`proto`], [`client`], [`exit`], [`screen`], [`detect`])
//! is what `rl` links; the binary half ([`holder`], [`backend`]) is the
//! process itself.

pub mod backend;
pub mod client;
pub mod detect;
pub mod exit;
pub mod holder;
pub mod proto;
pub mod ring;
pub mod screen;
pub mod state;
