//! The list's state: its rows as last read, the prefs they are built with,
//! and where the cursor, the search and the footer stand.

use super::cmdline::{self, CmdLine, Commands};
use super::draw::ListHit;
use super::line;
use super::live::Said;
use crate::badges::{self, Badges};
use crate::config::{Config, Grouping, ListPrefs};
use crate::git::{self, Checkouts};
use crate::rows::{self, Context, Line};
use crate::sources::{self, Row};
use crate::store::sessions::{self, Meta};
use crate::store::state;
use anyhow::Result;
use jiff::Timestamp;
use rl_core::Home;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::task::JoinSet;

/// How long a word in the footer stands.
pub const FLASH: Duration = Duration::from_secs(4);

/// A key armed by its first press, acting on its second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arm {
    Quit,
    /// `^x`, on the line with this key.
    Remove(Option<String>),
}

/// What has the keys besides the list itself (and the search, which is
/// `App::search`).
pub enum Mode {
    List,
    /// `^r`: a name typed where it stands.
    Rename(Renaming),
}

/// `^r` under way: the line's key, what is typed, the name it had, and its
/// own name — what an empty one goes back to.
pub struct Renaming {
    pub key: String,
    pub line: line::Line,
    pub was: String,
    pub own: String,
}

/// What `enter` hands the terminal to, once the list has let go of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Hand {
    pub to: To,
    /// What it is called on the list, for the words after.
    pub title: String,
    /// Its line, for the cursor after.
    pub key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum To {
    /// A Relay-held session, straight to its holder.
    Held(Box<Meta>),
    /// A harness's own attach command, run on a pty here.
    Argv(Vec<String>),
}

/// What background work (a stop, a start, a carry-on) came to.
#[derive(Debug, Clone, PartialEq)]
pub struct Done {
    /// For the footer.
    pub said: String,
    /// The line the cursor goes to, once it is there.
    pub select: Option<String>,
    /// A row gone for good, whose name, pin, hide and place go with it.
    pub forget: Option<String>,
    /// What the terminal goes to now: a conversation carried on.
    pub hand: Option<Hand>,
    /// A start in a harness's background came to nothing: the cursor waits
    /// for no new line.
    pub gave_up: bool,
    /// The harnesses are to be asked for their sessions now: one of them
    /// changed.
    pub ask_native: bool,
}

impl Done {
    pub fn said(s: impl Into<String>) -> Done {
        Done {
            said: s.into(),
            select: None,
            forget: None,
            hand: None,
            gave_up: false,
            ask_native: false,
        }
    }

    /// A start in a harness's background that came to nothing, and why.
    pub fn gave_up(s: impl Into<String>) -> Done {
        Done {
            gave_up: true,
            ..Done::said(s)
        }
    }
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
    pub native: Said,
    /// The checkouts of the projects' folders.
    pub checkouts: Checkouts,
    badges: Badges,
    /// How each row's pull request stands, by its URL, as `gh` last said.
    pub prs: HashMap<String, String>,
    /// A line the cursor goes to once it is on the list: a session just
    /// started in a harness's background, not listed yet.
    pub awaiting: Option<String>,
    /// The harnesses are to be asked for their sessions now (`main_loop`).
    pub ask_native: bool,
    /// What the list is narrowed to (`query`).
    pub filter: String,
    pub search: Option<Search>,
    /// The command line at the foot of the list (`cmdline`).
    pub cmd: CmdLine,
    /// The programs on `PATH`, read the first time the line wants them.
    pub commands: OnceCell<Commands>,
    /// The folder `rl` was started in: where a line starts with nothing
    /// under the cursor.
    pub started_in: PathBuf,
    pub mode: Mode,
    /// Set by `enter`: what the terminal goes to next (`main_loop`).
    pub hand: Option<Hand>,
    /// Stops and starts under way.
    pub work: JoinSet<Done>,
    /// The line under the cursor, by key.
    pub sel: Option<String>,
    /// Where the list's window starts, as last drawn.
    pub scroll: Cell<usize>,
    /// Where the list last drew, for the mouse.
    pub hit: RefCell<ListHit>,
    /// How tall the list was, for paging.
    pub list_h: Cell<u16>,
    /// Whether the keys stand as a grid in the footer's place (`^h`).
    pub help: bool,
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
            native: Said::default(),
            checkouts: Checkouts::new(),
            badges,
            prs: HashMap::new(),
            awaiting: None,
            ask_native: false,
            filter: String::new(),
            search: None,
            cmd: CmdLine::default(),
            commands: OnceCell::new(),
            started_in: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            mode: Mode::List,
            hand: None,
            work: JoinSet::new(),
            sel: None,
            scroll: Cell::new(0),
            hit: RefCell::default(),
            list_h: Cell::new(0),
            help: false,
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
        self.rows = sources::rows(&held, &hooked, &self.native.rows, &self.native.answered);
        let folders = rows::project_folders(&self.rows, &self.config.list);
        if folders
            .iter()
            .any(|f| !self.checkouts.contains_key(f) && git::toplevel(f).is_some())
        {
            self.regit();
        }
        self.settle();
        if let Some(k) = self.awaiting.take() {
            if self.keys().contains(&k) {
                self.select(Some(k));
            } else {
                self.awaiting = Some(k);
            }
        }
    }

    /// The projects' checkouts again, and what their badges last said.
    pub fn regit(&mut self) {
        let folders = rows::project_folders(&self.rows, &self.config.list);
        self.checkouts = git::checkouts(&folders, &self.badges, &self.config.badges);
        // The pull requests' states, the badges' way: none asked with the
        // badges off.
        if self.config.badges.is_empty() {
            return;
        }
        let hrefs: Vec<String> = self
            .rows
            .iter()
            .filter_map(|r| r.pr.as_ref())
            .map(|p| p.href.clone())
            .collect();
        self.prs = hrefs
            .into_iter()
            .filter_map(|h| self.badges.pr_state(&h).map(|s| (h, s)))
            .collect();
    }

    /// What the harnesses' listers now say.
    pub fn heard(&mut self, said: Said) {
        self.native = said;
        self.reload();
    }

    /// The native rows, as if every lister had answered with them.
    #[cfg(test)]
    pub fn set_native(&mut self, rows: Vec<crate::sources::NativeRow>) {
        let answered = sources::native_listers()
            .iter()
            .map(|l| l.harness().to_string())
            .collect();
        self.heard(Said {
            rows,
            answered,
            failing: Vec::new(),
        });
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
    /// written. A file that does not read is never written over: the change
    /// stands on this list only, and the footer says why. What does not
    /// write is said there too.
    pub fn change_prefs(&mut self, change: impl FnOnce(&mut ListPrefs)) {
        match Config::load(&self.home) {
            Ok(now) => {
                self.config = now;
                change(&mut self.config.list);
                if let Err(e) = self.config.save(&self.home) {
                    self.say(format!("config.json: {e:#}"));
                }
            }
            Err(e) => {
                change(&mut self.config.list);
                self.say(format!("not kept: config.json does not read ({e:#})"));
            }
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

    /// The programs on `PATH`, read once.
    pub fn commands(&self) -> &Commands {
        self.commands.get_or_init(Commands::from_env)
    }

    /// Where a line typed now starts: the folder of the session under the
    /// cursor, else of its project, else where `rl` was started.
    pub fn line_folder(&self) -> PathBuf {
        let here = match self.selected() {
            Some(Line::Session(r)) => Some(r.cwd.clone()),
            Some(Line::Group(h)) if h.is_project() => Some(h.workspace.clone()),
            _ => None,
        };
        here.filter(|p| p.is_dir()).unwrap_or_else(|| self.started_in.clone())
    }

    /// What the line typed would start, if anything is typed.
    pub fn detected(&self) -> Option<cmdline::Detected<'static>> {
        cmdline::detect(self.cmd.line.text(), rows::registry(), self.commands())
    }

    /// The line's ghost again, after a key changed it.
    pub fn retyped(&mut self) {
        let (folder, name) = (self.line_folder(), self.name_here());
        let cmds = self.commands.get_or_init(Commands::from_env);
        self.cmd.typed(&cmdline::Ctx {
            folder: &folder,
            reg: rows::registry(),
            cmds,
            name: name.as_deref(),
        });
    }

    /// The window's title: how things stand, as the header says it.
    pub fn title(&self) -> String {
        rows::headline(&self.lines(), &self.filter)
    }
}
