//! What the list is made of: its lines, a group's header, and what it is
//! built from.

use super::*;

/// One line of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line<'a> {
    /// A group: a project, or a state heading.
    Group(Head),
    Session(&'a Row),
}

impl<'a> Line<'a> {
    /// A stable key, so the cursor survives a rebuild: a row's own, a
    /// project's `g:<folder>`, a heading's `s:<state>`.
    pub fn key(&self) -> String {
        match self {
            Line::Group(h) => h.key.clone(),
            Line::Session(r) => r.key.clone(),
        }
    }

    /// The row a line is.
    pub fn row(&self) -> Option<&'a Row> {
        match self {
            Line::Session(r) => Some(r),
            Line::Group(_) => None,
        }
    }
}

/// The headings of the by-state grouping, in the order they stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Heading {
    NeedsInput,
    Working,
    Done,
}

impl Heading {
    pub const ALL: [Heading; 3] = [Heading::NeedsInput, Heading::Working, Heading::Done];

    /// The heading a state stands under.
    pub fn of(s: RunState) -> Heading {
        match s {
            RunState::NeedsInput | RunState::Blocked => Heading::NeedsInput,
            RunState::Starting | RunState::Working | RunState::Stale => Heading::Working,
            RunState::Idle | RunState::Finished | RunState::Failed | RunState::Stopped => Heading::Done,
        }
    }

    pub fn words(self) -> &'static str {
        match self {
            Heading::NeedsInput => "Needs input",
            Heading::Working => "Working",
            Heading::Done => "Done",
        }
    }

    /// Its line's key.
    pub fn key(self) -> &'static str {
        match self {
            Heading::NeedsInput => "s:needs-input",
            Heading::Working => "s:working",
            Heading::Done => "s:done",
        }
    }
}

/// A group's header line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// `g:<folder>`, or a heading's `s:<state>`.
    pub key: String,
    /// The folder, with `~` for home; empty for a heading.
    pub place: String,
    /// The folder itself, so actions can act on it; empty for a heading.
    pub workspace: PathBuf,
    /// The folder by your name for it, else its own; or the heading's words.
    pub name: String,
    /// Your name for it, where that is not its folder's own.
    pub own: Option<String>,
    /// How many of its rows need you but are off the list now — hidden, or a
    /// toggle dropped them. The header hints at them so nothing urgent is
    /// silently buried; `0` when everything that needs you is shown.
    pub attention: usize,
    /// The heading it is, grouped by state.
    pub heading: Option<Heading>,
}

impl Head {
    pub fn heading(h: Heading, attention: usize) -> Self {
        Self {
            key: h.key().into(),
            place: String::new(),
            workspace: PathBuf::new(),
            name: h.words().into(),
            own: None,
            attention,
            heading: Some(h),
        }
    }

    /// Whether it is a project's header, with a folder to act on.
    pub fn is_project(&self) -> bool {
        self.heading.is_none()
    }
}

/// What the list is built from besides its rows: the prefs (grouping,
/// toggles, order, hidden, pins, names) and the search.
pub struct Context<'a> {
    pub prefs: &'a ListPrefs,
    pub filter: &'a str,
}

impl Context<'_> {
    pub fn names(&self) -> Names<'_> {
        Names::new(&self.prefs.names)
    }

    pub fn pinned(&self, r: &Row) -> bool {
        self.prefs.pins.contains(&r.key)
    }
}
