//! `rld` as a library so tests (and `rl-cli`'s tests) can run a daemon in-process.

pub mod action;
pub mod assistant;
pub mod attach;
pub mod badges;
pub mod config;
pub mod decisions;
pub mod digest;
pub mod flows;
pub mod gc;
pub mod git;
pub mod handlers;
pub mod import;
pub mod outside;
pub mod recap;
pub mod report;
pub mod resume;
pub mod router;
pub mod server;
pub mod setup;
pub mod summary;
pub mod supervisor;
pub mod transcript;
pub mod workspace;

pub use server::{Daemon, ServeOptions, serve, serve_with};
