//! The PTY heuristics: what a run's byte stream alone says about its state.
//! They live in the holder's crate because the holder runs them.

pub mod ansi;
pub mod heuristics;
