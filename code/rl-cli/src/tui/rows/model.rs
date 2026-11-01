//! The rows themselves: what a row is, and the little shapes a header, a chip, a flow's folded row and the draw context are made of.

use super::*;

pub enum Row<'a> {
    /// A group: where it is, what it is called, and the flows that are in it.
    Group(Head),
    Question(&'a Decision),
    Run(&'a DigestItem),
    /// A flow's ended runs, as one row.
    Flow(FlowRow<'a>),
    /// One of an agent's own sessions, not a Relay run.
    Outside(&'a Outside),
}

impl<'a> Row<'a> {
    /// Selectable rows carry a stable key, so the cursor survives a re-rank. A
    /// folder header is selectable by its path.
    pub fn key(&self) -> Option<String> {
        match self {
            Row::Group(h) if h.is_everywhere() => Some(format!("g:{EVERYWHERE}")),
            Row::Group(h) => Some(format!("g:{}", h.workspace.display())),
            Row::Question(d) => Some(format!("q:{}", d.id)),
            Row::Run(i) => Some(format!("r:{}", i.run.id)),
            Row::Flow(f) => Some(f.key.clone()),
            Row::Outside(c) => Some(c.key()),
        }
    }

    /// The run a row is.
    pub fn item(&self) -> Option<&'a DigestItem> {
        match self {
            Row::Run(i) => Some(i),
            _ => None,
        }
    }
}

/// A group's header line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// The folder, with `~` for home.
    pub place: String,
    /// The folder itself, so actions can act on it.
    pub workspace: PathBuf,
    /// A ref, or a folder by the name it was registered under.
    pub name: String,
    /// The name it goes by where that is not its folder's own — a ref, or a
    /// folder registered (or renamed) under another name.
    pub own: Option<String>,
    /// The flows `in` that folder, in three numbers.
    pub tally: FlowTally,
    /// The folder's git checkout, when it is in one.
    pub checkout: Option<Checkout>,
    /// How many of the project's items need you but are off the list now —
    /// hidden, or a toggle dropped them. The header hints at them so nothing
    /// urgent is silently buried; `0` when everything that needs you is shown.
    pub attention: usize,
}

impl Head {
    /// The `everywhere` group's header: no folder, and not a project.
    pub fn everywhere() -> Self {
        Self {
            place: EVERYWHERE.into(),
            workspace: PathBuf::new(),
            name: EVERYWHERE.into(),
            own: None,
            tally: FlowTally::default(),
            checkout: None,
            attention: 0,
        }
    }

    /// Whether this is the `everywhere` group, which has no folder to act on.
    pub fn is_everywhere(&self) -> bool {
        self.workspace.as_os_str().is_empty()
    }
}

/// A project's flows in three numbers, as its line says them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlowTally {
    pub total: usize,
    /// Those with a firing under way.
    pub running: usize,
    /// Those that want you: a yes, an answer, or a failure unseen.
    pub needs: usize,
}

impl FlowTally {
    /// `3 flows · 1 running · 1 needs you`; the zero parts left out; empty with no flows.
    pub fn words(&self) -> String {
        if self.total == 0 {
            return String::new();
        }
        let mut s = format!("{} flow{}", self.total, if self.total == 1 { "" } else { "s" });
        if self.running > 0 {
            s.push_str(&format!(" · {} running", self.running));
        }
        if self.needs > 0 {
            s.push_str(&format!(" · {} needs you", self.needs));
        }
        s
    }
}

/// A flow on its project's header: `↻ nightly 4h`, `↻ tidy paused`,
/// `! deploy` when it needs a yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub name: String,
    pub status: FlowStatus,
    /// One that is on: how long until it fires.
    pub due: Option<String>,
}

impl Chip {
    pub fn needs_yes(&self) -> bool {
        self.status == FlowStatus::NeedsApproval
    }

    pub fn text(&self) -> String {
        match (&self.status, &self.due) {
            (FlowStatus::NeedsApproval, _) => format!("! {}", self.name),
            (FlowStatus::Paused, _) => format!("{FLOW} {} paused", self.name),
            (_, Some(d)) => format!("{FLOW} {} {d}", self.name),
            (_, None) => format!("{FLOW} {}", self.name),
        }
    }
}

/// A flow's runs, folded into one row: what the flow is, how it has been
/// going, and where its latest firing stands. `runs` is newest first.
#[derive(Clone)]
pub struct FlowRow<'a> {
    /// `f:<name>`.
    pub key: String,
    pub name: String,
    /// The flow's full run history (not just what a filter happened to
    /// match), newest first. Empty for a flow with a yes that has not run
    /// that the digest knows of: its row is there from its yes on.
    pub runs: Vec<&'a DigestItem>,
    /// `every 30m`, `on run.failed` — when the flow is known.
    pub when: Option<String>,
    /// Its latest firing, when the digest has it.
    pub firing: Option<&'a Firing>,
}

impl<'a> FlowRow<'a> {
    /// Its newest run; none before it has run.
    pub fn last(&self) -> Option<&'a DigestItem> {
        self.runs.first().copied()
    }
    pub fn ok(&self) -> usize {
        self.runs.iter().filter(|i| i.run.state == RunState::Finished).count()
    }
    pub fn failed(&self) -> usize {
        self.runs.iter().filter(|i| i.run.state == RunState::Failed).count()
    }
    pub fn stopped(&self) -> usize {
        self.runs.iter().filter(|i| i.run.state == RunState::Stopped).count()
    }
    pub fn unseen(&self) -> bool {
        self.runs.iter().any(|i| i.unseen)
    }
    /// Whether its last run failed.
    pub fn failed_last(&self) -> bool {
        self.last().is_some_and(|i| i.run.state == RunState::Failed)
    }
    /// What the last run came to, in its own words — `5 feedback rows to
    /// process`, or `exit 1 · 2s` where it failed. The cadence and the tally
    /// are the pane's; the marks say how it has been going. Nothing before
    /// it has run.
    pub fn summary(&self) -> String {
        self.last().map(|i| i.summary.clone()).unwrap_or_default()
    }
    /// One mark per firing — a firing's runs share its id as their chain — oldest first, at
    /// most `n` of the newest: `·` went well, `×` a step failed, `○` one was stopped.
    pub fn marks(&self, n: usize) -> String {
        let worst = |m: char| match m {
            '×' => 2,
            '○' => 1,
            _ => 0,
        };
        let mut firings: Vec<(&str, char)> = Vec::new();
        for i in &self.runs {
            let m = match i.run.state {
                RunState::Failed => '×',
                RunState::Stopped => '○',
                _ => '·',
            };
            match firings.iter_mut().find(|(chain, _)| *chain == i.run.chain) {
                Some((_, was)) if worst(m) > worst(*was) => *was = m,
                Some(_) => {}
                None => firings.push((&i.run.chain, m)),
            }
        }
        firings.iter().take(n).rev().map(|(_, m)| *m).collect()
    }
}

/// What the list is built from besides the digest.
pub struct Context<'a> {
    pub names: &'a Names,
    /// Every flow, for where its row stands and what starts it; may be empty.
    pub flows: &'a [Flow],
    pub filter: &'a str,
    /// The list toggles. Flows, agents and shells are additive — each on
    /// shows the runs it is for; off hides them. Hidden is off by default;
    /// on, what you hid stands inline in its place. Questions ignore them all.
    pub show_flows: bool,
    pub show_agents: bool,
    pub show_shells: bool,
    pub show_hidden: bool,
    pub now: Timestamp,
    /// The Relay home: an assistant's run there — a summary of everything —
    /// is Relay's own, and makes no group on the list.
    pub relay: Option<&'a Path>,
}

/// How a piece of the headline is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    /// Amber: yours to give.
    Needs,
    /// Red: broken.
    Bad,
}
