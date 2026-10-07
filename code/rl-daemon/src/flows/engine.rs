//! Firings carried out: a flow fired, each step's run started as the pure
//! [`advance`] says, and the firing moved on as those runs end.
//!
//! A firing runs on the definition it started with (`firings.def_json`),
//! whatever is edited meanwhile.

use super::Flows;
use super::ask::{option_of, question};
use super::triggers::vars;
use crate::action::{expand_home, run_spec};
use anyhow::{Context, Result, anyhow};
use jiff::Timestamp;
use rl_core::Home;
use rl_core::id::new_id;
use rl_flow::advance::{Do, Happened, advance, status};
use rl_flow::def::{Ask, Does, FlowDef, Step};
use rl_flow::template::{self, Lookup, Ref};
use rl_ledger::events::NewEvent;
use rl_ledger::firings::{self, NewFiring};
use rl_ledger::{decisions, events, flows, runs, session_io};
use rl_proto::action::{Action, Task};
use rl_proto::flow::{Firing, StepState, StepStatus};
use rl_proto::methods::{DecisionSpec, RunSpec};
use rl_proto::schedule::Overlap;
use rl_proto::types::{Cause, Event, Run, event_kind};
use rusqlite::Connection;
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// How much of a step's output is its `out`: the last of it.
const OUT_MAX: usize = 64 << 10;

/// What fired a firing; `firings.trigger` is `when`, `on` or `hand`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fired {
    /// On time.
    When,
    On,
    Hand,
}

impl Fired {
    fn as_str(self) -> &'static str {
        match self {
            Fired::When => "when",
            Fired::On => "on",
            Fired::Hand => "hand",
        }
    }
}

/// Why a flow was not fired.
#[derive(Debug)]
pub enum Refused {
    /// There is no such flow loaded.
    NotLoaded(String),
    /// The folder as it is now has no yes.
    NeedsYes(String),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::NotLoaded(name) => write!(f, "no flow `{name}` is loaded"),
            Refused::NeedsYes(name) => write!(f, "`{name}` needs a yes: rl flow approve {name}"),
        }
    }
}

impl std::error::Error for Refused {}

/// A firing moved on, and what is needed to carry out what that says.
pub(super) struct Moved {
    pub(super) def: FlowDef,
    pub(super) firing: Firing,
    /// The event that fired it, and that event's run.
    pub(super) event: Option<(Event, Option<Run>)>,
    /// When the flow's firing before this one started.
    last: Option<Timestamp>,
    /// What to do, each with what it needs, placeholders filled.
    pub(super) todo: Vec<(Do, Ready)>,
}

/// What one `Do` needs, made ready while the firing is read.
pub(super) enum Ready {
    /// A `Start`/`RunOption`'s action, or why it cannot have one.
    Run(Result<Action>),
    /// An `Ask`'s question.
    Ask(DecisionSpec),
    /// There is nothing to run or ask.
    Nothing,
}

impl Flows {
    /// Starts a firing of `name`: refused unless approved and loaded; `overlap` decides about a
    /// firing still going. Records `flow.fired`. `None` when not fired: `skip` says so in
    /// `flow.skipped`, as `queue` does for all but `when`, which stays due and says nothing.
    pub async fn fire(&self, name: &str, by: Fired, event: Option<&Event>) -> Result<Option<Firing>> {
        let _going = self.going.lock().await;
        let n = name.to_string();
        let (flow, open) = self
            .ledger
            .call(move |c| {
                let open: Vec<Firing> = firings::open(c)?.into_iter().filter(|f| f.flow == n).collect();
                Ok((flows::get(c, &n)?, open))
            })
            .await?;
        let Some(flow) = flow.filter(|f| f.removed_at.is_none()) else {
            return Err(Refused::NotLoaded(name.to_string()).into());
        };
        if !flow.approved() {
            return Err(Refused::NeedsYes(name.to_string()).into());
        }
        let def: FlowDef = serde_json::from_value(flow.def.clone()).context("the flow as loaded")?;
        if let Some(going) = open.first() {
            match def.overlap {
                Overlap::Queue if by == Fired::When => return Ok(None),
                Overlap::Skip | Overlap::Queue => {
                    let data = json!({ "flow": name, "firing": going.id, "reason": "overlap" });
                    let summary = format!("flow `{name}` is still firing");
                    self.record(&def, event_kind::FLOW_SKIPPED, summary, data).await?;
                    return Ok(None);
                }
                Overlap::Kill => self.kill(name, &open).await,
            }
        }

        let id = new_id().context("a firing id")?;
        let now = Timestamp::now();
        let names: Vec<String> = def.steps.iter().map(|s| s.name.clone()).collect();
        let def_json = serde_json::to_string(&flow.def)?;
        let mut data = json!({ "flow": name, "firing": id, "trigger": by.as_str() });
        if let Some(e) = event {
            data["event"] = json!(e.id);
        }
        let mut fired = self.event(&def, event_kind::FLOW_FIRED, format!("flow `{name}` fired"), data);
        let (n, i, ev) = (name.to_string(), id.clone(), event.map(|e| e.id));
        self.ledger
            .call(move |c| {
                firings::insert(
                    c,
                    &NewFiring {
                        id: &i,
                        flow: &n,
                        def_json: &def_json,
                        trigger: by.as_str(),
                        event: ev,
                        at: now,
                        steps: &names,
                    },
                )?;
                // When it last fired, from where a step's `RELAY_LAST_FIRING` reads it too.
                if by == Fired::When
                    && let (Some(last), Some(data)) = (firings::before(c, &i)?, fired.data.as_mut())
                {
                    data["last"] = json!(last);
                }
                events::append(c, &fired)?;
                flows::set_fired(c, &n, now)
            })
            .await?;
        info!(flow = name, firing = id, trigger = by.as_str(), "flow fired");
        self.step(&id, Happened::Started).await?;
        let i = id.clone();
        self.ledger
            .call(move |c| firings::get(c, &i))
            .await?
            .with_context(|| format!("firing {id}"))
            .map(Some)
    }

    /// Ends every open firing of `name`, for `overlap: kill`: a running step's run is stopped,
    /// and its end stops the firing; a step waiting, on a question or a lost run, stops now,
    /// its question dismissed. The caller holds `going`.
    async fn kill(&self, name: &str, open: &[Firing]) {
        let mut stop = VecDeque::new();
        for f in open {
            for s in &f.steps {
                match (s.status, s.run.as_deref()) {
                    (StepStatus::Running, Some(run)) => {
                        if let Err(e) = self.supervisor.stop(run, false).await {
                            warn!(flow = name, run, error = %format!("{e:#}"), "stopping the last firing");
                        }
                    }
                    (StepStatus::Waiting | StepStatus::Lost, _) => {
                        stop.push_back((f.id.clone(), Happened::Stopped { step: s.name.clone() }));
                    }
                    _ => {}
                }
            }
        }
        if let Err(e) = self.follow(stop).await {
            warn!(flow = name, error = %format!("{e:#}"), "stopping the last firing");
        }
    }

    /// Hands one event to the firings it is about: a step's run ending, a step's question answered.
    pub(crate) async fn on_event(&self, e: &Event) -> Result<()> {
        if e.kind.starts_with("decision.") {
            return self.on_answer(e).await;
        }
        let Some(run) = e.run.clone() else { return Ok(()) };
        let how: fn(String) -> Happened = match e.kind.as_str() {
            event_kind::RUN_FINISHED => |step| Happened::Ended { step, ok: true },
            event_kind::RUN_FAILED => |step| Happened::Ended { step, ok: false },
            event_kind::RUN_STOPPED => |step| Happened::Stopped { step },
            event_kind::RUN_LOST => |step| Happened::Lost { step },
            _ => return Ok(()),
        };
        let _going = self.going.lock().await;
        let Some((firing, step)) = self.ledger.call(move |c| firings::by_run(c, &run)).await? else {
            return Ok(());
        };
        self.step(&firing, how(step)).await
    }

    /// Moves one firing on from what happened, and does what that says, and so on for what
    /// that leads to — in this firing or, for a question taken over, another. The caller
    /// holds `going`.
    pub(super) async fn step(&self, firing: &str, h: Happened) -> Result<()> {
        self.follow(VecDeque::from([(firing.to_string(), h)])).await
    }

    /// Each of what happened, in turn, as [`step`](Self::step) does it.
    pub(super) async fn follow(&self, mut queue: VecDeque<(String, Happened)>) -> Result<()> {
        while let Some((firing, h)) = queue.pop_front() {
            let (id, home) = (firing.clone(), self.home.clone());
            let Some(m) = self.ledger.call(move |c| moved(c, &home, &id, &h)).await? else {
                continue;
            };
            queue.extend(self.carry_out(&m).await);
        }
        Ok(())
    }

    /// Does what a firing moved on says, and returns what that led to. A step that cannot
    /// start or ask is said in `flow.skipped`: a run step moves on as if it failed, an ask
    /// step as if unanswered.
    pub(super) async fn carry_out(&self, m: &Moved) -> Vec<(String, Happened)> {
        let mut next = Vec::new();
        let firing = m.firing.id.as_str();
        for (todo, ready) in &m.todo {
            match todo {
                Do::Start(step) | Do::RunOption(step) => {
                    let started = match ready {
                        Ready::Run(Ok(a)) => self.start(m, step, a).await,
                        Ready::Run(Err(e)) => Err(anyhow!("{e:#}")),
                        _ => Err(anyhow!("it has nothing to run")),
                    };
                    if let Err(e) = started {
                        self.not_started(&m.def, firing, step, &e).await;
                        let ended = Happened::Ended {
                            step: step.clone(),
                            ok: false,
                        };
                        next.push((firing.to_string(), ended));
                    }
                }
                Do::Ask(step) => {
                    let asked = match ready {
                        Ready::Ask(spec) => self.ask(m, step, spec.clone()).await,
                        _ => Err(anyhow!("it has nothing to ask")),
                    };
                    match asked {
                        Ok(None) => {}
                        Ok(Some((was, step))) => next.push((was, Happened::Unanswered { step })),
                        Err(e) => {
                            self.not_started(&m.def, firing, step, &e).await;
                            next.push((firing.to_string(), Happened::Unanswered { step: step.clone() }));
                        }
                    }
                }
                Do::End(how) => {
                    info!(flow = m.def.name, firing, status = how.as_str(), "firing ended");
                    let data = json!({ "flow": m.def.name, "firing": firing, "status": how.as_str() });
                    let summary = format!("flow `{}` {}", m.def.name, how.as_str());
                    self.say(&m.def, event_kind::FLOW_ENDED, summary, data).await;
                }
            }
        }
        for step in self.dismiss_if_stopped(m).await {
            next.push((firing.to_string(), Happened::Unanswered { step }));
        }
        next
    }

    /// Says that a step did not start, and why.
    pub(super) async fn not_started(&self, def: &FlowDef, firing: &str, step: &str, e: &anyhow::Error) {
        let (flow, reason) = (&def.name, format!("{e:#}"));
        warn!(flow, firing, step, reason, "step did not start");
        let data = json!({ "flow": flow, "firing": firing, "step": step, "reason": reason });
        let summary = format!("flow `{flow}` step `{step}` did not start: {reason}");
        self.say(def, event_kind::FLOW_SKIPPED, summary, data).await;
    }

    /// Starts one step's run, in the firing's chain, and records it as the step's.
    async fn start(&self, m: &Moved, step: &str, action: &Action) -> Result<()> {
        let spec = self.spec(m, step, action)?;
        let run = self.supervisor.start_on(spec, m.firing.event).await?;
        let (firing, s, id) = (m.firing.id.clone(), step.to_string(), run.id.clone());
        self.ledger
            .call(move |c| firings::set_step_run(c, &firing, &s, &id))
            .await?;
        info!(
            flow = m.def.name,
            firing = m.firing.id,
            step,
            run = run.id,
            "step started"
        );
        Ok(())
    }

    /// What a step's run is started with: no tty and no input, in the firing's chain, the
    /// flow's folder first on its `PATH` and the firing's names in its env.
    pub(super) fn spec(&self, m: &Moved, step: &str, action: &Action) -> Result<RunSpec> {
        let def = &m.def;
        let at = def.steps.iter().find(|s| s.name == step).context("no such step")?;
        let dir = self.home.flow_dir(&def.name);
        let cwd = self.cwd(def, at, &m.firing, m.event.as_ref());
        let cause = Cause::Flow {
            flow: def.name.clone(),
            step: step.to_string(),
        };
        let name = format!("{} · {step}", def.name);
        let chain = Some(m.firing.id.clone());
        let mut spec = run_spec(action, &name, &dir, Some(&cwd), &self.registry, cause, chain)?;
        spec.tty = false;
        spec.input = false;
        let path = std::env::var_os("PATH").unwrap_or_default();
        let env = [
            ("RELAY_FLOW", def.name.clone()),
            ("RELAY_FIRING", m.firing.id.clone()),
            ("RELAY_STEP", step.to_string()),
            ("RELAY_FLOW_DIR", dir.to_string_lossy().into_owned()),
            ("PATH", format!("{}:{}", dir.display(), path.to_string_lossy())),
        ];
        spec.env.extend(env.map(|(k, v)| (k.to_string(), v)));
        if m.firing.trigger == "when" {
            let last = m.last.map(|t| t.to_string()).unwrap_or_default();
            spec.env.push(("RELAY_LAST_FIRING".into(), last));
        }
        if let Some((e, _)) = &m.event {
            spec.env.push(("RELAY_EVENT_KIND".into(), e.kind.clone()));
            spec.env.push(("RELAY_EVENT_SUMMARY".into(), e.summary.clone()));
            spec.env
                .push(("RELAY_EVENT_RUN".into(), e.run.clone().unwrap_or_default()));
            spec.env.push(("RELAY_EVENT_ID".into(), e.id.to_string()));
            spec.env.push(("RELAY_EVENT".into(), serde_json::to_string(e)?));
        }
        Ok(spec)
    }

    /// Where a step runs: its `in`, else the flow's; with neither, where an `on` firing's
    /// event happened; else the Relay home.
    pub(super) fn cwd(
        &self,
        def: &FlowDef,
        step: &Step,
        firing: &Firing,
        event: Option<&(Event, Option<Run>)>,
    ) -> PathBuf {
        if let Some(p) = step.in_.as_deref().or(def.in_.as_deref()) {
            return expand_home(p);
        }
        event
            .filter(|_| firing.trigger == "on")
            .and_then(|(e, run)| event_folder(e, run.as_ref()))
            .unwrap_or_else(|| self.home.path().to_path_buf())
    }

    /// One of the flow's own events, in its `in` or the Relay home.
    fn event(&self, def: &FlowDef, kind: &str, summary: String, data: serde_json::Value) -> NewEvent {
        let workspace = match &def.in_ {
            Some(p) => expand_home(p),
            None => self.home.path().to_path_buf(),
        };
        NewEvent {
            at: Timestamp::now(),
            group_key: workspace.to_string_lossy().into_owned(),
            workspace,
            run: None,
            kind: kind.to_string(),
            summary,
            data: Some(data),
        }
    }

    pub(super) async fn record(
        &self,
        def: &FlowDef,
        kind: &str,
        summary: String,
        data: serde_json::Value,
    ) -> Result<()> {
        let ev = self.event(def, kind, summary, data);
        self.ledger.call(move |c| events::append(c, &ev).map(|_| ())).await
    }

    /// The same, where a firing must move on whatever: a failure is logged, not returned.
    async fn say(&self, def: &FlowDef, kind: &str, summary: String, data: serde_json::Value) {
        if let Err(e) = self.record(def, kind, summary, data).await {
            warn!(flow = def.name, kind, error = %format!("{e:#}"), "recording a flow's event");
        }
    }
}

/// The firing `id` moved on from `h` and saved, in one go; `None` when there is no such firing.
fn moved(c: &Connection, home: &Home, id: &str, h: &Happened) -> Result<Option<Moved>> {
    let Some(def) = def_of(c, id)? else {
        return Ok(None);
    };
    let mut steps = firings::steps(c, id)?;
    // A firing just written has every step pending: started, it is laid out afresh.
    if *h == Happened::Started && steps.values().all(|s| *s == StepStatus::Pending) {
        steps.clear();
    }
    let firing = firings::get(c, id)?.context("the firing")?;
    let event = event_of(c, &firing)?;
    let facts = Facts::new(c, &firing.steps, event.as_ref());
    let todo = advance(&def, &mut steps, h, &facts);
    let over = todo.iter().find_map(|d| match d {
        Do::End(s) => Some(*s),
        _ => None,
    });
    let now = over.map(|_| Timestamp::now());
    firings::save(c, id, &steps, over.unwrap_or_else(|| status(&steps)), now)?;
    laid_out(c, home, id, def, todo).map(Some)
}

/// The definition firing `id` runs on; `None` when there is no such firing.
pub(super) fn def_of(c: &Connection, id: &str) -> Result<Option<FlowDef>> {
    let Some(def) = firings::def_json(c, id)? else {
        return Ok(None);
    };
    serde_json::from_str(&def).context("the firing's definition").map(Some)
}

/// The firing `id` as saved, with what `todo` needs made ready.
pub(super) fn laid_out(c: &Connection, home: &Home, id: &str, def: FlowDef, todo: Vec<Do>) -> Result<Moved> {
    let firing = firings::get(c, id)?.context("the firing, saved")?;
    let event = event_of(c, &firing)?;
    let facts = Facts::new(c, &firing.steps, event.as_ref());
    let dir = home.flow_dir(&def.name);
    let todo = todo
        .into_iter()
        .map(|d| {
            let ready = match &d {
                Do::Start(s) => run_of(&def, s).map(|a| Ready::Run(filled(a, &dir, &facts))),
                Do::RunOption(s) => option_of(c, &def, &firing.steps, s)?.map(|a| Ready::Run(filled(&a, &dir, &facts))),
                Do::Ask(s) => ask_of(&def, s).map(|a| Ready::Ask(question(a, &facts))),
                Do::End(_) => None,
            };
            Ok((d, ready.unwrap_or(Ready::Nothing)))
        })
        .collect::<Result<_>>()?;
    let last = firings::before(c, id)?;
    Ok(Moved {
        def,
        firing,
        event,
        last,
        todo,
    })
}

/// The event that fired `firing`, and that event's run.
fn event_of(c: &Connection, firing: &Firing) -> Result<Option<(Event, Option<Run>)>> {
    let Some(e) = firing.event else { return Ok(None) };
    Ok(events::get(c, e)?.map(|e| {
        let run = e.run.as_deref().and_then(|r| runs::get(c, r).ok().flatten());
        (e, run)
    }))
}

fn run_of<'a>(def: &'a FlowDef, step: &str) -> Option<&'a Action> {
    match &def.steps.iter().find(|s| s.name == step)?.does {
        Does::Run(a) => Some(a),
        Does::Ask(_) => None,
    }
}

fn ask_of<'a>(def: &'a FlowDef, step: &str) -> Option<&'a Ask> {
    match &def.steps.iter().find(|s| s.name == step)?.does {
        Does::Ask(a) => Some(a),
        Does::Run(_) => None,
    }
}

/// The action with its agent prompt's placeholders filled; a command is never filled. A
/// `prompt_file` (relative to the flow's folder) is read and becomes the prompt, filled.
fn filled(action: &Action, dir: &Path, facts: &dyn Lookup) -> Result<Action> {
    let mut a = action.clone();
    if let Some(Task::Agent {
        prompt, prompt_file, ..
    }) = &mut a.run
    {
        if prompt.is_none()
            && let Some(f) = prompt_file.take()
        {
            let path = dir.join(f);
            let text = std::fs::read_to_string(&path).with_context(|| format!("prompt_file {}", path.display()))?;
            *prompt = Some(text);
        }
        if let Some(p) = prompt {
            *p = template::fill(p, facts);
        }
    }
    Ok(a)
}

/// What a firing's steps left: `{x.out}` from the step's run, `{x.choice}` and `{x.note}`
/// from its question; and what its event said, `{event.summary}`, `{event.data.x}`.
struct Facts<'a> {
    conn: &'a Connection,
    steps: &'a [StepState],
    event: Option<BTreeMap<String, String>>,
}

impl<'a> Facts<'a> {
    fn new(conn: &'a Connection, steps: &'a [StepState], event: Option<&(Event, Option<Run>)>) -> Self {
        let event = event.map(|(e, run)| vars(e, run.as_ref()));
        Self { conn, steps, event }
    }
}

impl Lookup for Facts<'_> {
    fn get(&self, r: &Ref) -> Option<String> {
        if r.head == "event" {
            return self.event.as_ref()?.get(&r.rest).cloned();
        }
        let s = self.steps.iter().find(|s| s.name == r.head)?;
        let found = match r.rest.as_str() {
            "out" => {
                let run = s.run.as_deref()?;
                session_io::tail(self.conn, run, OUT_MAX).map(|b| Some(rl_detect::ansi::strip(&b).trim().to_string()))
            }
            "choice" | "note" => decisions::get(self.conn, s.decision.as_deref()?).map(|d| {
                let d = d?;
                match r.rest.as_str() {
                    "choice" => (!d.chosen.is_empty()).then(|| d.chosen.join(", ")),
                    _ => d.note,
                }
            }),
            _ => return None,
        };
        found.unwrap_or_else(|e| {
            warn!(step = s.name, error = %format!("{e:#}"), "reading what a step left");
            None
        })
    }
}

/// The folder an event happened in: the folder its run ran in, while that is
/// still there; else the event's workspace, while that is. None when it names
/// neither.
pub fn event_folder(event: &Event, run: Option<&Run>) -> Option<PathBuf> {
    let there = |p: &Path| !p.as_os_str().is_empty() && p.is_dir();
    run.map(|r| r.cwd.clone())
        .filter(|p| there(p))
        .or_else(|| Some(event.workspace.clone()).filter(|p| there(p)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_ledger::Ledger;
    use rl_ledger::session_io::Direction;
    use rl_proto::flow::StepStatus;
    use rl_proto::types::RunKind;

    fn agent(prompt: &str) -> Action {
        Action {
            run: Some(Task::Agent {
                agent: "claude".into(),
                prompt: Some(prompt.into()),
                prompt_file: None,
                args: vec!["{probe.out}".into()],
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_prompt_takes_what_a_step_printed_and_a_command_takes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::write(dir.join("triage.md"), "Triage this:\n{probe.out}\n").unwrap();
        let from_file = |file: PathBuf| Action {
            run: Some(Task::Agent {
                agent: "claude".into(),
                prompt: None,
                prompt_file: Some(file),
                args: vec![],
            }),
            ..Default::default()
        };
        let (from_file, absolute, missing) = (
            from_file("triage.md".into()),
            from_file(dir.join("triage.md")),
            from_file("gone.md".into()),
        );
        let ledger = Ledger::open_in_memory().unwrap();
        let (prompt, command, read, read_absolute, unread) = ledger
            .call_blocking(move |c| {
                let run = Run::new(
                    "r1",
                    RunKind::Command,
                    "probe",
                    vec![],
                    "/w",
                    "/w",
                    Timestamp::UNIX_EPOCH,
                );
                runs::insert(c, &run)?;
                let said = b"\x1b[31m3 new errors\x1b[0m {triage.out}\n\n";
                session_io::append(c, "r1", Direction::Out, said, Timestamp::UNIX_EPOCH)?;
                let steps = [
                    StepState {
                        name: "probe".into(),
                        status: StepStatus::Done,
                        run: Some("r1".into()),
                        decision: None,
                    },
                    StepState {
                        name: "triage".into(),
                        status: StepStatus::Pending,
                        run: None,
                        decision: None,
                    },
                ];
                let facts = Facts::new(c, &steps, None);
                let shell = Action {
                    run: Some(Task::Shell("echo {probe.out}".into())),
                    ..Default::default()
                };
                Ok((
                    filled(&agent("look at {probe.out} and {triage.out}; {nope.out}"), &dir, &facts)?,
                    filled(&shell, &dir, &facts)?,
                    filled(&from_file, &dir, &facts)?,
                    filled(&absolute, Path::new("/elsewhere"), &facts)?,
                    filled(&missing, &dir, &facts).map(|_| ()).map_err(|e| format!("{e:#}")),
                ))
            })
            .unwrap();
        for read in [read, read_absolute] {
            let Some(Task::Agent {
                prompt, prompt_file, ..
            }) = read.run
            else {
                panic!()
            };
            assert_eq!(prompt.as_deref(), Some("Triage this:\n3 new errors {triage.out}\n"));
            assert_eq!(prompt_file, None, "read once, here, and filled");
        }
        let err = unread.unwrap_err();
        assert!(err.contains("gone.md"), "{err}");
        let Some(Task::Agent { prompt, args, .. }) = prompt.run else {
            panic!()
        };
        assert_eq!(
            prompt.as_deref(),
            Some("look at 3 new errors {triage.out} and {triage.out}; {nope.out}"),
            "stripped, trimmed, spliced once; what is not known is left as written"
        );
        assert_eq!(args, ["{probe.out}"], "an agent's args are never filled");
        assert_eq!(command.run, Some(Task::Shell("echo {probe.out}".into())));
    }
}
