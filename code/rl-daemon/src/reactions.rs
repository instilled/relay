//! The reactor: events → actions, and questions.
//!
//! `reactions.yaml` reads like `schedules.yaml` and is trusted the same way:
//! the user's own file is approved as it loads, a workspace's waits for `rl
//! up`. The loop reads new event rows by id, matches each against the rules
//! that are approved and unpaused, and either starts a run or raises a
//! decision. A rule never fires on what it started itself, and a throttle
//! keeps a burst of failures from opening twenty terminals.

use crate::decisions::Decisions;
use crate::supervisor::Supervisor;
use anyhow::{Context, Result, anyhow, bail};
use jiff::{SignedDuration, Timestamp};
use relay_core::Home;
use relay_core::time::parse_duration;
use relay_detect::Registry;
use relay_ledger::entries::Loaded;
use relay_ledger::{Ledger, decisions, events, meta, reactions, runs, workspaces};
use relay_proto::action::{Action, Task};
use relay_proto::decision::DecisionOption;
use relay_proto::methods::{DecisionSpec, EventFilter};
use relay_proto::reaction::{AskSpec, DEFAULT_THROTTLE, Reaction, ReactionSpec};
use relay_proto::schedule::{GLOBAL_SCOPE, ReloadReport, WORKSPACE_DIR};
use relay_proto::types::{Cause, Event, Run};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// How often the reactor looks for new events.
const TICK: Duration = Duration::from_secs(1);
/// Events older than this are history: a daemon that was down does not come
/// back and fire everything it missed.
const MAX_AGE: Duration = Duration::from_secs(300);
/// How many events one pass takes at most.
const BATCH: u32 = 200;
/// Where the reactor keeps how far it has read.
const CURSOR: &str = "reactions.cursor";

// ---- the file ----------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    reactions: BTreeMap<String, ReactionSpec>,
}

/// Where a workspace keeps its reactions.
pub fn workspace_file(workspace: &Path) -> PathBuf {
    workspace.join(WORKSPACE_DIR).join("reactions.yaml")
}

/// Reads and validates a reactions file. A missing file is an empty one.
/// Entries that fail validation are reported, the rest load.
pub fn load_file(path: &Path, in_workspace: bool) -> Result<(Vec<Loaded>, Vec<String>)> {
    if !path.is_file() {
        return Ok((Vec::new(), Vec::new()));
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: File = serde_norway::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let mut loaded = Vec::new();
    let mut errors = Vec::new();
    for (name, spec) in file.reactions {
        match validate(&name, &spec, in_workspace) {
            Ok(()) => loaded.push(Loaded {
                hash: crate::schedule::hash(&spec),
                spec_json: serde_json::to_string(&spec)?,
                name,
            }),
            Err(e) => errors.push(format!("{name}: {e:#}")),
        }
    }
    Ok((loaded, errors))
}

fn validate(name: &str, spec: &ReactionSpec, in_workspace: bool) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        bail!("names are letters, digits, `.`, `_` and `-`");
    }
    match (&spec.run, &spec.ask) {
        (Some(_), Some(_)) => bail!("`run` or `ask`, not both"),
        (None, None) => bail!("needs `run` or `ask`"),
        (None, Some(ask)) => {
            if ask.title.trim().is_empty() {
                bail!("a question needs a title");
            }
            if ask.options.is_empty() {
                bail!("a question needs at least one option");
            }
            if ask.options.iter().filter(|o| o.default).count() > 1 {
                bail!("only one option can be the default");
            }
        }
        (Some(_), None) => {}
    }
    if spec.cwd.is_none() && !in_workspace {
        bail!("needs `cwd`");
    }
    throttle(spec)?;
    Ok(())
}

/// How long this rule waits between firings.
fn throttle(spec: &ReactionSpec) -> Result<SignedDuration> {
    let text = spec.throttle.as_deref().unwrap_or(DEFAULT_THROTTLE);
    parse_duration(text)
        .or_else(|| (text.trim() == "0").then(SignedDuration::default))
        .ok_or_else(|| anyhow!("throttle: `{text}` is not a duration (10s, 5m)"))
}

// ---- matching ----------------------------------------------------------------

/// Why a rule did or did not fire, for `rl react test` and the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Fires,
    /// It does not want this kind of event.
    NotThisKind,
    /// `where` said no; which field is in the string.
    Filtered(String),
    NotApproved,
    Paused,
    /// It fired too recently.
    Throttled,
    /// The event came from something this very rule started.
    ItsOwnDoing,
}

impl Verdict {
    pub fn fires(&self) -> bool {
        *self == Verdict::Fires
    }
    pub fn why(&self) -> String {
        match self {
            Verdict::Fires => "fires".into(),
            Verdict::NotThisKind => "not this kind of event".into(),
            Verdict::Filtered(f) => format!("`where {f}` says no"),
            Verdict::NotApproved => "not approved".into(),
            Verdict::Paused => "paused".into(),
            Verdict::Throttled => "fired too recently".into(),
            Verdict::ItsOwnDoing => "this rule started it".into(),
        }
    }
}

/// Everything about an event a rule can look at: the event, the run it is
/// about, and the reaction that caused that run, if any.
pub struct Context_<'a> {
    pub event: &'a Event,
    pub run: Option<&'a Run>,
    /// The reaction that asked, when the event is about a decision.
    pub by_reaction: Option<String>,
}

/// Whether one rule fires for one event, and why not when it does not.
pub fn verdict(r: &Reaction, cx: &Context_, now: Timestamp) -> Verdict {
    if !r.spec.matches_kind(&cx.event.kind) {
        return Verdict::NotThisKind;
    }
    if !r.approved() {
        return Verdict::NotApproved;
    }
    if r.paused || r.removed_at.is_some() {
        return Verdict::Paused;
    }
    // Its own doing: a run it started, or a question it asked.
    let mine = cx.run.map(|run| run.cause == Cause::Reaction(r.name.clone())) == Some(true)
        || cx.by_reaction.as_deref() == Some(r.name.as_str());
    if mine {
        return Verdict::ItsOwnDoing;
    }
    if let Some(f) = &r.spec.filter {
        if let Some(tag) = &f.tag
            && cx.run.is_none_or(|run| !run.tags.iter().any(|t| t == tag))
        {
            return Verdict::Filtered("tag".into());
        }
        if let Some(cause) = &f.cause
            && cx.run.is_none_or(|run| !run.cause.to_string().starts_with(cause))
        {
            return Verdict::Filtered("cause".into());
        }
        if let Some(cli) = &f.cli
            && cx.run.is_none_or(|run| run.cli.as_deref() != Some(cli))
        {
            return Verdict::Filtered("cli".into());
        }
        if let Some(group) = &f.group
            && &cx.event.group_key != group
        {
            return Verdict::Filtered("group".into());
        }
    }
    if let (Some(last), Ok(gap)) = (r.last_fired_at, throttle(&r.spec))
        && !gap.is_zero()
        && last.checked_add(gap).map(|next| next > now).unwrap_or(false)
    {
        return Verdict::Throttled;
    }
    Verdict::Fires
}

/// What an event says, for `{…}` in a rule.
pub fn vars(event: &Event, run: Option<&Run>) -> BTreeMap<String, String> {
    let mut v = BTreeMap::new();
    v.insert("kind".into(), event.kind.clone());
    v.insert("summary".into(), event.summary.clone());
    v.insert("group".into(), event.group_key.clone());
    v.insert("workspace".into(), event.workspace.to_string_lossy().into_owned());
    v.insert("run".into(), event.run.clone().unwrap_or_default());
    v.insert("title".into(), run.map(|r| r.title.clone()).unwrap_or_default());
    v.insert("cause".into(), run.map(|r| r.cause.to_string()).unwrap_or_default());
    v.insert("cli".into(), run.and_then(|r| r.cli.clone()).unwrap_or_default());
    if let Some(serde_json::Value::Object(map)) = &event.data {
        for (k, val) in map {
            let text = match val {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            v.insert(format!("data.{k}"), text);
        }
    }
    v
}

/// Replaces `{name}` with what the event said. A name nothing knows is left
/// exactly as it is, so shell braces survive.
pub fn substitute(text: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                match vars.get(name) {
                    Some(value) => out.push_str(value),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The same action with every `{…}` filled in.
fn resolved(action: &Action, vars: &BTreeMap<String, String>) -> Action {
    let mut a = action.clone();
    a.run = a.run.map(|task| match task {
        Task::Shell(line) => Task::Shell(substitute(&line, vars)),
        Task::Argv(argv) => Task::Argv(argv.iter().map(|w| substitute(w, vars)).collect()),
        Task::Agent {
            agent,
            prompt,
            prompt_file,
            args,
        } => Task::Agent {
            agent,
            prompt: prompt.map(|p| substitute(&p, vars)),
            prompt_file,
            args: args.iter().map(|w| substitute(w, vars)).collect(),
        },
    });
    a.title = a.title.map(|t| substitute(&t, vars));
    a.env = a.env.iter().map(|(k, v)| (k.clone(), substitute(v, vars))).collect();
    a
}

/// The question a rule asks, with every `{…}` filled in.
fn resolved_ask(ask: &AskSpec, name: &str, vars: &BTreeMap<String, String>) -> AskSpec {
    AskSpec {
        title: substitute(&ask.title, vars),
        summary: ask.summary.as_ref().map(|s| substitute(s, vars)),
        options: ask
            .options
            .iter()
            .map(|o| DecisionOption {
                label: substitute(&o.label, vars),
                run: o.run.clone().map(|task| match task {
                    Task::Shell(line) => Task::Shell(substitute(&line, vars)),
                    Task::Argv(argv) => Task::Argv(argv.iter().map(|w| substitute(w, vars)).collect()),
                    Task::Agent {
                        agent,
                        prompt,
                        prompt_file,
                        args,
                    } => Task::Agent {
                        agent,
                        prompt: prompt.map(|p| substitute(&p, vars)),
                        prompt_file,
                        args: args.iter().map(|w| substitute(w, vars)).collect(),
                    },
                }),
                ..o.clone()
            })
            .collect(),
        multi: ask.multi,
        group: Some(
            ask.group
                .as_ref()
                .map(|g| substitute(g, vars))
                .unwrap_or_else(|| format!("reaction:{name}")),
        ),
        expires: ask.expires.clone(),
    }
}

// ---- the loop ----------------------------------------------------------------

pub struct Reactor {
    home: Home,
    ledger: Ledger,
    supervisor: Arc<Supervisor>,
    decisions: Arc<Decisions>,
    registry: Arc<Registry>,
    wake: Notify,
    shutdown: CancellationToken,
}

/// One rule against one event, as `rl react test` reports it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Trial {
    pub reaction: String,
    pub scope: String,
    pub fires: bool,
    pub why: String,
    /// What it would run, or the question it would ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

impl Reactor {
    pub fn new(
        home: Home,
        ledger: Ledger,
        supervisor: Arc<Supervisor>,
        decisions: Arc<Decisions>,
        registry: Arc<Registry>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            home,
            ledger,
            supervisor,
            decisions,
            registry,
            wake: Notify::new(),
            shutdown,
        }
    }

    /// Re-reads every file (the user's, then each workspace's) into the ledger.
    pub async fn reload(&self) -> Result<ReloadReport> {
        let mut report = self.load_scope(GLOBAL_SCOPE, &self.home.reactions_file(), true).await?;
        let workspaces = self.ledger.call(|c| workspaces::list(c)).await?;
        for w in workspaces {
            let scope = w.path.to_string_lossy().into_owned();
            match self.load_scope(&scope, &workspace_file(&w.path), false).await {
                Ok(r) => {
                    report.loaded += r.loaded;
                    report
                        .added
                        .extend(r.added.into_iter().map(|n| format!("{}:{n}", w.name)));
                    report
                        .changed
                        .extend(r.changed.into_iter().map(|n| format!("{}:{n}", w.name)));
                    report
                        .removed
                        .extend(r.removed.into_iter().map(|n| format!("{}:{n}", w.name)));
                    report
                        .errors
                        .extend(r.errors.into_iter().map(|e| format!("{}:{e}", w.name)));
                }
                Err(e) => report.errors.push(format!("{}: {e:#}", w.name)),
            }
        }
        info!(
            loaded = report.loaded,
            added = report.added.len(),
            changed = report.changed.len(),
            removed = report.removed.len(),
            errors = report.errors.len(),
            "reactions reloaded"
        );
        self.wake.notify_one();
        Ok(report)
    }

    async fn load_scope(&self, scope: &str, path: &Path, trusted: bool) -> Result<ReloadReport> {
        let (loaded, errors) = load_file(path, !trusted)?;
        let now = Timestamp::now();
        let (scope, path) = (scope.to_string(), path.to_path_buf());
        let mut report = self
            .ledger
            .call(move |c| reactions::sync(c, &scope, &path, &loaded, trusted, now))
            .await?;
        report.errors = errors;
        Ok(report)
    }

    /// One workspace's reactions, loaded and (when asked) approved. Called by
    /// `rl up`, beside the schedules.
    pub async fn up(&self, path: &Path, approve: bool) -> Result<(ReloadReport, Vec<Reaction>, Vec<String>)> {
        let scope = path.to_string_lossy().into_owned();
        let report = self.load_scope(&scope, &workspace_file(path), false).await?;
        let approved = if approve {
            let sc = scope.clone();
            self.ledger.call(move |c| reactions::approve(c, &sc, &[])).await?
        } else {
            Vec::new()
        };
        let sc = scope.clone();
        let live = self
            .ledger
            .call(move |c| reactions::list_scope(c, &sc))
            .await?
            .into_iter()
            .filter(|r| r.removed_at.is_none())
            .collect();
        self.wake.notify_one();
        Ok((report, live, approved))
    }

    /// Marks a workspace's reactions removed, as `rl workspace rm` does.
    pub async fn remove_scope(&self, path: &Path) -> Result<()> {
        let scope = path.to_string_lossy().into_owned();
        let now = Timestamp::now();
        self.ledger
            .call(move |c| reactions::remove_scope(c, &scope, now))
            .await?;
        Ok(())
    }

    pub async fn list(&self, include_removed: bool) -> Result<Vec<Reaction>> {
        self.ledger.call(move |c| reactions::list(c, include_removed)).await
    }

    /// A reaction by name; `scope` settles it when the name is in several.
    pub async fn get(&self, name: &str, scope: Option<&str>) -> Result<Reaction> {
        let n = name.to_string();
        if let Some(scope) = scope {
            let scope = self.scope_of(scope).await?;
            let s = scope.clone();
            return self
                .ledger
                .call(move |c| reactions::get(c, &s, &n))
                .await?
                .ok_or_else(|| anyhow!("no reaction named `{name}` in {scope}"));
        }
        let found = self.ledger.call(move |c| reactions::find(c, &n)).await?;
        match found.len() {
            0 => bail!("no reaction named `{name}`"),
            1 => Ok(found.into_iter().next().unwrap()),
            _ => {
                let scopes: Vec<String> = found.iter().map(|r| r.scope_label()).collect();
                bail!("`{name}` exists in {}; say which with --in", scopes.join(" and "))
            }
        }
    }

    async fn scope_of(&self, scope: &str) -> Result<String> {
        if scope == GLOBAL_SCOPE {
            return Ok(scope.to_string());
        }
        let path = std::fs::canonicalize(scope).with_context(|| format!("workspace {scope}"))?;
        let w = self
            .ledger
            .call(move |c| workspaces::get(c, &path))
            .await?
            .ok_or_else(|| anyhow!("{scope} is not a registered workspace; `rl up` there first"))?;
        Ok(w.path.to_string_lossy().into_owned())
    }

    pub async fn set_paused(&self, name: &str, scope: Option<&str>, paused: bool) -> Result<Reaction> {
        let r = self.get(name, scope).await?;
        let (sc, n) = (r.scope.clone(), r.name.clone());
        self.ledger
            .call(move |c| reactions::set_paused(c, &sc, &n, paused))
            .await?;
        self.get(&r.name, Some(&r.scope)).await
    }

    /// What every rule would do with one event, without doing any of it.
    pub async fn test(&self, event_id: i64) -> Result<Vec<Trial>> {
        let event = self
            .event(event_id)
            .await?
            .ok_or_else(|| anyhow!("no event {event_id}"))?;
        let (run, by_reaction) = self.about(&event).await?;
        let cx = Context_ {
            event: &event,
            run: run.as_ref(),
            by_reaction,
        };
        let now = Timestamp::now();
        let mut trials = Vec::new();
        for r in self.list(false).await? {
            let v = verdict(&r, &cx, now);
            let vars = vars(&event, run.as_ref());
            let action = match (&r.spec.run, &r.spec.ask) {
                (Some(_), _) => Some(resolved(&r.spec.action(), &vars).label()),
                (_, Some(ask)) => Some(format!("asks: {}", substitute(&ask.title, &vars))),
                _ => None,
            };
            trials.push(Trial {
                reaction: r.name.clone(),
                scope: r.scope_label(),
                fires: v.fires(),
                why: v.why(),
                action,
            });
        }
        Ok(trials)
    }

    async fn event(&self, id: i64) -> Result<Option<Event>> {
        let list = self
            .ledger
            .call(move |c| {
                events::list(
                    c,
                    &EventFilter {
                        limit: Some(1),
                        ..Default::default()
                    },
                )
            })
            .await?;
        // `event.list` has no id filter; the one we want is usually recent.
        if let Some(e) = list.into_iter().find(|e| e.id == id) {
            return Ok(Some(e));
        }
        let all = self.ledger.call(move |c| events::get(c, id)).await?;
        Ok(all)
    }

    /// The run an event is about, and the reaction behind it when it is about
    /// a decision.
    async fn about(&self, event: &Event) -> Result<(Option<Run>, Option<String>)> {
        let run = match &event.run {
            Some(id) => {
                let id = id.clone();
                self.ledger.call(move |c| runs::get(c, &id)).await?
            }
            None => None,
        };
        let by_reaction = match event.kind.starts_with("decision.") {
            true => match event
                .data
                .as_ref()
                .and_then(|d| d.get("decision"))
                .and_then(|v| v.as_str())
            {
                Some(id) => {
                    let id = id.to_string();
                    self.ledger.call(move |c| decisions::by_reaction(c, &id)).await?
                }
                None => None,
            },
            false => None,
        };
        Ok((run, by_reaction))
    }

    /// Reads what is new and acts on it. Returns how many events it read.
    async fn drain(&self) -> Result<usize> {
        let after = self.cursor().await?;
        let batch = self.ledger.call(move |c| events::after(c, after, BATCH)).await?;
        if batch.is_empty() {
            return Ok(0);
        }
        let rules = self.list(false).await?;
        let now = Timestamp::now();
        let oldest = now - SignedDuration::try_from(MAX_AGE).unwrap_or(SignedDuration::from_secs(300));
        let mut last = after;
        let n = batch.len();
        for event in batch {
            last = event.id;
            if event.at < oldest {
                continue; // history: the daemon was away
            }
            if rules.is_empty() {
                continue;
            }
            let (run, by_reaction) = self.about(&event).await?;
            let cx = Context_ {
                event: &event,
                run: run.as_ref(),
                by_reaction,
            };
            for r in &rules {
                let v = verdict(r, &cx, Timestamp::now());
                if !v.fires() {
                    debug!(reaction = r.name, event = event.id, why = v.why(), "not fired");
                    continue;
                }
                if let Err(e) = self.fire(r, &cx).await {
                    warn!(reaction = r.name, event = event.id, error = %format!("{e:#}"), "reaction failed");
                }
            }
        }
        self.set_cursor(last).await?;
        Ok(n)
    }

    /// Does what one rule says about one event.
    async fn fire(&self, r: &Reaction, cx: &Context_<'_>) -> Result<()> {
        let vars = vars(cx.event, cx.run);
        let now = Timestamp::now();
        let base = self.base_dir(r);
        let flow = cx.run.map(|run| run.flow.clone());

        if let Some(ask) = &r.spec.ask {
            let ask = resolved_ask(ask, &r.name, &vars);
            let spec = DecisionSpec {
                title: ask.title,
                summary: ask.summary,
                options: ask.options,
                multi: ask.multi,
                group: ask.group,
                run: cx.event.run.clone(),
                cwd: Some(base),
                expires: ask.expires,
            };
            let d = self.decisions.raise_by(spec, Some(&r.name)).await?;
            self.fired(r, now, None).await?;
            info!(reaction = r.name, decision = d.id, "reaction asked");
            return Ok(());
        }

        let action = resolved(&r.spec.action(), &vars);
        let mut spec = crate::action::run_spec(
            &action,
            &r.name,
            &base,
            Some(&base),
            &self.registry,
            Cause::Reaction(r.name.clone()),
            flow,
        )?;
        // What set it off, for the command itself to read.
        spec.env.push(("RELAY_EVENT_KIND".into(), cx.event.kind.clone()));
        spec.env.push(("RELAY_EVENT_SUMMARY".into(), cx.event.summary.clone()));
        spec.env
            .push(("RELAY_EVENT_RUN".into(), cx.event.run.clone().unwrap_or_default()));
        if let Ok(json) = serde_json::to_string(cx.event) {
            spec.env.push(("RELAY_EVENT".into(), json));
        }
        let run = self.supervisor.start(spec).await?;
        self.fired(r, now, Some(&run.id)).await?;
        info!(reaction = r.name, event = cx.event.id, run = run.id, "reaction ran");
        Ok(())
    }

    /// Where a rule's action runs: what it says, or its workspace.
    fn base_dir(&self, r: &Reaction) -> PathBuf {
        if r.scope == GLOBAL_SCOPE {
            r.spec
                .cwd
                .clone()
                .map(|c| crate::action::expand_home(&c))
                .unwrap_or_else(|| self.home.path().to_path_buf())
        } else {
            PathBuf::from(&r.scope)
        }
    }

    async fn fired(&self, r: &Reaction, at: Timestamp, run: Option<&str>) -> Result<()> {
        let (scope, name, run) = (r.scope.clone(), r.name.clone(), run.map(str::to_string));
        self.ledger
            .call(move |c| reactions::set_fired(c, &scope, &name, at, run.as_deref()))
            .await
    }

    async fn cursor(&self) -> Result<i64> {
        match self.ledger.call(|c| meta::get_i64(c, CURSOR)).await? {
            Some(n) => Ok(n),
            None => {
                // First run: start from now, not from the whole history.
                let last = self.ledger.call(|c| events::last_id(c)).await?;
                self.set_cursor(last).await?;
                Ok(last)
            }
        }
    }

    async fn set_cursor(&self, id: i64) -> Result<()> {
        self.ledger.call(move |c| meta::set_i64(c, CURSOR, id)).await
    }

    /// Runs until shutdown.
    pub async fn serve(self: Arc<Self>) {
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(TICK) => {}
            }
            if let Err(e) = self.drain().await {
                warn!(error = %format!("{e:#}"), "reactor");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_proto::types::{Confidence, RunKind, RunState, Source};

    fn spec(yaml: &str) -> ReactionSpec {
        serde_norway::from_str(yaml).unwrap()
    }

    fn reaction(name: &str, yaml: &str) -> Reaction {
        let spec = spec(yaml);
        Reaction {
            scope: GLOBAL_SCOPE.into(),
            name: name.into(),
            hash: "h".into(),
            source: "/f".into(),
            spec,
            paused: false,
            last_fired_at: None,
            last_run: None,
            removed_at: None,
            approved_hash: Some("h".into()),
        }
    }

    fn event(kind: &str) -> Event {
        Event {
            id: 1,
            at: Timestamp::now(),
            workspace: "/w".into(),
            run: Some("r1".into()),
            group_key: "main".into(),
            kind: kind.into(),
            summary: "exit 1 · 3s".into(),
            data: Some(serde_json::json!({ "exit": 1 })),
        }
    }

    fn run() -> Run {
        let mut r = Run::new(
            "r1",
            RunKind::Agent,
            "nightly tests",
            vec![],
            "/w",
            "/w",
            Timestamp::now(),
        );
        r.tags = vec!["nightly".into()];
        r.cli = Some("claude".into());
        r.cause = Cause::Schedule("nightly".into());
        r.state = RunState::Failed;
        r.source = Source::Manual;
        r.confidence = Confidence::Exact;
        r
    }

    fn cx<'a>(e: &'a Event, r: &'a Run) -> Context_<'a> {
        Context_ {
            event: e,
            run: Some(r),
            by_reaction: None,
        }
    }

    #[test]
    fn a_rule_takes_the_events_it_asked_for() {
        let r = reaction("notify", "on: run.failed\nrun: notify\ncwd: /tmp");
        let (e, run) = (event("run.failed"), run());
        assert!(verdict(&r, &cx(&e, &run), Timestamp::now()).fires());
        let other = event("run.finished");
        assert_eq!(verdict(&r, &cx(&other, &run), Timestamp::now()), Verdict::NotThisKind);
    }

    #[test]
    fn where_narrows_it() {
        let (e, run) = (event("run.failed"), run());
        let yes = reaction(
            "n",
            "on: run.failed\nwhere: { tag: nightly, cli: claude, cause: 'schedule:', group: main }\nrun: x\ncwd: /tmp",
        );
        assert!(verdict(&yes, &cx(&e, &run), Timestamp::now()).fires());
        for (field, yaml) in [
            ("tag", "on: run.failed\nwhere: { tag: other }\nrun: x\ncwd: /tmp"),
            ("cli", "on: run.failed\nwhere: { cli: codex }\nrun: x\ncwd: /tmp"),
            (
                "cause",
                "on: run.failed\nwhere: { cause: 'decision:' }\nrun: x\ncwd: /tmp",
            ),
            ("group", "on: run.failed\nwhere: { group: other }\nrun: x\ncwd: /tmp"),
        ] {
            let r = reaction("n", yaml);
            assert_eq!(
                verdict(&r, &cx(&e, &run), Timestamp::now()),
                Verdict::Filtered(field.into()),
                "{yaml}"
            );
        }
    }

    #[test]
    fn a_rule_never_fires_on_its_own_doing() {
        let r = reaction("notify", "on: run.*\nrun: x\ncwd: /tmp");
        let e = event("run.started");
        let mut mine = run();
        mine.cause = Cause::Reaction("notify".into());
        assert_eq!(verdict(&r, &cx(&e, &mine), Timestamp::now()), Verdict::ItsOwnDoing);

        // Nor on a question it asked itself.
        let asked = Context_ {
            event: &e,
            run: None,
            by_reaction: Some("notify".into()),
        };
        assert_eq!(verdict(&r, &asked, Timestamp::now()), Verdict::ItsOwnDoing);

        // Another rule's doing is fair game.
        let other = reaction("watch", "on: run.*\nrun: x\ncwd: /tmp");
        assert!(verdict(&other, &cx(&e, &mine), Timestamp::now()).fires());
    }

    #[test]
    fn throttle_holds_a_burst_back() {
        let now = Timestamp::now();
        let (e, run) = (event("run.failed"), run());
        let mut r = reaction("n", "on: run.failed\nrun: x\ncwd: /tmp");
        r.last_fired_at = Some(now - SignedDuration::from_secs(2));
        assert_eq!(verdict(&r, &cx(&e, &run), now), Verdict::Throttled);
        r.last_fired_at = Some(now - SignedDuration::from_secs(30));
        assert!(verdict(&r, &cx(&e, &run), now).fires());

        // A rule can say how long, or ask for no throttle at all.
        let mut slow = reaction("n", "on: run.failed\nthrottle: 1h\nrun: x\ncwd: /tmp");
        slow.last_fired_at = Some(now - SignedDuration::from_secs(120));
        assert_eq!(verdict(&slow, &cx(&e, &run), now), Verdict::Throttled);
        let mut never = reaction("n", "on: run.failed\nthrottle: '0'\nrun: x\ncwd: /tmp");
        never.last_fired_at = Some(now);
        assert!(verdict(&never, &cx(&e, &run), now).fires());
    }

    #[test]
    fn approval_and_pausing_stop_a_rule() {
        let (e, run) = (event("run.failed"), run());
        let mut r = reaction("n", "on: run.failed\nrun: x\ncwd: /tmp");
        r.approved_hash = None;
        assert_eq!(verdict(&r, &cx(&e, &run), Timestamp::now()), Verdict::NotApproved);
        let mut r = reaction("n", "on: run.failed\nrun: x\ncwd: /tmp");
        r.paused = true;
        assert_eq!(verdict(&r, &cx(&e, &run), Timestamp::now()), Verdict::Paused);
    }

    #[test]
    fn the_event_fills_in_the_braces() {
        let (e, run) = (event("run.failed"), run());
        let v = vars(&e, Some(&run));
        assert_eq!(substitute("{kind}: {summary}", &v), "run.failed: exit 1 · 3s");
        assert_eq!(substitute("run {run} of {title}", &v), "run r1 of nightly tests");
        assert_eq!(substitute("exit {data.exit}", &v), "exit 1");
        assert_eq!(substitute("{cause} {cli} {group}", &v), "schedule:nightly claude main");
        // A name nothing knows, and a stray brace, are left alone.
        assert_eq!(substitute("${nope} and {", &v), "${nope} and {");
        assert_eq!(
            substitute("for i in 1 2; do echo $i; done", &v),
            "for i in 1 2; do echo $i; done"
        );
    }

    #[test]
    fn a_rule_fills_in_what_it_runs() {
        let (e, run) = (event("run.failed"), run());
        let v = vars(&e, Some(&run));
        let r = reaction("n", "on: run.failed\nrun: 'notify \"{title}: {summary}\"'\ncwd: /tmp");
        assert_eq!(
            resolved(&r.spec.action(), &v).label(),
            "notify \"nightly tests: exit 1 · 3s\""
        );
        let r = reaction(
            "n",
            "on: run.failed\nrun: { agent: claude, prompt: 'look at {run}' }\ncwd: /tmp",
        );
        assert_eq!(resolved(&r.spec.action(), &v).label(), "claude · look at r1");
    }

    #[test]
    fn a_question_gets_the_rules_name_as_its_group() {
        let (e, run) = (event("run.failed"), run());
        let v = vars(&e, Some(&run));
        let r = reaction(
            "ask-me",
            "on: run.failed\nask:\n  title: '{title} failed'\n  options: [{ key: look }]\ncwd: /tmp",
        );
        let ask = resolved_ask(r.spec.ask.as_ref().unwrap(), &r.name, &v);
        assert_eq!(ask.title, "nightly tests failed");
        assert_eq!(ask.group.as_deref(), Some("reaction:ask-me"));
    }

    #[test]
    fn validation_says_what_is_wrong() {
        let err = |y: &str| validate("x", &spec(y), false).unwrap_err().to_string();
        assert!(err("on: run.failed").contains("run"));
        assert!(err("on: run.failed\nrun: x\nask: { title: t, options: [{key: a}] }\ncwd: /tmp").contains("not both"));
        assert!(err("on: run.failed\nrun: x").contains("cwd"));
        assert!(err("on: run.failed\nrun: x\ncwd: /tmp\nthrottle: soon").contains("throttle"));
        assert!(err("on: run.failed\nask: { title: t, options: [] }\ncwd: /tmp").contains("option"));
        assert!(validate("ok", &spec("on: run.failed\nrun: x\ncwd: /tmp"), false).is_ok());
        assert!(
            validate("ws", &spec("on: run.failed\nrun: x"), true).is_ok(),
            "a workspace has a cwd"
        );
    }
}
