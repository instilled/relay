//! `rld` as a library so tests (and `rl-cli`'s tests) can run a daemon in-process.

pub mod handlers;
pub mod router;
pub mod server;

pub use server::{Daemon, serve};
