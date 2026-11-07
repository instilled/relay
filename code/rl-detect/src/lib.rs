//! How `rld` knows what a run is doing. Principle 3: signals over scraping,
//! scraping as the floor, confidence always visible.
//!
//! - [`registry`]: which CLI is this (`detect/*.yaml`, user-overridable), and
//!   what signals does it offer.
//! - [`pty`] (in `rl-session`, where the holder runs it): the floor — output-idle timers and prompt-line patterns over the
//!   holder's byte stream. Only ever `guess` or `likely`.
//! - [`claude`]: exact signals from Claude Code hooks, and the settings file
//!   that makes Claude send them.
//! - [`sessions`]: an agent's own sessions as its profile tells of them: its
//!   hooks' file and events and what each says, how it lists them, how one
//!   is opened.
//! - [`user_hooks`]: the format of those hooks, set in the user's own files
//!   on a yes.
//! - [`machine`]: the one place a `(current, signal)` pair becomes a
//!   transition, where exact signals beat guesses.

pub mod claude;
pub mod machine;

pub mod registry;
pub mod sessions;
pub mod user_hooks;

pub use rl_session::detect::{ansi, heuristics as pty};

#[cfg(test)]
mod profile_heuristics_tests;

pub use machine::{Current, Signal, Transition, step};
pub use registry::{Profile, Registry, Resolved};
