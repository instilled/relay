//! How Relay knows what an agent is doing. Principle 3: signals over
//! scraping, scraping as the floor, confidence always visible.
//!
//! - [`registry`]: which CLI is this (`detect/*.yaml`, user-overridable), and
//!   what signals does it offer.
//! - [`pty`] (in `rl-session`, where the holder runs it): the floor —
//!   output-idle timers and prompt-line patterns over the holder's byte
//!   stream. Only ever `guess` or `likely`.
//! - [`claude`]: the settings file that makes a held Claude session call
//!   its hooks.
//! - [`sessions`]: an agent's own sessions as its profile tells of them: its
//!   hooks' file and events and what each says, how it lists them, how one
//!   is opened.
//! - [`user_hooks`]: the format of those hooks, set in the user's own files
//!   on a yes.
//! - [`harness`]: everything Relay does with one harness, one
//!   implementation per kind: [`claude_native`] Claude's (a background of
//!   its own), [`codex_native`] Codex's (its daemon), and `harness::Held`
//!   for every other profile, which Relay holds.
//! - [`mcp_entry`]: Relay's MCP server as an entry in a harness's own list
//!   of servers, for `rl setup`.

pub mod claude;
pub mod claude_native;
pub mod codex_native;
pub mod harness;
pub mod mcp_entry;
pub mod registry;
pub mod sessions;
pub mod user_hooks;

pub use rl_session::detect::{ansi, heuristics as pty};

#[cfg(test)]
mod profile_heuristics_tests;

pub use registry::{Profile, Registry};
