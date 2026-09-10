//! `rld` as a library so tests (and `rl-cli`'s tests) can run a daemon in-process.

pub mod action;
pub mod attach;
pub mod decisions;
pub mod digest;
pub mod git;
pub mod handlers;
pub mod router;
pub mod schedule;
pub mod server;
pub mod supervisor;

pub use server::{Daemon, ServeOptions, serve, serve_with};
