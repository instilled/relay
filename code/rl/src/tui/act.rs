//! `impl App` — what the list does to what the cursor is on: `enter`
//! attaches, the command line starts something new, `^x` twice stops,
//! removes or hides (or asks which, for a harness's own), `^r` renames, `^t` pins, `^g` colours, `⇧↑↓` moves. What is yours to
//! keep (names, pins, colours, hides, order) goes to `config.json`; what takes a
//! moment (stopping, starting) runs in the background and says how it went
//! in the footer.

use super::app::{App, Arm, Done, Hand, Mode, Renaming, To};
use super::cmdline::{self, Detected};
use super::line;
use super::remove::{self, Removal};
use super::slash;
use super::theme;
use crate::config::{Hidden, ListPrefs};
use crate::rows::{self, Context, Line, Names};
use crate::sources::{self, Attach, Harness, Owner, Session, Source};
use crate::spawn::{self, Start};
use crate::store::sessions;
use crossterm::event::{KeyCode, KeyEvent};
use jiff::Timestamp;
use rl_detect::harness::{RenameHow, Renamed, Started};
use rl_detect::registry::Profile;
use std::path::PathBuf;

/// The size a session starts at when the list cannot say its own.
const DEFAULT_SIZE: (u16, u16) = (120, 32);

/// A native row's key.
fn native_key(harness: &str, id: &str) -> String {
    format!("n:{harness}:{id}")
}

/// What attaches to `id`, one of `a`'s own a start just began in `cwd`.
fn hand_new(a: &dyn Harness, id: &str, cwd: &std::path::Path, title: String) -> Option<Hand> {
    let name = &a.info().name;
    match a.attach(&Session::started(name, id, cwd)) {
        rl_detect::harness::Attach::Argv(argv) => Some(Hand {
            to: To::Argv { argv },
            title,
            key: native_key(name, id),
        }),
        _ => None,
    }
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
        let (harness, source, state) = (r.harness.clone(), r.source, r.state);
        // Its harness lists it as open in a terminal of its own still.
        let open = matches!(r.session.owner, Owner::Elsewhere { open: true, .. });
        let to = match &r.attach {
            Attach::Held(name) => {
                let name = name.clone();
                match sessions::list(&self.home).into_iter().find(|h| h.meta.name == name) {
                    None => return self.say(format!("{title} is gone")),
                    Some(h) if h.status == sessions::Status::Lost => {
                        return self.say(format!(
                            "{title} is lost: its holder is not answering · ctrl+x removes it"
                        ));
                    }
                    Some(h) => To::Held(Box::new(h.meta)),
                }
            }
            Attach::Argv(argv) => To::Argv { argv: argv.clone() },
            Attach::CarryOn(start) => {
                let start = start.clone();
                return self.carry_on(start, &harness, title, key);
            }
            Attach::None if open => {
                return self.say(format!("{title} runs in another terminal · switch to it there"));
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

    /// Carries a conversation open in another terminal on, as its harness
    /// starts it (`Harness::start`), then attaches to it: in the harness's
    /// own background, else as a session Relay holds.
    fn carry_on(&mut self, start: rl_detect::harness::Start, harness: &str, title: String, from: String) {
        self.say(format!("carrying {title} on…"));
        let a = sources::harness_of(harness);
        let home = self.home.clone();
        let (cols, rows) = crossterm::terminal::size().unwrap_or(DEFAULT_SIZE);
        self.work.spawn(async move {
            let (name, cwd) = (a.info().name.clone(), start.cwd.clone());
            let run = tokio::task::spawn_blocking(move || a.start(&start)).await;
            match run {
                Ok(Ok(Started::Background(id))) => Done {
                    said: format!("{title} carries on in {name}'s background"),
                    select: Some(native_key(&name, &id)),
                    // A new session on its conversation: the old row's
                    // name, pin, colour and place are the new one's.
                    carried: Some((from, native_key(&name, &id))),
                    hand: hand_new(a, &id, &cwd, title),
                    ..Done::said("")
                },
                Ok(Ok(Started::Here(argv))) => Done {
                    hand: Some(Hand {
                        to: To::Here { argv, cwd },
                        title: title.clone(),
                        key: String::new(),
                    }),
                    ask_native: true,
                    ..Done::said(format!("{title} carries on here"))
                },
                Ok(Ok(Started::Held(argv))) => {
                    let start = Start {
                        harness: Some(name),
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
                }
                Ok(Err(e)) => Done::gave_up(format!("carry {title} on: {e:#}")),
                Err(e) => Done::gave_up(format!("carry {title} on: {e}")),
            }
        });
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
    /// rest as its first prompt; else the default (`list.default`) — an
    /// agent, the whole line its prompt, or the line run in a shell (as a
    /// `!` line always is) — in the folder under the cursor. A line of
    /// several lines goes whole: an agent's prompt keeps its newlines, a
    /// shell runs them in order. The line is cleared once it is on its way;
    /// a harness that is not here keeps it, and says so.
    pub(super) fn run_line(&mut self) {
        let text = self.cmd.line.text().trim().to_string();
        if slash::is_command(&text) {
            return self.run_command();
        }
        let cwd = self.line_folder();
        let Some(d) = self.detected() else {
            return;
        };
        let payload = cmdline::payload(&text, &d).to_string();
        match d {
            // A `/` line went to `run_command` above.
            Detected::Command(_) | Detected::UnknownCommand => {}
            Detected::Agent {
                profile,
                missing: Some(bin),
                ..
            } => self.say(format!("{bin} is not on PATH · {} can't start", profile.name)),
            Detected::Agent {
                profile, missing: None, ..
            } => {
                if payload.starts_with('-') {
                    return self.say("a prompt can't start with `-`: the agent would read it as a flag");
                }
                self.cmd.clear();
                self.start_agent(profile, &payload, cwd);
            }
            Detected::Shell => {
                self.cmd.clear();
                self.start_shell(&payload, cwd);
            }
        }
    }

    /// Starts `profile`'s agent in `cwd`, in the background; the cursor goes
    /// to it once it is there. Its harness says where (`Harness::start`): in
    /// a background of its own, not held by Relay; where it begins one only
    /// in a terminal (Codex with no prompt), here, handed the terminal as an
    /// attach is; else held by Relay.
    fn start_agent(&mut self, profile: &Profile, prompt: &str, cwd: PathBuf) {
        let what = profile.name.clone();
        self.say(format!("starting {what}…"));
        let prompt = (!prompt.is_empty()).then(|| prompt.to_string());
        let held = |argv| Start {
            harness: Some(profile.name.clone()),
            cwd: cwd.clone(),
            argv,
            shell: false,
            cols: 0,
            rows: 0,
        };
        let Some(a) = sources::harness(&what) else {
            let start = held(profile.prompt_args(prompt.as_deref().unwrap_or("")));
            return self.hold(start, what);
        };
        let held = held(Vec::new());
        let home = self.home.clone();
        let size = crossterm::terminal::size().unwrap_or(DEFAULT_SIZE);
        self.work.spawn(async move {
            let start = rl_detect::harness::Start {
                cwd: cwd.clone(),
                prompt,
                ..Default::default()
            };
            let started = tokio::task::spawn_blocking(move || a.start(&start)).await;
            match started {
                Ok(Ok(Started::Background(id))) => Done {
                    select: Some(native_key(&a.info().name, &id)),
                    ..Done::said(format!("started {what} {id} in its background · enter attaches"))
                },
                Ok(Ok(Started::Here(argv))) => Done {
                    hand: Some(Hand {
                        to: To::Here { argv, cwd },
                        title: what.clone(),
                        key: String::new(),
                    }),
                    ask_native: true,
                    ..Done::said(format!("{what} runs here · it keeps its session in its own background"))
                },
                Ok(Err(e)) => Done::gave_up(format!("start {what}: {e:#}")),
                Err(e) => Done::gave_up(format!("start {what}: {e}")),
                // No background: Relay holds it.
                Ok(Ok(Started::Held(argv))) => held_start(&home, Start { argv, ..held }, size, &what).await,
            }
        });
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
    fn hold(&mut self, start: Start, what: String) {
        let home = self.home.clone();
        let size = crossterm::terminal::size().unwrap_or(DEFAULT_SIZE);
        self.work
            .spawn(async move { held_start(&home, start, size, &what).await });
    }

    /// What background work came to: the list read again, and the cursor
    /// where it said — once that line is there, where it is not yet: a
    /// session in a harness's background is listed when the harness is next
    /// asked, which is now. The terminal goes where it said, if anywhere —
    /// not while a name is being typed: it is not taken from
    /// under the keys, and the footer says how to get there.
    pub fn done(&mut self, d: Done) {
        if let Some((key, name)) = &d.renamed {
            // The harness's name now, at once, not at its next listing; and
            // Relay keeps none beside it.
            for n in self.native.rows.iter_mut() {
                if !name.is_empty() && sources::key_of(n) == *key {
                    n.name = name.clone();
                }
            }
            if self.config.list.names.get(key) == Some(name) {
                self.change_prefs(|p| {
                    p.names.remove(key);
                });
            }
        }
        if let Some((from, to)) = &d.carried {
            let at = Timestamp::now();
            self.change_prefs(|p| p.carry(from, to, at));
        }
        if let Some(key) = &d.forget {
            let key = key.clone();
            self.change_prefs(|p| forget(p, &key));
            // Gone from its harness too: off the list now, not at its next
            // listing.
            self.native.rows.retain(|r| sources::key_of(r) != key);
        }
        self.reload();
        if let Some(c) = d.setup {
            self.checked(c);
        }
        // Work with nothing to say (the start's check) leaves the footer be.
        if !d.said.is_empty() {
            self.say(d.said);
        }
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
        let plan = match self.removal() {
            Ok(p) => p,
            Err(why) => return self.say(why),
        };
        match armed {
            Some(Arm::Remove(at)) if at == self.sel => self.remove(plan),
            Some(Arm::Remove(_)) => self.say("the line moved · ctrl+x again"),
            _ => {
                self.arm = Some(Arm::Remove(self.sel.clone()));
                self.say(format!("press ctrl+x again to {}", plan.hint()));
            }
        }
    }

    /// What `^x` would do to the line under the cursor, or why nothing.
    pub(super) fn removal(&self) -> Result<Removal, String> {
        match self.selected() {
            Some(line) => {
                let held = sessions::list(&self.home);
                remove::plan(&line, &self.config.list, |name| {
                    held.iter().find(|h| h.meta.name == name).map(|h| h.status.clone())
                })
            }
            None => Err("nothing here to remove".into()),
        }
    }

    /// Does what was planned for a line: stops, removes, hides or brings it
    /// back.
    pub(super) fn remove(&mut self, plan: Removal) {
        match plan {
            Removal::Stop { session, title } => {
                let a = sources::harness_of(&session.harness);
                let then = if a.actions(&session).remove {
                    " · ctrl+x again removes it"
                } else {
                    ""
                };
                self.say(format!("stopping {title}…"));
                self.work.spawn(async move {
                    let run = tokio::task::spawn_blocking(move || a.stop(&session)).await;
                    match run {
                        Ok(Ok(())) => Done {
                            ask_native: true,
                            ..Done::said(format!("stopped {title}{then}"))
                        },
                        Ok(Err(e)) => Done::said(format!("stop {title}: {e:#}")),
                        Err(e) => Done::said(format!("stop {title}: {e}")),
                    }
                });
            }
            Removal::Ask(ask) => self.ask(ask),
            Removal::Remove { session, key, title } => {
                // Whatever was kept for it goes with it: a session started
                // later by the same name is a new one. One still going is
                // stopped first, by its harness.
                let a = sources::harness_of(&session.harness);
                self.near = self.near_keys();
                self.say(format!("removing {title}…"));
                self.work.spawn(async move {
                    let run = tokio::task::spawn_blocking(move || a.remove(&session)).await;
                    match run {
                        Ok(Ok(())) => Done {
                            forget: Some(key),
                            ask_native: true,
                            ..Done::said(format!("removed {title}"))
                        },
                        // What its harness said, as it said it: the row stays.
                        Ok(Err(e)) => Done::said(format!("remove {title}: {e:#}")),
                        Err(e) => Done::said(format!("remove {title}: {e}")),
                    }
                });
            }
            Removal::Hide { key, title } => {
                let near = self.near_keys();
                let at = Timestamp::now();
                self.change_prefs(|p| {
                    p.hidden.retain(|h| h.key != key);
                    p.hidden.push(Hidden { key: key.clone(), at });
                });
                self.say(format!("hid {title} · ctrl+o shows the hidden"));
                if !self.keys().contains(&key) {
                    self.select_near(&near);
                }
            }
            Removal::Unhide { key, title } => {
                self.change_prefs(|p| p.hidden.retain(|h| h.key != key));
                self.say(format!("brought back {title}"));
            }
            Removal::Forget { key, folder, title } => self.forget_project(key, folder, title),
        }
    }

    /// What to do with one of a harness's own, asked in the footer: the
    /// keys are the choice's till one is taken or `esc` (`key_ask`).
    pub(super) fn ask(&mut self, ask: remove::Ask) {
        self.search = None;
        self.help = false;
        self.mode = Mode::Ask(ask);
    }

    /// A key while a choice is asked: one of its keys takes that choice,
    /// `enter` the first offered (the stop, where there is one), `esc`
    /// nothing. Any other key is not an answer, and the choice stands.
    pub(super) fn key_ask(&mut self, k: KeyEvent) {
        let Mode::Ask(ask) = &self.mode else {
            return;
        };
        let plain = !k
            .modifiers
            .intersects(crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT);
        let taken = match k.code {
            KeyCode::Esc => {
                self.mode = Mode::List;
                return;
            }
            KeyCode::Enter => Some(ask.first()),
            KeyCode::Char(c) if plain => ask.take(c),
            _ => None,
        };
        if let Some(plan) = taken {
            self.mode = Mode::List;
            self.remove(plan);
        }
    }

    /// `^r` (or `/rename` with no name, `by`): a name of yours for a session
    /// or a project, typed where it stands, over the name it has now, which
    /// stands selected. A harness's own session keeps its name too, beside
    /// yours: the harness has no way to rename it.
    pub(super) fn rename_here(&mut self, by: &str) {
        match self.rename_target(by) {
            Ok(RenameTarget { key, was, own, outside }) => {
                self.search = None;
                self.help = false;
                let mut l = line::Line::default();
                let at = was.len();
                l.set_with_cursor(was.clone(), at);
                // Selected whole: what is typed replaces it.
                l.select_all();
                self.mode = Mode::Rename(Renaming {
                    key,
                    line: l,
                    was,
                    own,
                    outside,
                });
            }
            Err(why) => self.say(why),
        }
    }

    /// What the line under the cursor is called, and its own name, if it
    /// is one of yours to name; else why not, said as `by` would.
    pub(super) fn rename_target(&self, by: &str) -> Result<RenameTarget, String> {
        let names = Names::new(&self.config.list.names);
        match self.selected() {
            Some(Line::Group(h)) if h.is_project() => {
                let own = h
                    .workspace
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok(RenameTarget {
                    key: h.key.clone(),
                    was: h.name.clone(),
                    own,
                    outside: None,
                })
            }
            Some(Line::Session(r)) if r.source == Source::Held => Ok(RenameTarget {
                key: r.key.clone(),
                was: names.title(r).to_string(),
                own: r.name.clone(),
                outside: None,
            }),
            // The harness's own, or one its hooks told of: a name of Relay's,
            // beside the harness's.
            Some(Line::Session(r)) => Ok(RenameTarget {
                key: r.key.clone(),
                was: names.title(r).to_string(),
                own: r.name.clone(),
                outside: Some(r.harness.clone()),
            }),
            _ => Err(format!("{by} renames a session or a project")),
        }
    }

    pub(super) fn key_rename(&mut self, k: KeyEvent) {
        let Mode::Rename(r) = &mut self.mode else {
            return;
        };
        // `home` and `end` go to the name's ends, and `^a` to its start, as a
        // shell's line does: the filters wait till the name is done.
        let ctrl = k.modifiers == crossterm::event::KeyModifiers::CONTROL;
        match k.code {
            KeyCode::Home => return r.line.home(),
            KeyCode::Char('a') if ctrl => return r.line.home(),
            KeyCode::End => return r.line.end(),
            _ => {}
        }
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
        let name = r.line.text().to_string();
        self.apply_name(
            RenameTarget {
                key: r.key,
                was: r.was,
                own: r.own,
                outside: r.outside,
            },
            &name,
        );
    }

    /// The name kept for a line; empty, or its own, goes back to its own.
    /// A session's harness says how a name lands (`Harness::actions`): in
    /// the harness at once (Codex's thread, on its daemon), where the name is
    /// Relay's until the harness has taken it and the harness's own after;
    /// at Relay's next start of it (Claude's, `-n`), where Relay keeps it
    /// till then; or in Relay alone. Cleared, it is cleared there too where
    /// the harness can.
    pub(super) fn apply_name(&mut self, r: RenameTarget, name: &str) {
        let name = name.trim().to_string();
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
        self.select(Some(r.key.clone()));
        let session = self
            .rows
            .iter()
            .find(|row| row.key == r.key)
            .map(|row| row.session.clone())
            .filter(|_| !back || name.is_empty());
        let a = session.as_ref().map(|s| sources::harness_of(&s.harness));
        let how = match (&session, a) {
            (Some(s), Some(a)) => a.actions(s).rename,
            _ => RenameHow::RelayOnly,
        };
        let (Some(session), Some(a), RenameHow::Now) = (session, a, &how) else {
            return self.say(match (&r.outside, back, how) {
                (_, true, _) => format!("back to {}", r.own),
                (Some(harness), false, RenameHow::AtNextStart) => {
                    format!("named in relay · {harness} takes it at its next start")
                }
                (Some(harness), false, _) => format!("named in relay · {harness} still calls it {}", r.own),
                (None, false, _) => format!("renamed to {name}"),
            });
        };
        let who = a.info().name.clone();
        self.say(if name.is_empty() {
            format!("back to {} · clearing its name in {who}…", r.own)
        } else {
            format!("renaming it {name} in {who}…")
        });
        let key = r.key;
        self.work.spawn(async move {
            let to = name.clone();
            let run = tokio::task::spawn_blocking(move || a.rename(&session, &to)).await;
            match run {
                Ok(Renamed::Now) => Done {
                    renamed: Some((key, name.clone())),
                    ask_native: true,
                    ..Done::said(if name.is_empty() {
                        format!("cleared its name in {who}")
                    } else {
                        format!("renamed to {name} · in {who} too")
                    })
                },
                Ok(Renamed::RelayOnly(Some(e))) if name.is_empty() => {
                    Done::said(format!("back to its name in relay · {who} kept its own: {e}"))
                }
                Ok(Renamed::RelayOnly(Some(e))) => Done::said(format!("named in relay · {who} didn't take it: {e}")),
                Ok(_) => Done::said(format!("named in relay · {who} keeps its own for now")),
                Err(e) => Done::said(format!("rename: {e}")),
            }
        });
    }

    /// `^t`: the row under the cursor to the band at the top, or back among
    /// the rest. The cursor goes with it.
    pub(super) fn toggle_pin(&mut self, by: &str) {
        let Some(Line::Session(r)) = self.selected() else {
            return self.say(format!("{by} pins a session"));
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

    /// `^g`: the row's own colour on to the next of the palette, and after
    /// the last none. A harness's colour for the row still wins on screen,
    /// and the footer says so.
    pub(super) fn cycle_colour(&mut self) {
        let (key, harness) = match self.colour_target("ctrl+g") {
            Ok(t) => t,
            Err(why) => return self.say(why),
        };
        let next = theme::next_colour(self.config.list.colors.get(&key).map(String::as_str));
        self.set_colour(key, harness, next.map(str::to_string));
    }

    /// The row under the cursor's key, and the colour its harness picked for
    /// it, if any; or why there is none to colour, said as `by` would.
    pub(super) fn colour_target(&self, by: &str) -> Result<(String, Option<String>), String> {
        let Some(Line::Session(r)) = self.selected() else {
            return Err(format!("{by} colours a session"));
        };
        let harness = r
            .color
            .as_deref()
            .filter(|c| theme::picked(c).is_some())
            .map(str::to_string);
        Ok((r.key.clone(), harness))
    }

    /// Your own colour for the row `key`, or none, kept; the footer says so,
    /// and that its harness's wins where it picked one.
    pub(super) fn set_colour(&mut self, key: String, harness: Option<String>, colour: Option<String>) {
        self.change_prefs(|p| match &colour {
            Some(c) => {
                p.colors.insert(key.clone(), c.clone());
            }
            None => {
                p.colors.remove(&key);
            }
        });
        let yours = colour.as_deref().unwrap_or("none");
        self.say(match harness {
            Some(h) => format!("{h}'s colour wins · yours: {yours}"),
            None => format!("colour: {yours}"),
        });
        self.select(Some(key));
    }

    /// `⇧↑↓`: the row under the cursor one place up or down inside its
    /// project, which keeps that order from then on — or, on a header, the
    /// project among the projects. The cursor stays on it.
    pub(super) fn reorder(&mut self, up: bool) {
        let Some(key) = self.sel.clone() else {
            return self.say("nothing here to move");
        };
        let order = {
            let shown = self.lines();
            // Every row, nothing narrowing it — hidden ones too — so what a
            // toggle, the search or a hide leaves out keeps its place, and
            // a row moves the same whether hidden ones are shown or not.
            let all = ListPrefs {
                show_agents: true,
                show_shells: true,
                show_hidden: true,
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

/// Starts a session Relay holds, at `size`, and says how it went; the cursor
/// goes to it.
async fn held_start(home: &rl_core::Home, mut start: Start, size: (u16, u16), what: &str) -> Done {
    (start.cols, start.rows) = size;
    match spawn::start(home, start).await {
        Ok(meta) => Done {
            select: Some(format!("r:{}", meta.name)),
            ..Done::said(format!("started {} · enter attaches", meta.name))
        },
        Err(e) => Done::said(format!("start {what}: {e:#}")),
    }
}

/// A line you may name: its key, what it is called now, and its own name —
/// what an empty one goes back to.
pub struct RenameTarget {
    pub key: String,
    pub was: String,
    pub own: String,
    /// The harness that still calls it `own`, for a session that is its own.
    pub outside: Option<String>,
}

/// What was kept for a row, let go of once the row is.
fn forget(p: &mut ListPrefs, key: &str) {
    p.names.remove(key);
    p.colors.remove(key);
    p.pins.retain(|k| k != key);
    p.hidden.retain(|h| h.key != key);
    for o in &mut p.order {
        o.rows.retain(|k| k != key);
    }
    p.order.retain(|o| !o.rows.is_empty());
}
