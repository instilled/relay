//! `impl App` — what the list does to what the cursor is on: `enter`
//! attaches, the command line starts something new, `^x` twice stops,
//! removes or hides, `^r` renames, `^t` pins, `⇧↑↓` moves. What is yours to
//! keep (names, pins, hides, order) goes to `config.json`; what takes a
//! moment (stopping, starting) runs in the background and says how it went
//! in the footer.

use super::app::{App, Arm, Done, Hand, Mode, Renaming, To};
use super::cmdline::{self, Detected};
use super::line;
use super::remove::{self, Removal};
use crate::cmd;
use crate::config::{Hidden, ListPrefs};
use crate::rows::{self, Context, Line, Names};
use crate::sources::{self, Adapter, Attach, Source};
use crate::spawn::{self, Start};
use crate::store::sessions;
use crossterm::event::{KeyCode, KeyEvent};
use jiff::Timestamp;
use rl_detect::adapter;
use rl_detect::registry::Profile;
use std::collections::HashSet;
use std::path::PathBuf;

/// The size a session starts at when the list cannot say its own.
const DEFAULT_SIZE: (u16, u16) = (120, 32);

/// Runs `argv`, which starts or carries on a session in `a`'s harness's own
/// background, in `cwd`; the id of the session it began, once the harness
/// lists it new there (`adapter::start`). `before`: the ids it listed last.
async fn in_background(
    a: &'static dyn Adapter,
    argv: Vec<String>,
    cwd: PathBuf,
    before: HashSet<String>,
) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || adapter::start(a, &argv, &cwd, &before, adapter::LISTED_WAIT)).await?
}

/// A native row's key.
fn native_key(a: &dyn Adapter, id: &str) -> String {
    format!("n:{}:{id}", a.harness())
}

impl App {
    /// `enter`: attach to the session under the cursor, the list handing the
    /// terminal over and taking it back once you let go (see `main_loop`).
    pub fn enter(&mut self) {
        let Some(line) = self.selected() else {
            return;
        };
        let r = match line {
            Line::Group(h) if h.is_project() => {
                return self.say(format!("enter attaches to a session · type to start one in {}", h.name));
            }
            Line::Group(_) => return self.say("enter attaches to a session"),
            Line::Session(r) => r,
        };
        let names = Names::new(&self.config.list.names);
        let (title, key) = (names.title(r).to_string(), r.key.clone());
        let (harness, cwd, source, state) = (r.harness.clone(), r.cwd.clone(), r.source, r.state);
        let to = match &r.attach {
            Attach::Held(name) => {
                let name = name.clone();
                match sessions::list(&self.home).into_iter().find(|h| h.meta.name == name) {
                    None => return self.say(format!("{title} is gone")),
                    Some(h) if h.status == sessions::Status::Lost => {
                        return self.say(format!("{title} is lost: its holder is not answering · ^x removes it"));
                    }
                    Some(h) => To::Held(Box::new(h.meta)),
                }
            }
            Attach::Argv(argv) => To::Argv(argv.clone()),
            Attach::CarryOn { argv, background } => {
                let argv = argv.clone();
                return self.carry_on(argv, *background, &harness, cwd, title);
            }
            Attach::None if source == Source::Hooked && state == rl_core::state::RunState::Idle => {
                return self.say(format!(
                    "waiting for {harness} to list its own sessions · enter carries {title} on once it has"
                ));
            }
            Attach::None if source == Source::Hooked => {
                let how = if state == rl_core::state::RunState::Working {
                    "busy"
                } else {
                    "waiting on you"
                };
                return self.say(format!(
                    "{title} is {how} in its own terminal · enter carries it on here once it is idle"
                ));
            }
            Attach::None => {
                return self.say(format!("{harness}'s own sessions can't be attached from the list yet"));
            }
        };
        self.hand = Some(Hand { to, title, key });
    }

    /// Carries a conversation open in another terminal on, in the background,
    /// then attaches to it: in the harness's own background (`background`),
    /// else as a session Relay holds.
    fn carry_on(&mut self, argv: Vec<String>, background: bool, harness: &str, cwd: PathBuf, title: String) {
        self.say(format!("carrying {title} on…"));
        if background {
            let Some(a) = sources::adapter(harness) else {
                return self.say(format!("{harness} has no background here"));
            };
            let before = self.listed_ids(a);
            self.work.spawn(async move {
                match in_background(a, argv, cwd, before).await {
                    Ok(id) => {
                        let key = native_key(a, &id);
                        Done {
                            said: format!("{title} carries on in {}'s background", a.harness()),
                            select: Some(key.clone()),
                            hand: Some(Hand {
                                to: To::Argv(a.attach_argv(&id)),
                                title,
                                key,
                            }),
                            ..Done::said("")
                        }
                    }
                    Err(e) => Done::gave_up(format!("carry {title} on: {e:#}")),
                }
            });
            return;
        }
        let home = self.home.clone();
        let (cols, rows) = crossterm::terminal::size().unwrap_or(DEFAULT_SIZE);
        self.work.spawn(async move {
            let start = Start {
                harness: None,
                cwd,
                argv,
                shell: false,
                cols,
                rows,
            };
            match spawn::start(&home, start).await {
                Ok(meta) => {
                    let key = format!("r:{}", meta.name);
                    Done {
                        said: format!("{title} carries on as {}", meta.name),
                        select: Some(key.clone()),
                        hand: Some(Hand {
                            title: meta.name.clone(),
                            to: To::Held(Box::new(meta)),
                            key,
                        }),
                        ..Done::said("")
                    }
                }
                Err(e) => Done::said(format!("carry {title} on: {e:#}")),
            }
        });
    }

    /// The ids `a`'s harness listed last: what a session it starts now is not.
    fn listed_ids(&self, a: &dyn Adapter) -> HashSet<String> {
        self.native
            .rows
            .iter()
            .filter(|r| r.harness == a.harness())
            .map(|r| r.id.clone())
            .collect()
    }

    /// The words once the terminal is back from an attach, and the cursor
    /// on what it was attached to.
    pub fn back(&mut self, key: &str, words: String) {
        self.reload();
        self.say(words);
        if self.keys().iter().any(|k| k == key) {
            self.select(Some(key.to_string()));
        } else if key.starts_with("n:") {
            // Carried on into a harness's background, not listed yet.
            self.awaiting = Some(key.to_string());
            self.ask_native = true;
        }
    }

    /// `enter` on the command line: the agent its first word names, with the
    /// rest as its first prompt, or the line run in a shell — in the folder
    /// under the cursor. The line is cleared once it is on its way; a
    /// harness that is not here keeps it, and says so.
    pub(super) fn run_line(&mut self) {
        let text = self.cmd.line.text().trim().to_string();
        let cwd = self.line_folder();
        match self.detected() {
            None => {}
            Some(Detected::Agent {
                profile,
                missing: Some(bin),
            }) => self.say(format!("{bin} is not on PATH · {} can't start", profile.name)),
            Some(Detected::Agent { profile, missing: None }) => {
                let (_, prompt) = cmdline::split_first(&text);
                if prompt.starts_with('-') {
                    return self.say("a prompt can't start with `-`: the agent would read it as a flag");
                }
                self.cmd.clear();
                self.start_agent(profile, prompt, cwd);
            }
            Some(Detected::Shell) => {
                self.cmd.clear();
                self.start_shell(&text, cwd);
            }
        }
    }

    /// Starts `profile`'s agent in `cwd`, in the background; the cursor goes
    /// to it once it is there. A harness with a background of its own starts
    /// it there, not held by Relay.
    fn start_agent(&mut self, profile: &Profile, prompt: &str, cwd: PathBuf) {
        let what = profile.name.clone();
        self.say(format!("starting {what}…"));
        let prompt = (!prompt.is_empty()).then_some(prompt);
        let own = sources::adapter(&what).and_then(|a| Some((a, a.start_argv(None, prompt)?)));
        if let Some((a, argv)) = own {
            let before = self.listed_ids(a);
            self.work.spawn(async move {
                match in_background(a, argv, cwd, before).await {
                    Ok(id) => Done {
                        select: Some(native_key(a, &id)),
                        ..Done::said(format!("started {what} {id} in its background · enter attaches"))
                    },
                    Err(e) => Done::gave_up(format!("start {what}: {e:#}")),
                }
            });
            return;
        }
        let start = Start {
            harness: Some(what.clone()),
            cwd,
            argv: profile.prompt_args(prompt.unwrap_or("")),
            shell: false,
            cols: 0,
            rows: 0,
        };
        self.hold(start, what);
    }

    /// Runs `line` in a shell Relay holds, in `cwd`, which stays a shell
    /// once the line is done.
    fn start_shell(&mut self, line: &str, cwd: PathBuf) {
        self.say("starting a shell…");
        let start = Start {
            harness: None,
            cwd,
            argv: spawn::shell_line_argv(line),
            shell: true,
            cols: 0,
            rows: 0,
        };
        self.hold(start, "a shell".into());
    }

    /// Starts a session Relay holds, at this terminal's size, in the
    /// background; the cursor goes to it.
    fn hold(&mut self, mut start: Start, what: String) {
        let home = self.home.clone();
        (start.cols, start.rows) = crossterm::terminal::size().unwrap_or(DEFAULT_SIZE);
        self.work.spawn(async move {
            match spawn::start(&home, start).await {
                Ok(meta) => Done {
                    select: Some(format!("r:{}", meta.name)),
                    ..Done::said(format!("started {} · enter attaches", meta.name))
                },
                Err(e) => Done::said(format!("start {what}: {e:#}")),
            }
        });
    }

    /// What background work came to: the list read again, and the cursor
    /// where it said — once that line is there, where it is not yet: a
    /// session in a harness's background is listed when the harness is next
    /// asked, which is now. The terminal goes where it said, if anywhere —
    /// not while a name is being typed: it is not taken from
    /// under the keys, and the footer says how to get there.
    pub fn done(&mut self, d: Done) {
        if let Some(key) = &d.forget {
            let key = key.clone();
            self.change_prefs(|p| forget(p, &key));
        }
        self.reload();
        self.say(d.said);
        if d.gave_up {
            self.awaiting = None;
        }
        if d.ask_native {
            self.ask_native = true;
        }
        if let Some(k) = d.select {
            if self.keys().contains(&k) {
                if matches!(self.mode, Mode::List) {
                    self.select(Some(k));
                }
            } else if k.starts_with("n:") {
                self.awaiting = Some(k);
                self.ask_native = true;
            }
        }
        if let Some(hand) = d.hand {
            if matches!(self.mode, Mode::List) {
                self.hand = Some(hand);
            } else {
                self.say(format!("{} carries on · enter on it attaches", hand.title));
            }
        }
    }

    /// Waits for all the background work, and takes what it came to.
    pub async fn finish(&mut self) {
        while let Some(r) = self.work.join_next().await {
            match r {
                Ok(d) => self.done(d),
                Err(e) => self.say(format!("{e}")),
            }
        }
    }

    /// `^x`: armed by the first press, which says what the second will do;
    /// done by the second, if the cursor has not moved between.
    pub(super) fn key_x(&mut self, armed: Option<Arm>) {
        let plan = match self.selected() {
            Some(line) => {
                let held = sessions::list(&self.home);
                remove::plan(&line, &self.config.list, |name| {
                    held.iter().find(|h| h.meta.name == name).map(|h| h.status.clone())
                })
            }
            None => Err("nothing here to remove".into()),
        };
        let plan = match plan {
            Ok(p) => p,
            Err(why) => return self.say(why),
        };
        match armed {
            Some(Arm::Remove(at)) if at == self.sel => self.remove(plan),
            Some(Arm::Remove(_)) => self.say("the line moved · ^x again"),
            _ => {
                self.arm = Some(Arm::Remove(self.sel.clone()));
                self.say(format!("press ^x again to {}", plan.hint()));
            }
        }
    }

    fn remove(&mut self, plan: Removal) {
        match plan {
            Removal::Stop { name, title } => {
                self.say(format!("stopping {title}…"));
                let home = self.home.clone();
                self.work.spawn(async move {
                    match cmd::stop(&home, &name).await {
                        Ok(()) => Done::said(format!("stopped {title} · ^x again removes it")),
                        Err(e) => Done::said(format!("stop {title}: {e:#}")),
                    }
                });
            }
            Removal::StopNative { argv, cwd, title } => {
                self.say(format!("stopping {title}…"));
                self.work.spawn(async move {
                    let run =
                        tokio::task::spawn_blocking(move || adapter::run(&argv, Some(&cwd), false, adapter::LIST_WAIT))
                            .await;
                    match run {
                        Ok(Ok(_)) => Done {
                            ask_native: true,
                            ..Done::said(format!("stopped {title}"))
                        },
                        Ok(Err(e)) => Done::said(format!("stop {title}: {e:#}")),
                        Err(e) => Done::said(format!("stop {title}: {e}")),
                    }
                });
            }
            Removal::Remove { name, title } => {
                // Whatever was kept for it goes with it: a session started
                // later by the same name is a new one.
                let next = self.next_key();
                let home = self.home.clone();
                self.work.spawn(async move {
                    match cmd::rm(&home, &name).await {
                        Ok(()) => Done {
                            select: next,
                            forget: Some(format!("r:{name}")),
                            ..Done::said(format!("removed {title}"))
                        },
                        Err(e) => Done::said(format!("remove {title}: {e:#}")),
                    }
                });
            }
            Removal::Hide { key, title } => {
                let next = self.next_key();
                let at = Timestamp::now();
                self.change_prefs(|p| {
                    p.hidden.retain(|h| h.key != key);
                    p.hidden.push(Hidden { key: key.clone(), at });
                });
                self.say(format!("hid {title} · h shows the hidden"));
                if !self.keys().contains(&key) {
                    self.select(next);
                    self.settle();
                }
            }
            Removal::Unhide { key, title } => {
                self.change_prefs(|p| p.hidden.retain(|h| h.key != key));
                self.say(format!("brought back {title}"));
            }
        }
    }

    /// The line after the cursor, else the one before: where it goes once
    /// its own line has left.
    fn next_key(&self) -> Option<String> {
        let keys = self.keys();
        let at = keys.iter().position(|k| Some(k) == self.sel.as_ref())?;
        keys.get(at + 1)
            .or_else(|| at.checked_sub(1).and_then(|i| keys.get(i)))
            .cloned()
    }

    /// `^r`: a name of yours for a Relay session or a project, typed where it
    /// stands. A harness's own sessions keep the names it gives them.
    pub(super) fn rename_here(&mut self) {
        let names = Names::new(&self.config.list.names);
        let what = match self.selected() {
            Some(Line::Group(h)) if h.is_project() => {
                let own = h
                    .workspace
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok((h.key.clone(), h.name.clone(), own))
            }
            Some(Line::Session(r)) if r.source == Source::Held => {
                Ok((r.key.clone(), names.title(r).to_string(), r.name.clone()))
            }
            Some(Line::Session(r)) => Err(format!("{} names its own sessions", r.harness)),
            _ => Err("^r renames a session of Relay's or a project".to_string()),
        };
        match what {
            Ok((key, was, own)) => {
                self.search = None;
                self.shortcuts = false;
                let mut l = line::Line::default();
                let at = was.len();
                l.set_with_cursor(was.clone(), at);
                self.mode = Mode::Rename(Renaming { key, line: l, was, own });
            }
            Err(why) => self.say(why),
        }
    }

    pub(super) fn key_rename(&mut self, k: KeyEvent) {
        let Mode::Rename(r) = &mut self.mode else {
            return;
        };
        match r.line.key(&k) {
            line::Act::Submit => {
                let Mode::Rename(r) = std::mem::replace(&mut self.mode, Mode::List) else {
                    return;
                };
                self.rename(r);
            }
            line::Act::Cancel => self.mode = Mode::List,
            line::Act::Typing | line::Act::Complete | line::Act::Nothing => {}
        }
        // Moving off the line lets the name go, and moves.
        if let KeyCode::Up | KeyCode::Down = k.code {
            self.mode = Mode::List;
            self.step(if k.code == KeyCode::Up { -1 } else { 1 });
        }
    }

    fn rename(&mut self, r: Renaming) {
        let name = r.line.text().trim().to_string();
        if name == r.was {
            return;
        }
        let back = name.is_empty() || name == r.own;
        self.change_prefs(|p| {
            if back {
                p.names.remove(&r.key);
            } else {
                p.names.insert(r.key.clone(), name.clone());
            }
        });
        self.say(if back {
            format!("back to {}", r.own)
        } else {
            format!("renamed to {name}")
        });
        self.select(Some(r.key));
    }

    /// `^t`: the row under the cursor to the band at the top, or back among
    /// the rest. The cursor goes with it.
    pub(super) fn toggle_pin(&mut self) {
        let Some(Line::Session(r)) = self.selected() else {
            return self.say("^t pins a session");
        };
        let key = r.key.clone();
        let title = Names::new(&self.config.list.names).title(r).to_string();
        let pinned = self.config.list.pins.contains(&key);
        self.change_prefs(|p| {
            p.pins.retain(|k| *k != key);
            if !pinned {
                p.pins.push(key.clone());
            }
        });
        self.say(if pinned {
            format!("unpinned {title}")
        } else {
            format!("pinned {title}")
        });
        self.select(Some(key));
    }

    /// `⇧↑↓`: the row under the cursor one place up or down inside its
    /// project, which keeps that order from then on — or, on a header, the
    /// project among the projects. The cursor stays on it.
    pub(super) fn reorder(&mut self, up: bool) {
        if self.config.list.show_hidden {
            return self.say("with hidden on the list keeps its order · h, then move it");
        }
        let Some(key) = self.sel.clone() else {
            return self.say("nothing here to move");
        };
        let order = {
            let shown = self.lines();
            // Every row, nothing narrowing it, so what a toggle or the search
            // leaves out keeps its place.
            let all = ListPrefs {
                show_agents: true,
                show_shells: true,
                show_hidden: false,
                ..self.config.list.clone()
            };
            let full = rows::build(
                &self.rows,
                &Context {
                    prefs: &all,
                    filter: "",
                    checkouts: &self.checkouts,
                },
            );
            let p = &self.config.list;
            rows::reorder(&shown, &full, &p.order, &p.pins, &key, up)
        };
        match order {
            Ok(order) => self.change_prefs(|p| {
                p.order.retain(|o| o.group != order.group);
                p.order.push(order);
            }),
            Err(why) => self.say(why),
        }
    }
}

/// What was kept for a row, let go of once the row is.
fn forget(p: &mut ListPrefs, key: &str) {
    p.names.remove(key);
    p.pins.retain(|k| k != key);
    p.hidden.retain(|h| h.key != key);
    for o in &mut p.order {
        o.rows.retain(|k| k != key);
    }
    p.order.retain(|o| !o.rows.is_empty());
}
