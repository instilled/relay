//! `rld` as a library so tests (and `rl-cli`'s tests) can run a daemon in-process.

pub mod action;
pub mod assistant;
pub mod attach;
pub mod badges;
pub mod config;
pub mod decisions;
pub mod digest;
pub mod gc;
pub mod git;
pub mod graph;
pub mod handlers;
pub mod import;
pub mod reactions;
pub mod recap;
pub mod resume;
pub mod router;
pub mod rulestore;
pub mod schedule;
pub mod server;
pub mod setup;
pub mod summarise;
pub mod supervisor;
pub mod transcript;
pub mod workspace;

pub use server::{Daemon, ServeOptions, serve, serve_with};

/// Signals that a rule name matched more than one scope, carrying the
/// ready-to-show message. `handlers::RuleApprove` downcasts to this to tell
/// "ambiguous" apart from "not found" without sniffing the message text, so a
/// future wording change can't silently misclassify the two.
#[derive(Debug)]
pub struct AmbiguousName(pub String);

impl std::fmt::Display for AmbiguousName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for AmbiguousName {}
