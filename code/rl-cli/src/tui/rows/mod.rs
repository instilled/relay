//! The one list: questions and runs, grouped by what they belong to, each
//! where it came and staying there, and named in words rather than ids and
//! paths.
//!
//! A group is a run's ref, else its folder — never its branch: a folder is one
//! project whichever branch it is on, and its header says which. Groups come in
//! order of name, until you move one (shift-↑/↓ on its header): then in yours,
//! and what you have not placed after it by name; `everywhere` comes last.
//! Inside a group, runs and flows stand in the order they came — a run by when
//! it started, a flow by its oldest run on the list — the newest at the
//! bottom; nothing that happens to a row moves it. A question stands right
//! under the run that asked it, or, that run not being in the group, at the
//! bottom of it. A workspace path reads as the name it was registered under.
//!
//! A flow's step runs, once they have ended, fold into one row per flow —
//! its name, how the last few went, and where its latest firing stands — so
//! a flow firing every thirty seconds is one row, not a screen, and it never
//! grows into one: its firings are in the pane. The row stands in the
//! project the flow is `in`, wherever its runs ran; a flow with no `in`
//! stands under `everywhere`. A run still going, or waiting on you, is never
//! folded. A flow with a yes has its row from that yes on — what starts it
//! and when it fires next, before it ever fired — and its row keeps all the
//! runs the digest has of it, however long ago they ended.
//!
//! Before all of it, pinned runs, in the order they were pinned.
//!
//! What you hid (`/hide`: a run, a flow, a project and everything in it) is
//! off the list, back inline only when the `hidden` toggle is on — unless it
//! comes to need you after you hid it: then it comes through, above the pinned
//! runs, whenever hidden is off.
//!
//! Agents' own sessions — Claude's that `claude agents` lists, and others',
//! that are not Relay's runs — stand under the project `rld` filed them
//! under, as a run in their folder would be, in the order they came with
//! its runs. They are agents, not a flow's; `/hide` takes one off the list
//! as it does a flow, and one that comes to need you comes through.
//!
//! A project whose rows you moved (shift-↑/↓) keeps your order: every row
//! stands where you put it, whatever comes of it, what is new enters at the
//! bottom, and `/sort` lets it go back to the order its rows came in.

use super::flows;
use super::outside;
use super::query::{Named, Query, Subject};
use crate::rank;
use jiff::{SignedDuration, Timestamp};
use rl_core::time::fmt_age;
use rl_proto::decision::Decision;
use rl_proto::flow::{Firing, Flow, FlowStatus};
use rl_proto::methods::{Digest, DigestItem, Hidden, Order, Outside};
use rl_proto::types::{Cause, Checkout, Confidence, Run, RunKind, RunState};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

mod build;
mod group;
mod model;
mod name;
mod order;
mod word;

pub(crate) use build::*;
pub(crate) use group::*;
pub(crate) use model::*;
pub(crate) use name::*;
pub(crate) use order::*;
pub(crate) use word::*;

#[cfg(test)]
mod tests;

/// Ended runs stay on the list this long — but a flow's with a row of its
/// own, which stay as long as the digest has them.
pub const RECENT: SignedDuration = SignedDuration::from_hours(24);

/// The group, and the word, for the flows with no `in` and their runs
/// outside every project. It comes after the projects.
pub const EVERYWHERE: &str = "everywhere";

/// Its key among the groups: nothing a folder or a ref could be.
pub(crate) const EVERYWHERE_KEY: &str = "\u{0}everywhere";

pub use rl_proto::types::SHELL_TAG;

/// The icon slot at the left of every row: what a thing is, as one quiet
/// glyph in a colour (`icon_style`) — how it stands is its word's. Text
/// glyphs one column wide in every font and every width table, ratatui's and
/// tmux's alike, so the title after them always starts in the same column;
/// emoji were wide, loud, and not always agreed on.
pub const AGENT: &str = "✦";

/// The prompt's own caret, in the prompt's green.
pub const SHELL: &str = "❯";

pub const COMMAND: &str = "▸";

/// A flow, as its row and its chips wear it.
pub const FLOW: &str = "↻";

/// A question waiting on you.
pub const QUESTION: &str = "?";

/// A question, and the key of the row that asked it: its run's, or the
/// row of the flow that run folded into, or its firing's flow's.
pub(crate) type Asked<'a> = (Option<String>, &'a Decision);

/// On a header whose flows did not load — or on `everywhere` when one with
/// no `in`, or not known, did not — in red, before any flow waiting for a
/// yes: the folder is not what is loaded until it is fixed.
pub const BROKEN_CHIP: &str = "! flows";
