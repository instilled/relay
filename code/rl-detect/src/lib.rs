//! How `rld` knows what a run is doing. Principle 3: signals over scraping,
//! scraping as the floor, confidence always visible.
//!
//! - [`registry`]: which CLI is this (`detect/*.yaml`, user-overridable), and
//!   what signals does it offer.
//! - [`pty`]: the floor — output-idle timers and prompt-line patterns over the
//!   holder's byte stream. Only ever `guess` or `likely`.
//! - [`claude`]: exact signals from Claude Code hooks, and the settings file
//!   that makes Claude send them.
//! - [`user_hooks`]: the agents' own hooks (Claude's, Codex's, Gemini's), set in
//!   the user's own files on a yes, and what their events say.
//! - [`machine`]: the one place a `(current, signal)` pair becomes a
//!   transition, where exact signals beat guesses.

pub mod ansi;
pub mod claude;
pub mod machine;
pub mod pty;
pub mod registry;
pub mod user_hooks;

pub use machine::{Current, Signal, Transition, step};
pub use registry::{Profile, Registry, Resolved};
