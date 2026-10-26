//! The Relay wire protocol.
//!
//! Every client of `rld` — the `rl` CLI and TUI, shell scripts, plugin hosts —
//! speaks this and nothing else: JSON-RPC 2.0, one message per line, over the
//! daemon's Unix socket. This crate is the public API; nothing here depends on
//! the daemon.

pub mod action;
pub mod brief;
pub mod codec;
pub mod config;
pub mod decision;
pub mod facts;
pub mod flow;
pub mod lead;
pub mod methods;
pub mod rpc;
pub mod types;

pub use facts::RunFacts;

/// Bumped when a message changes shape incompatibly. Reported by `daemon.ping`.
pub const PROTOCOL_VERSION: u32 = 9;
