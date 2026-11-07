//! The list's state: its rows as last read, the prefs they are built with,
//! and where the cursor, the search and the footer stand.

use super::draw::ListHit;
use super::line;
use crate::badges::{self, Badges};
use crate::config::{Config, Grouping, ListPrefs};
use crate::git::{self, Checkouts};
use crate::rows::{self, Context, Line};
use crate::sources::{self, NativeRow, Row};
use crate::store::{sessions, state};
use anyhow::Result;
use jiff::Timestamp;
use rl_core::Home;
use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

/// How long a word in the footer stands.
pub const FLASH: Duration = Duration::from_secs(4);

/// A key armed by its first press, acting on its second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arm {
    Quit,
}

/// `^f`: the search being typed, and the filter it started from — what `esc`
/// puts back.
pub struct Search {
    pub line: line::Line,
    pub before: String,
}

pub struct App {
    pub home: Home,
    pub config: Config,
    /// Every row, as the files and the harnesses last said.
    pub rows: Vec<Row>,
    /// The harnesses' own sessions, as their listers last said.
    pub native: Vec<NativeRow>,
    /// The checkouts of the projects' folders.
    pub checkouts: Checkouts,
    badges: Badges,
    /// What the list is narrowed to (`query`).
    pub filter: String,
    pub search: Option<Search>,
    /// The line under the cursor, by key.
    pub sel: Option<String>,
    /// Where the list's window starts, as last drawn.
    pub scroll: Cell<usize>,
    /// Where the list last drew, for the mouse.
    pub hit: RefCell<ListHit>,
    /// How tall the list was, for paging.
    pub list_h: Cell<u16>,
    /// Whether the footer shows the shortcuts (`^h`).
    pub shortcuts: bool,
    pub flash: Option<(String, Instant)>,
    pub arm: Option<Arm>,
    pub quit: bool,
}

impl App {
    /// The list over `home`: its prefs from `config.json` and its rows from
    /// the files. Starts the badges asking, so it needs a runtime.
    pub fn new(home: Home) -> Result<App> {
        let config = Config::load(&home)?;
        let badges = Badges::new(home.plugins_dir(), badges::EVERY, badges::WAIT);
        let mut app = App {
            home,
            config,
            rows: Vec::new(),
            native: Vec::new(),
            checkouts: Checkouts::new(),
            badges,
            filter: String::new(),
            search: None,
            sel: None,
            scroll: Cell::new(0),
            hit: RefCell::default(),
            list_h: Cell::new(0),
            shortcuts: false,
            flash: None,
            arm: None,
            quit: false,
        };
        app.reload();
        Ok(app)
    }

    pub fn prefs(&self) -> &ListPrefs {
        &self.config.list
    }

    /// The rows again, from the session and hook files and the native rows
    /// last said. A project new to the list gets its checkout at once; the
    /// rest wait for the tick.
    pub fn reload(&mut self) {
        let held = sessions::list(&self.home);
        let hooked = state::list(&self.home, Timestamp::now());
        self.rows = sources::rows(&held, &hooked, &self.native);
        let folders = rows::project_folders(&self.rows, &self.config.list);
        if folders
            .iter()
            .any(|f| !self.checkouts.contains_key(f) && git::toplevel(f).is_some())
        {
            self.regit();
        }
        self.settle();
    }

    /// The projects' checkouts again, and what their badges last said.
    pub fn regit(&mut self) {
        let folders = rows::project_folders(&self.rows, &self.config.list);
        self.checkouts = git::checkouts(&folders, &self.badges, &self.config.badges);
    }

    /// The native rows, as their listers now say.
    pub fn set_native(&mut self, native: Vec<NativeRow>) {
        self.native = native;
        self.reload();
    }

    /// The list as it stands: its lines, narrowed by the filter.
    pub fn lines(&self) -> Vec<Line<'_>> {
        self.lines_with(&self.filter)
    }

    /// The list as it would stand with this filter.
    pub fn lines_with(&self, filter: &str) -> Vec<Line<'_>> {
        rows::build(
            &self.rows,
            &Context {
                prefs: &self.config.list,
                filter,
                checkouts: &self.checkouts,
            },
        )
    }

    /// Changes the list's prefs and keeps them in `config.json`: read again
    /// first, so what another list kept meanwhile stays, then changed and
    /// written. What does not write is said in the footer; the change stands
    /// on this list either way.
    pub fn change_prefs(&mut self, change: impl FnOnce(&mut ListPrefs)) {
        if let Ok(now) = Config::load(&self.home) {
            self.config = now;
        }
        change(&mut self.config.list);
        if let Err(e) = self.config.save(&self.home) {
            self.say(format!("config.json: {e:#}"));
        }
        self.settle();
    }

    pub fn toggle_agents(&mut self) {
        self.change_prefs(|p| p.show_agents = !p.show_agents);
    }

    pub fn toggle_shells(&mut self) {
        self.change_prefs(|p| p.show_shells = !p.show_shells);
    }

    pub fn toggle_hidden(&mut self) {
        self.change_prefs(|p| p.show_hidden = !p.show_hidden);
    }

    /// `^s`: by project, or by state.
    pub fn toggle_grouping(&mut self) {
        self.change_prefs(|p| {
            p.grouping = match p.grouping {
                Grouping::Project => Grouping::State,
                Grouping::State => Grouping::Project,
            }
        });
    }

    pub fn say(&mut self, s: impl Into<String>) {
        self.flash = Some((s.into(), Instant::now()));
    }

    /// What the footer says for a moment, while it still does.
    pub fn flashing(&self) -> Option<&str> {
        self.flash
            .as_ref()
            .filter(|(_, at)| at.elapsed() < FLASH)
            .map(|(m, _)| m.as_str())
    }

    /// The window's title: how things stand, as the header says it.
    pub fn title(&self) -> String {
        rows::headline(&self.lines(), &self.filter)
    }
}
