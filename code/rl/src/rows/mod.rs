//! The one list: every session — held by Relay, told of by an agent's hooks,
//! or one of a harness's own — grouped by what it belongs to, each where it
//! came and staying there, and named in words rather than ids and paths.
//!
//! By project (the default), a group is a row's folder — never its branch: a
//! folder is one project whichever branch it is on, and its header says
//! which. A row stands under the folder you named (`names`, `g:<folder>`)
//! that its folder is in, the deepest where they nest, else under its
//! repository's root (a linked worktree is a root of its own), else under its
//! own folder. A project's header carries its folder's checkout.
//! Groups come in order of name, until you move one (shift-↑/↓ on its
//! header): then in yours, and what you have not placed after it by name.
//! Inside a group, rows stand in the order they came — by when each started,
//! the newest at the bottom; nothing that happens to a row moves it. A folder
//! you named is on the list even with nothing in it.
//!
//! By state (`^s`), the groups are Needs input · Working · Done, as
//! `claude agents` has them, and inside each the oldest first.
//!
//! Before all of it, pinned rows, in the order they were pinned.
//!
//! What you hid (a row, or a project and everything in it) is off the list,
//! back inline only when the `hidden` toggle is on — unless it comes to need
//! you after you hid it: then it comes through, above the pinned rows,
//! whenever hidden is off. What needs you but is off the list — hidden, or a
//! toggle dropped it — is hinted on its project's header: `! 2 need you`.
//!
//! A project whose rows you moved (shift-↑/↓) keeps your order: every row
//! stands where you put it, whatever comes of it, and what is new enters at
//! the bottom.

use crate::config::{Grouping, Hidden, ListPrefs, Order};
use crate::git::{Checkout, Checkouts};
use crate::query::{Named, Query, Subject};
use crate::rank;
use crate::sources::Row;
use jiff::Timestamp;
use rl_core::state::{Confidence, RunKind, RunState};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

mod build;
mod group;
mod model;
mod name;
mod order;
mod word;

pub use build::*;
pub use group::*;
pub use model::*;
pub use name::*;
pub use order::*;
pub use word::*;

#[cfg(test)]
mod tests;

/// The icon slot at the left of every row: what a thing is, as one quiet
/// glyph in a colour — how it stands is its word's. Text glyphs one column
/// wide in every font and every width table, so the title after them always
/// starts in the same column.
pub const AGENT: &str = "✦";

/// The prompt's own caret, in the prompt's green.
pub const SHELL: &str = "❯";

pub const COMMAND: &str = "▸";
