//! The list's state: its rows as last read, the prefs they are built with,
//! and where the cursor, the search and the footer stand.

use super::cmdline::{self, CmdLine, Commands};
use super::draw::ListHit;
use super::line;
use super::live::Said;
use crate::badges::{self, Badges};
use crate::config::{Config, Grouping, ListPrefs};
use crate::git::{self, Checkouts};
use crate::rows::{self, Context, Head, Line};
use crate::setup;
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
    /// `^x` twice (or `/rm`) on one of a harness's own: what to do with it,
    /// asked in the footer until a choice is taken or `esc`.
    Ask(super::remove::Ask),
}

/// `^r` under way: the line's key, what is typed, the name it had, and its
/// own name — what an empty one goes back to.
pub struct Renaming {
    pub key: String,
    pub line: line::Line,
    pub was: String,
    pub own: String,
    /// The harness whose own session it is, where it still calls it `own`.
    pub outside: Option<String>,
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
    Argv { argv: Vec<String> },
    /// A harness run on a pty here, in `cwd`, where it begins a session of
    /// its own (Codex with no prompt: its TUI).
    Here { argv: Vec<String>, cwd: PathBuf },
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
    /// What `setup::check` said, after the start's check or a `/setup`.
    pub setup: Option<Checked>,
    /// A row its harness has renamed (its key, and the name): the name is
    /// the harness's own now, and Relay keeps none of its own for it.
    pub renamed: Option<(String, String)>,
    /// A row its harness carried on as a new one (the old key, the new):
    /// what the list keeps for the old is the new one's, the old hidden.
    pub carried: Option<(String, String)>,
}

/// How setup stands, as `setup::check` last said: what needs setting up, or
/// why it could not tell.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub needs: Result<Vec<setup::Need>, String>,
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
            setup: None,
            renamed: None,
            carried: None,
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

/// The list as last built, by what it was built with: `build` is the costly
/// part of a key or a draw, so it is done once per change rather than once
/// per question asked of the list. Rows by their place in `App::rows`.
struct Built {
    filter: String,
    slots: Vec<Slot>,
}

enum Slot {
    Group(Head),
    Row(usize),
}

pub struct App {
    pub home: Home,
    pub config: Config,
    /// When `config.json` was last read or written here: one changed since
    /// (`rl mcp` carrying a row on) is read again (`heard_config`).
    config_at: Option<std::time::SystemTime>,
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
    /// What setup runs with (`/setup`, and the start's check); none where
    /// this `rl` could not say where it is.
    pub setup_host: Option<setup::Host>,
    /// How setup stands, once the start's check has said.
    pub setup: Option<Result<Vec<setup::Need>, String>>,
    /// `/setup` asked for the wizard: the terminal goes to it next
    /// (`main_loop`).
    pub wizard: bool,
    /// `/report` asked for the report: the terminal goes to it next
    /// (`main_loop`).
    pub reporting: bool,
    /// The drift notice as last read, and when its file was last written.
    pub drift: RefCell<(Option<std::time::SystemTime>, Option<String>)>,
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
    /// The list as last built (`lines`); none once what it is built from
    /// changed (`rebuild`).
    built: RefCell<Option<Built>>,
}

impl App {
    /// The list over `home`: its prefs from `config.json` and its rows from
    /// the files. Starts the badges asking, so it needs a runtime.
    pub fn new(home: Home) -> Result<App> {
        let config_at = config_at(&home);
        let config = Config::load(&home)?;
        let badges = Badges::new(home.plugins_dir(), badges::EVERY, badges::WAIT);
        let mut app = App {
            home,
            config,
            config_at,
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
            setup_host: setup::Host::real().ok(),
            setup: None,
            wizard: false,
            reporting: false,
            drift: RefCell::default(),
            sel: None,
            scroll: Cell::new(0),
            hit: RefCell::default(),
            list_h: Cell::new(0),
            help: false,
            flash: None,
            arm: None,
            quit: false,
            built: RefCell::new(None),
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
        self.heard_config();
        let held = sessions::list(&self.home);
        let hooked = state::list(&self.home, Timestamp::now());
        self.rows = sources::rows(&held, &hooked, &self.native.rows, &self.native.answered);
        self.rebuild();
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

    /// `config.json` read again where another (`rl mcp`) changed it since
    /// it was last read or written here; one that does not read is passed
    /// over.
    fn heard_config(&mut self) {
        let at = config_at(&self.home);
        if at == self.config_at {
            return;
        }
        self.config_at = at;
        if let Ok(c) = Config::load(&self.home) {
            self.config = c;
        }
    }

    /// The projects' checkouts again, and what their badges last said.
    pub fn regit(&mut self) {
        let folders = rows::project_folders(&self.rows, &self.config.list);
        self.checkouts = git::checkouts(&folders, &self.badges, &self.config.badges);
        self.rebuild();
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

    /// What the harnesses' listers now say. A name you gave one of their
    /// rows that its harness now calls it too is Relay's no longer: the
    /// harness's own stands alone (a rename that reached it).
    pub fn heard(&mut self, said: Said) {
        self.native = said;
        let same: Vec<String> = self
            .native
            .rows
            .iter()
            .map(|n| (sources::key_of(n), &n.name))
            .filter(|(k, name)| self.config.list.names.get(k) == Some(*name))
            .map(|(k, _)| k)
            .collect();
        if !same.is_empty() {
            self.change_prefs(|p| {
                for k in &same {
                    p.names.remove(k);
                }
            });
        }
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

    /// The list as it stands: its lines, narrowed by the filter. Built
    /// once per change (`rebuild`, or another filter), then read from that.
    pub fn lines(&self) -> Vec<Line<'_>> {
        self.with_built(|slots| slots.iter().map(|s| self.line_of(s)).collect())
    }

    /// The line under the cursor's key, and only that one made.
    pub fn line_keyed(&self, key: &str) -> Option<Line<'_>> {
        self.with_built(|slots| {
            slots
                .iter()
                .find(|s| match s {
                    Slot::Group(h) => h.key == key,
                    Slot::Row(i) => self.rows[*i].key == key,
                })
                .map(|s| self.line_of(s))
        })
    }

    /// Keys of the list's lines, in order: every header and every row.
    pub fn keys(&self) -> Vec<String> {
        self.with_built(|slots| {
            slots
                .iter()
                .map(|s| match s {
                    Slot::Group(h) => h.key.clone(),
                    Slot::Row(i) => self.rows[*i].key.clone(),
                })
                .collect()
        })
    }

    /// What the list was built from changed: the rows, the checkouts or the
    /// prefs. Every change to them comes through here; the filter is
    /// compared as the list is read.
    pub fn rebuild(&mut self) {
        *self.built.get_mut() = None;
    }

    fn line_of(&self, s: &Slot) -> Line<'_> {
        match s {
            Slot::Group(h) => Line::Group(h.clone()),
            Slot::Row(i) => Line::Session(&self.rows[*i]),
        }
    }

    /// `f` over the list as built for the filter in force, built first
    /// where it is not.
    fn with_built<T>(&self, f: impl FnOnce(&[Slot]) -> T) -> T {
        let mut built = self.built.borrow_mut();
        if built.as_ref().is_none_or(|b| b.filter != self.filter) {
            // Every row a line holds is one of `self.rows`: found by where it is.
            let at: HashMap<*const Row, usize> = self
                .rows
                .iter()
                .enumerate()
                .map(|(i, r)| (r as *const Row, i))
                .collect();
            let slots = self
                .lines_with(&self.filter)
                .into_iter()
                .map(|l| match l {
                    Line::Group(h) => Slot::Group(h),
                    Line::Session(r) => Slot::Row(at[&(r as *const Row)]),
                })
                .collect();
            *built = Some(Built {
                filter: self.filter.clone(),
                slots,
            });
        }
        f(&built.as_ref().expect("built just now").slots)
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
                self.rebuild();
                if let Err(e) = self.config.save(&self.home) {
                    self.say(format!("config.json: {e:#}"));
                }
                self.config_at = config_at(&self.home);
            }
            Err(e) => {
                change(&mut self.config.list);
                self.rebuild();
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

    /// What the footer says for a moment, while it still does: `FLASH` for
    /// each of its lines.
    pub fn flashing(&self) -> Option<&str> {
        self.flash
            .as_ref()
            .filter(|(m, at)| at.elapsed() < FLASH * m.lines().count().max(1) as u32)
            .map(|(m, _)| m.as_str())
    }

    /// The programs on `PATH`, read once.
    pub fn commands(&self) -> &Commands {
        self.commands.get_or_init(Commands::from_env)
    }

    /// Where a line typed now starts: the project root of the row under the
    /// cursor (the project it is listed under — not its own folder, which
    /// for a session in a worktree is that worktree), or the project the
    /// cursor is on, else where `rl` was started.
    pub fn line_folder(&self) -> PathBuf {
        let here = match self.selected() {
            Some(Line::Session(r)) => Some(rows::project_of(r, &rows::Names::new(&self.config.list.names))),
            Some(Line::Group(h)) if h.is_project() => Some(h.workspace.clone()),
            _ => None,
        };
        here.filter(|p| p.is_dir()).unwrap_or_else(|| self.started_in.clone())
    }

    /// What the line typed would start, if anything is typed.
    pub fn detected(&self) -> Option<cmdline::Detected<'static>> {
        cmdline::detect(
            self.cmd.line.text(),
            rows::registry(),
            self.commands(),
            &self.config.list.default,
        )
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
            default: &self.config.list.default,
        });
    }

    /// The window's title: how things stand, as the header says it.
    pub fn title(&self) -> String {
        rows::headline(&self.lines(), &self.filter)
    }
}

/// When `config.json` in `home` was last changed; None where it is not.
fn config_at(home: &Home) -> Option<std::time::SystemTime> {
    std::fs::metadata(home.config_json()).and_then(|m| m.modified()).ok()
}
