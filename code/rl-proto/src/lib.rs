//! The Relay wire protocol.
//!
//! Every client of `rld` — the `rl` CLI and TUI, shell scripts, plugin hosts —
//! speaks this and nothing else: JSON-RPC 2.0, one message per line, over the
//! daemon's Unix socket. This crate is the public API; nothing here depends on
//! the daemon.

pub mod action;
pub mod codec;
pub mod config;
pub mod decision;
pub mod flow;
pub mod graph;
pub mod methods;
pub mod reaction;
pub mod rpc;
pub mod rule;
pub mod schedule;
pub mod types;

/// Bumped when a message changes shape incompatibly. Reported by `daemon.ping`.
pub const PROTOCOL_VERSION: u32 = 3;
