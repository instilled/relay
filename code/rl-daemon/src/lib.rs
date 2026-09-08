//! `rld` as a library so tests (and `rl-cli`'s tests) can run a daemon in-process.

pub mod git;
pub mod handlers;
pub mod router;
pub mod server;
pub mod supervisor;

pub use server::{Daemon, ServeOptions, serve, serve_with};
