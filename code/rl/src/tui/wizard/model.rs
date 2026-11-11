//! The wizard's state, and what each key does to it: pure, so every step
//! and every way through them is a test. What it shows is `view`; what runs
//! the changes is `engine`.

use crate::rows::AGENT;
use crate::setup::{Host, Need, Part, State, What};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rl_core::Home;
use rl_detect::Registry;
use rl_detect::registry::Profile;

/// Where the wizard was started from: what its last `enter` goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Came {
    /// The list's first start: the list opens after it.
    FirstRun,
    /// `rl setup` on a terminal: it ends.
    Command,
    /// The list's `/setup`: back to the list.
    List,
}

/// The six steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Found,
    Changes,
    Applying,
    YourTurn,
    Keys,
    Now,
}

pub const STEPS: usize = 6;

impl Step {
    /// Its place among the six: 1 to 6.
    pub fn number(self) -> usize {
        match self {
            Step::Found => 1,
            Step::Changes => 2,
            Step::Applying => 3,
            Step::YourTurn => 4,
            Step::Keys => 5,
            Step::Now => 6,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Step::Found => "Your harnesses",
            Step::Changes => "What setup changes",
            Step::Applying => "Setting up",
            Step::YourTurn => "Your turn",
            Step::Keys => "Keys you'll use",
            Step::Now => "What you can do now",
        }
    }
}

/// How it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// Through to its end, or the quick way.
    Finished,
    /// `q` (or `esc` on the first step): left early.
    Skipped,
    /// `ctrl+c`: left early, and the first start opens no list after it.
    Interrupted,
}

/// One change a run makes to a harness: the part `check` named, and how
/// the wizard says it (`+ hook`, why it is wanted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub part: Part,
    /// `+` added, `~` changed.
    pub sign: char,
    pub label: String,
    pub why: String,
    /// Ticked: to be made. Unticked, it is declined (`config.json`'s
    /// `setup.declined`, by `id`).
    pub on: bool,
    /// `What::id`: `claude/settings/leftArrowOpensAgents`.
    pub id: String,
}

/// An agent harness in the registry, as the wizard shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Harn {
    pub name: String,
    pub icon: String,
    pub color: Option<(u8, u8, u8)>,
    /// Its binary is on this machine.
    pub found: bool,
    /// What setup does for it, in short (`hooks`, `mcp relay`, `← back to
    /// list`); empty, nothing.
    pub does: Vec<String>,
    /// What a run would change for it (empty: set up), or why that could
    /// not be told. Empty for one not found.
    pub need: Result<Vec<Change>, String>,
    /// What is left for you once its hooks are in (its profile's
    /// `after_install`).
    pub note: Option<String>,
}

impl Harn {
    /// Found, and something to change.
    pub fn needs(&self) -> bool {
        self.found && self.need.as_ref().is_ok_and(|c| !c.is_empty())
    }

    /// On step 2: found, and setup has something for it.
    pub fn listed(&self) -> bool {
        self.found && !self.does.is_empty()
    }

    /// Its changes; none where they could not be told.
    pub fn changes(&self) -> &[Change] {
        self.need.as_deref().unwrap_or(&[])
    }

    /// Every change of it ticked.
    pub fn all_on(&self) -> bool {
        self.changes().iter().all(|c| c.on)
    }
}

/// How one change stands while it is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    Waiting,
    Running,
    Done(Result<String, String>),
}

/// A change being applied: which harness, which of its changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub harn: usize,
    pub change: usize,
    pub run: Run,
}

/// What the engine says as it goes, by the job's place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Started(usize),
    Finished(usize, Result<String, String>),
    AllDone,
}

/// What a key asks of whoever runs the wizard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Keep what was declined (`decline`) and what was ticked again
    /// (`accept`), by id; then apply `jobs`, in order: a harness and one of
    /// its parts each (none: nothing to run).
    Apply {
        jobs: Vec<(String, What)>,
        decline: Vec<String>,
        accept: Vec<String>,
    },
}

pub struct Wizard {
    pub came: Came,
    pub harns: Vec<Harn>,
    pub step: Step,
    history: Vec<Step>,
    /// Step 2's cursor, among its rows (`rows`).
    pub cursor: usize,
    pub jobs: Vec<Job>,
    /// The quick way (`tab`): everything applied, then the summary.
    pub fast: bool,
    /// Every job is done.
    pub all_done: bool,
    /// Nothing needed doing anywhere.
    pub already: bool,
    pub ended: Option<Ended>,
    /// Frames drawn, for the spinner.
    pub tick: usize,
}

/// What setup does for a harness, in short, by its profile.
pub fn does_of(p: &Profile) -> Vec<String> {
    let mut v = Vec::new();
    if p.sessions.as_ref().is_some_and(|s| s.hooks.is_some()) {
        v.push("hooks".to_string());
    }
    if p.mcp.is_some() {
        v.push(format!("mcp {}", rl_detect::mcp_entry::SERVER));
    }
    if let Some(s) = &p.settings {
        v.extend(s.set.iter().map(|s| setting_words(&s.key, &s.value).0));
    }
    v
}

/// A setting in short, and why Relay sets it.
fn setting_words(key: &str, value: &serde_json::Value) -> (String, String) {
    match key {
        "leftArrowOpensAgents" => ("← back to list".into(), "← goes back to Relay's list".into()),
        _ => (format!("{key} {value}"), "as Relay needs it".into()),
    }
}

/// Why each part is wanted.
const WHY_HOOKS: &str = "so the list knows when it needs you";
const WHY_MCP: &str = "so agents can message each other";

/// What `check` said of a harness, as the wizard says it; what is in
/// `declined` unticked.
pub fn changes_of(p: &Profile, need: Option<Need>, declined: &[String]) -> Vec<Change> {
    let Some(need) = need else { return Vec::new() };
    need.items
        .into_iter()
        .map(|part| {
            let (label, why) = match &part.what {
                What::Hooks => ("hook".to_string(), WHY_HOOKS.to_string()),
                What::Mcp => (format!("mcp {}", rl_detect::mcp_entry::SERVER), WHY_MCP.to_string()),
                What::Setting { key, says } => p
                    .settings
                    .as_ref()
                    .and_then(|s| s.set.iter().find(|s| &s.key == key))
                    .map(|s| (format!("{} {}", s.key, s.value), setting_words(&s.key, &s.value).1))
                    .unwrap_or_else(|| (says.clone(), "as Relay needs it".into())),
            };
            let (sign, why) = match (&part.what, &part.state) {
                (What::Setting { .. }, _) => ('~', why),
                (_, State::Missing) => ('+', why),
                (_, State::Stale { was: Some(was) }) => ('~', format!("it points at {was}")),
                (_, State::Stale { was: None }) => ('~', "it is turned off".to_string()),
            };
            let id = part.what.id(&p.name);
            Change {
                on: !declined.contains(&id),
                id,
                part,
                sign,
                label,
                why,
            }
        })
        .collect()
}

/// Every agent harness in the registry, and what setup would change for
/// those found: files only, nothing written, no harness's CLI run.
pub fn gather(home: &Home, host: &Host) -> anyhow::Result<Vec<Harn>> {
    let registry = Registry::load(home)?;
    let declined = crate::setup::declined(home)?;
    Ok(registry
        .agents()
        .map(|p| {
            let found = (host.bin_of)(p).is_some();
            let does = does_of(p);
            let need = if found && !does.is_empty() {
                host.need_of(home, &p.name)
                    .map(|n| changes_of(p, n, &declined))
                    .map_err(|e| format!("{e:#}"))
            } else {
                Ok(Vec::new())
            };
            Harn {
                name: p.name.clone(),
                icon: p.icon.clone().unwrap_or_else(|| AGENT.to_string()),
                color: p.color,
                found,
                does,
                need,
                note: p
                    .sessions
                    .as_ref()
                    .and_then(|s| s.hooks.as_ref())
                    .and_then(|h| h.after_install.clone()),
            }
        })
        .collect())
}

impl Wizard {
    pub fn new(came: Came, harns: Vec<Harn>) -> Wizard {
        Wizard {
            came,
            harns,
            step: Step::Found,
            history: Vec::new(),
            cursor: 0,
            jobs: Vec::new(),
            fast: false,
            all_done: false,
            already: false,
            ended: None,
            tick: 0,
        }
    }

    /// The harnesses step 2 lists, by place.
    pub fn listed(&self) -> Vec<usize> {
        (0..self.harns.len()).filter(|i| self.harns[*i].listed()).collect()
    }

    /// The harnesses step 2's cursor moves among: those that need
    /// something.
    pub fn needy(&self) -> Vec<usize> {
        (0..self.harns.len()).filter(|i| self.harns[*i].needs()).collect()
    }

    /// The change a job applies.
    pub fn change(&self, j: &Job) -> &Change {
        &self.harns[j.harn]
            .need
            .as_ref()
            .expect("a job's harness has its changes")[j.change]
    }

    /// Whether changes are being applied now.
    pub fn running(&self) -> bool {
        !self.jobs.is_empty() && !self.all_done
    }

    pub fn key(&mut self, k: KeyEvent) -> Option<Effect> {
        if self.ended.is_some() {
            return None;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // Nothing stops a run half way: its files are being written.
        if self.running() {
            return None;
        }
        match k.code {
            KeyCode::Char('c') if ctrl => {
                self.ended = Some(Ended::Interrupted);
                return None;
            }
            KeyCode::Char('q') => {
                self.ended = Some(Ended::Skipped);
                return None;
            }
            _ => {}
        }
        match (self.step, k.code) {
            (Step::Found, KeyCode::Enter | KeyCode::Char('w')) => self.walk(),
            (Step::Found, KeyCode::Tab | KeyCode::Char('y')) => return self.go_fast(),
            (Step::Found, KeyCode::Esc) => self.ended = Some(Ended::Skipped),
            (Step::Changes, KeyCode::Up) => self.cursor = self.cursor.saturating_sub(1),
            (Step::Changes, KeyCode::Char('k')) if ctrl => self.cursor = self.cursor.saturating_sub(1),
            (Step::Changes, KeyCode::Down) => self.down(),
            (Step::Changes, KeyCode::Char('j')) if ctrl => self.down(),
            (Step::Changes, KeyCode::Char(' ')) => self.toggle(),
            (Step::Changes, KeyCode::Enter | KeyCode::Right) => return self.apply_chosen(),
            (Step::Keys, KeyCode::Char('j')) if ctrl => self.next(),
            (_, KeyCode::Esc | KeyCode::Left) => self.back(),
            (_, KeyCode::Enter | KeyCode::Right) => self.next(),
            _ => {}
        }
        None
    }

    /// Step 2's rows, in order: each harness that needs something (its
    /// header, which ticks all of it), then each of its changes.
    pub fn rows(&self) -> Vec<(usize, Option<usize>)> {
        let mut out = Vec::new();
        for i in self.needy() {
            out.push((i, None));
            out.extend((0..self.harns[i].changes().len()).map(|c| (i, Some(c))));
        }
        out
    }

    fn down(&mut self) {
        if self.cursor + 1 < self.rows().len() {
            self.cursor += 1;
        }
    }

    /// Space on step 2: the change under the cursor ticked or not; on a
    /// harness's header, all of its changes (all on, unless they all are).
    fn toggle(&mut self) {
        let Some(&(h, c)) = self.rows().get(self.cursor) else {
            return;
        };
        let all_on = self.harns[h].all_on();
        let Ok(changes) = &mut self.harns[h].need else { return };
        match c {
            Some(c) => changes[c].on = !changes[c].on,
            None => changes.iter_mut().for_each(|c| c.on = !all_on),
        }
    }

    /// A click on step 2's row `row`: the cursor to it, and it toggled.
    pub fn click(&mut self, row: usize) {
        if self.step == Step::Changes && row < self.rows().len() && !self.running() {
            self.cursor = row;
            self.toggle();
        }
    }

    fn go(&mut self, to: Step) {
        self.history.push(self.step);
        self.step = to;
    }

    fn back(&mut self) {
        if let Some(s) = self.history.pop() {
            self.step = s;
        }
    }

    /// `enter` on step 1: the walkthrough, from step 2 — or, where nothing
    /// needs doing, from step 5.
    fn walk(&mut self) {
        if self.needy().is_empty() {
            self.already = true;
            self.go(Step::Keys);
        } else {
            self.cursor = 0;
            self.go(Step::Changes);
        }
    }

    /// `tab` on step 1: every change not declined applied, then the
    /// summary.
    fn go_fast(&mut self) -> Option<Effect> {
        self.fast = true;
        let (jobs, _, _) = self.start();
        if jobs.is_empty() {
            self.already = true;
            self.ended = Some(Ended::Finished);
            return None;
        }
        Some(Effect::Apply {
            jobs,
            decline: Vec::new(),
            accept: Vec::new(),
        })
    }

    /// `enter` on step 2: what is ticked applied, what is not declined;
    /// with nothing ticked, on to the next step with nothing changed.
    fn apply_chosen(&mut self) -> Option<Effect> {
        let (jobs, decline, accept) = self.start();
        if jobs.is_empty() {
            self.go(Step::Keys);
        }
        Some(Effect::Apply { jobs, decline, accept })
    }

    /// The jobs for every change ticked, and the step that shows them
    /// (none: nothing starts); the ids of the changes unticked, and of
    /// those ticked.
    fn start(&mut self) -> (Vec<(String, What)>, Vec<String>, Vec<String>) {
        let mut jobs = Vec::new();
        let mut apply = Vec::new();
        let (mut decline, mut accept) = (Vec::new(), Vec::new());
        for i in self.needy() {
            let h = &self.harns[i];
            for (c, change) in h.changes().iter().enumerate() {
                if !change.on {
                    decline.push(change.id.clone());
                    continue;
                }
                accept.push(change.id.clone());
                jobs.push(Job {
                    harn: i,
                    change: c,
                    run: Run::Waiting,
                });
                apply.push((h.name.clone(), change.part.what.clone()));
            }
        }
        if !jobs.is_empty() {
            self.jobs = jobs;
            self.step = Step::Applying;
            // Nothing goes back past what is done.
            self.history.clear();
        }
        (apply, decline, accept)
    }

    fn next(&mut self) {
        match self.step {
            Step::Found => self.walk(),
            Step::Changes => {
                self.apply_chosen();
            }
            Step::Applying if self.notes().is_empty() => self.go(Step::Keys),
            Step::Applying => self.go(Step::YourTurn),
            Step::YourTurn => self.go(Step::Keys),
            Step::Keys => self.go(Step::Now),
            Step::Now => self.ended = Some(Ended::Finished),
        }
    }

    /// What the engine said.
    pub fn progress(&mut self, p: Progress) {
        match p {
            Progress::Started(i) => {
                if let Some(j) = self.jobs.get_mut(i) {
                    j.run = Run::Running;
                }
            }
            Progress::Finished(i, r) => {
                if let Some(j) = self.jobs.get_mut(i) {
                    j.run = Run::Done(r);
                }
            }
            Progress::AllDone => {
                self.all_done = true;
                if self.fast {
                    self.ended = Some(Ended::Finished);
                }
            }
        }
    }

    /// What is left for you, by harness: the notes of those whose hooks
    /// went in just now.
    pub fn notes(&self) -> Vec<(usize, String)> {
        let mut out: Vec<(usize, String)> = Vec::new();
        for j in &self.jobs {
            let h = &self.harns[j.harn];
            if self.change(j).part.what == What::Hooks
                && matches!(j.run, Run::Done(Ok(_)))
                && let Some(n) = &h.note
                && !out.iter().any(|(i, _)| *i == j.harn)
            {
                out.push((j.harn, n.clone()));
            }
        }
        out
    }

    /// The harnesses changes were applied to, in order, each with its
    /// jobs.
    pub fn applied(&self) -> Vec<(usize, Vec<&Job>)> {
        let mut out: Vec<(usize, Vec<&Job>)> = Vec::new();
        for j in &self.jobs {
            match out.iter_mut().find(|(h, _)| *h == j.harn) {
                Some((_, v)) => v.push(j),
                None => out.push((j.harn, vec![j])),
            }
        }
        out
    }

    /// How many changes failed.
    pub fn failed(&self) -> usize {
        self.jobs.iter().filter(|j| matches!(j.run, Run::Done(Err(_)))).count()
    }

    /// The footer's words for the list, once it is over.
    pub fn said(&self) -> String {
        if self.jobs.is_empty() {
            return if self.already {
                "setup: already set up".into()
            } else {
                "setup: nothing changed".into()
            };
        }
        let names: Vec<&str> = self
            .applied()
            .iter()
            .map(|(h, _)| self.harns[*h].name.as_str())
            .collect();
        match self.failed() {
            0 => format!("set up: {}", names.join(", ")),
            n => format!("set up: {} · {n} failed, rl setup says why", names.join(", ")),
        }
    }
}
