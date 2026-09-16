//! The scheduler: schedule files → rows in the ledger → runs, on time.
//!
//! Two kinds of file: the user's own `~/.relay/schedules.yaml` (the `global`
//! scope, trusted as it loads) and one `.relay/schedules.yaml` per registered
//! workspace (scope = the folder; a version fires only once approved). A
//! reload diffs each file against the `schedules` table (identity by name,
//! version by hash). The loop sleeps until the earliest `next_at`, fires
//! what is due as ordinary runs with `cause: schedule:<name>`, and applies
//! the missed and overlap policies.

pub mod cron;

use crate::action::expand_home;
use crate::rulestore::RuleStore;
use crate::supervisor::Supervisor;
use anyhow::{Context, Result, anyhow, bail};
use cron::Cron;
use jiff::Timestamp;
use rl_core::Home;
use rl_core::time::parse_duration;
use rl_detect::Registry;
use rl_ledger::entries::Loaded;
use rl_ledger::schedules;
use rl_ledger::{Ledger, events, events::NewEvent, runs, workspaces};
use rl_proto::methods::{RunFilter, RunSpec};
use rl_proto::schedule::{
    GLOBAL_SCOPE, Missed, Overlap, ReloadReport, Schedule, ScheduleSpec, UpReport, WORKSPACE_DIR, Workspace,
};
use rl_proto::types::{Cause, Run, event_kind};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// A firing this far past due was missed (asleep, or the daemon was down),
/// not merely late.
pub const MISSED_AFTER: Duration = Duration::from_secs(120);
/// How often the loop looks around when nothing is due sooner.
const TICK: Duration = Duration::from_secs(30);
/// …and while a queued firing waits for the previous run to end.
const BUSY_TICK: Duration = Duration::from_secs(1);

// ---- the file ----------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    schedules: BTreeMap<String, ScheduleSpec>,
}

/// Reads and validates a schedules file. A missing file is an empty one.
/// Entries that fail validation are reported, the rest load. `in_workspace`
/// lets `cwd` default to the folder.
pub fn load_file(path: &Path, in_workspace: bool) -> Result<(Vec<Loaded>, Vec<String>)> {
    if !path.is_file() {
        return Ok((Vec::new(), Vec::new()));
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: File = serde_norway::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let mut loaded = Vec::new();
    let mut errors = Vec::new();
    for (name, spec) in file.schedules {
        match validate(&name, &spec, in_workspace) {
            Ok(()) => loaded.push(Loaded {
                hash: hash(&spec),
                spec_json: serde_json::to_string(&spec)?,
                name,
            }),
            Err(e) => errors.push(format!("{name}: {e:#}")),
        }
    }
    Ok((loaded, errors))
}

fn validate(name: &str, spec: &ScheduleSpec, in_workspace: bool) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        bail!("names are letters, digits, `.`, `_` and `-`");
    }
    match (&spec.cron, &spec.every) {
        (Some(c), None) => {
            Cron::parse(c).context("cron")?;
        }
        (None, Some(e)) => {
            let d = parse_duration(e).ok_or_else(|| anyhow!("every: `{e}` is not a duration (30m, 4h)"))?;
            if d.is_zero() {
                bail!("every: must be more than 0");
            }
        }
        (Some(_), Some(_)) => bail!("`cron` or `every`, not both"),
        (None, None) => bail!("needs `cron` or `every`"),
    }
    if spec.run.is_none() {
        bail!("needs `run`");
    }
    if spec.cwd.is_none() && !in_workspace {
        bail!("needs `cwd`");
    }
    Ok(())
}

/// sha256 over a spec with keys sorted: the version of a schedule, a
/// reaction, or anything else a file writes down.
pub fn hash<T: serde::Serialize>(spec: &T) -> String {
    let v = serde_json::to_value(spec).expect("spec serialises");
    let mut h = Sha256::new();
    h.update(canonical(&v));
    format!("{:x}", h.finalize())
}

fn canonical(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", serde_json::to_string(k).unwrap(), canonical(&m[k])))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

/// When a schedule fires next, strictly after `after`. `every` counts from
/// the last firing; `cron` from the clock.
pub fn next_fire(spec: &ScheduleSpec, after: Timestamp, last_fired: Option<Timestamp>) -> Option<Timestamp> {
    if let Some(c) = &spec.cron {
        return Cron::parse(c).ok()?.next_after(after);
    }
    let every = parse_duration(spec.every.as_deref()?)?;
    match last_fired {
        Some(l) => l.checked_add(every).ok(),
        None => after.checked_add(every).ok(),
    }
}

/// The run a schedule starts. An agent runs headless: the profile's
/// `headless` arguments, then `args`, then the prompt. In a workspace,
/// `cwd` defaults to the folder and a relative one is inside it.
pub fn run_spec(
    name: &str,
    spec: &ScheduleSpec,
    source_dir: &Path,
    workspace: Option<&Path>,
    registry: &Registry,
) -> Result<RunSpec> {
    crate::action::run_spec(
        &spec.action(),
        name,
        source_dir,
        workspace,
        registry,
        Cause::Schedule(name.to_string()),
        None,
    )
}

/// Where a workspace keeps its schedules.
pub fn workspace_file(workspace: &Path) -> PathBuf {
    workspace.join(WORKSPACE_DIR).join("schedules.yaml")
}

// ---- the loop ----------------------------------------------------------------

pub struct Scheduler {
    home: Home,
    ledger: Ledger,
    supervisor: Arc<Supervisor>,
    registry: Arc<Registry>,
    wake: Notify,
    shutdown: CancellationToken,
}

impl Scheduler {
    pub fn new(
        home: Home,
        ledger: Ledger,
        supervisor: Arc<Supervisor>,
        registry: Arc<Registry>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            home,
            ledger,
            supervisor,
            registry,
            wake: Notify::new(),
            shutdown,
        }
    }

    /// Re-reads every file (the user's, then each workspace's), diffs them
    /// into the ledger, plans what is approved, and wakes the loop.
    pub async fn reload(&self) -> Result<ReloadReport> {
        RuleStore::reload(self).await
    }

    /// One file into one scope. Errors in entries are reported, not fatal;
    /// a file that does not parse is.
    async fn load_scope(&self, scope: &str, path: &Path, trusted: bool) -> Result<ReloadReport> {
        RuleStore::load_scope(self, scope, path, trusted).await
    }

    /// Gives every approved, unpaused schedule without a `next_at` one.
    async fn plan_all(&self) -> Result<()> {
        let now = Timestamp::now();
        for s in self.list(false).await? {
            if s.next_at.is_none() && !s.paused && s.approved() {
                self.plan(&s, now, s.last_fired_at).await?;
            }
        }
        Ok(())
    }

    pub async fn list(&self, include_removed: bool) -> Result<Vec<Schedule>> {
        self.ledger.call(move |c| schedules::list(c, include_removed)).await
    }

    /// A schedule by name; `scope` settles it when the name exists in more
    /// than one.
    pub async fn get(&self, name: &str, scope: Option<&str>) -> Result<Schedule> {
        RuleStore::get(self, name, scope).await
    }

    /// `global`, or a workspace path (any spelling of it) as registered.
    async fn scope_of(&self, scope: &str) -> Result<String> {
        RuleStore::scope_of(self, scope).await
    }

    /// Pausing keeps `next_at`; resuming plans afresh from now, so nothing
    /// missed while paused is made up.
    pub async fn set_paused(&self, name: &str, scope: Option<&str>, paused: bool) -> Result<Schedule> {
        let s = self.get(name, scope).await?;
        let (sc, n) = (s.scope.clone(), s.name.clone());
        self.ledger
            .call(move |c| schedules::set_paused(c, &sc, &n, paused))
            .await?;
        if !paused && s.approved() {
            self.plan(&s, Timestamp::now(), None).await?;
        }
        self.wake.notify_one();
        self.get(&s.name, Some(&s.scope)).await
    }

    /// Says yes to the current version of one schedule, and plans it.
    pub async fn approve(&self, name: &str, scope: Option<&str>) -> Result<Schedule> {
        let s = self.get(name, scope).await?;
        if s.removed_at.is_some() {
            bail!("`{name}` is no longer in {}", s.source.display());
        }
        let (sc, n) = (s.scope.clone(), vec![s.name.clone()]);
        self.ledger.call(move |c| schedules::approve(c, &sc, &n)).await?;
        self.plan_all().await?;
        self.wake.notify_one();
        self.get(&s.name, Some(&s.scope)).await
    }

    /// Fires now, whatever the clock or the policies say. Approval still
    /// counts: a version nobody said yes to does not run.
    pub async fn run_now(&self, name: &str, scope: Option<&str>) -> Result<Run> {
        let s = self.get(name, scope).await?;
        if s.removed_at.is_some() {
            bail!("`{name}` is no longer in {}", s.source.display());
        }
        if !s.approved() {
            bail!(
                "`{name}` in {} is not approved; `rl up {}` first",
                s.scope_label(),
                s.scope
            );
        }
        self.fire(&s, Timestamp::now()).await
    }

    // ---- workspaces ----------------------------------------------------------

    pub async fn workspaces(&self) -> Result<Vec<Workspace>> {
        self.ledger.call(|c| workspaces::list(c)).await
    }

    /// Renames a registered folder (any spelling of its path), keeping
    /// everything else. Fails when the folder was never `rl up`'d.
    pub async fn rename_workspace(&self, path: &Path, name: &str) -> Result<Workspace> {
        let scope = self.scope_of(&path.to_string_lossy()).await?;
        let path = PathBuf::from(scope);
        let name = name.to_string();
        self.ledger
            .call(move |c| workspaces::rename(c, &path, &name))
            .await?
            .ok_or_else(|| anyhow!("not a registered workspace; `rl up` there first"))
    }

    /// Registers a folder (idempotent), loads its `.relay/schedules.yaml`,
    /// and approves every pending version when asked.
    pub async fn up(&self, path: &Path, approve: bool) -> Result<UpReport> {
        let path = std::fs::canonicalize(path).with_context(|| format!("workspace {}", path.display()))?;
        if !path.is_dir() {
            bail!("{} is not a directory", path.display());
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let now = Timestamp::now();
        let workspace = self
            .ledger
            .call({
                let path = path.clone();
                move |c| workspaces::register(c, &path, &name, now)
            })
            .await?;
        let scope = path.to_string_lossy().into_owned();
        let report = self.load_scope(&scope, &workspace_file(&path), false).await?;
        let approved = if approve {
            let sc = scope.clone();
            self.ledger.call(move |c| schedules::approve(c, &sc, &[])).await?
        } else {
            Vec::new()
        };
        self.plan_all().await?;
        self.wake.notify_one();
        let sc = scope.clone();
        let schedules = self
            .ledger
            .call(move |c| schedules::list_scope(c, &sc))
            .await?
            .into_iter()
            .filter(|s| s.removed_at.is_none())
            .collect();
        info!(workspace = %path.display(), approved = approved.len(), "workspace up");
        Ok(UpReport {
            workspace,
            report,
            schedules,
            approved,
            reaction_report: ReloadReport::default(),
            reactions: Vec::new(),
            approved_reactions: Vec::new(),
        })
    }

    /// Unregisters a folder; its schedules stop and are marked removed.
    pub async fn remove_workspace(&self, path: &Path) -> Result<bool> {
        let scope = self.scope_of(&path.to_string_lossy()).await?;
        let now = Timestamp::now();
        let path = PathBuf::from(&scope);
        let gone = self
            .ledger
            .call(move |c| {
                schedules::remove_scope(c, &scope, now)?;
                workspaces::remove(c, &path)
            })
            .await?;
        self.wake.notify_one();
        Ok(gone)
    }

    // ---- the loop --------------------------------------------------------------

    /// Runs until shutdown. The caller has loaded the files.
    pub async fn serve(self: Arc<Self>) {
        loop {
            let nap = match self.tick().await {
                Ok(d) => d,
                Err(e) => {
                    warn!(error = %format!("{e:#}"), "scheduler tick");
                    TICK
                }
            };
            tokio::select! {
                _ = self.shutdown.cancelled() => break,
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(nap) => {}
            }
        }
    }

    /// Fires what is due; says how long until the next look.
    async fn tick(&self) -> Result<Duration> {
        let now = Timestamp::now();
        let mut busy = false;
        for s in self.list(false).await? {
            if s.paused || !s.approved() {
                continue;
            }
            match s.next_at {
                None => self.plan(&s, now, s.last_fired_at).await?,
                Some(due) if due <= now => busy |= self.due(&s, now, due).await?,
                Some(_) => {}
            }
        }
        // Firing and planning moved the clocks; read them again for the nap.
        let now = Timestamp::now();
        let mut nap = if busy { BUSY_TICK } else { TICK };
        for s in self.list(false).await? {
            if let (false, true, Some(next)) = (s.paused, s.approved(), s.next_at) {
                let wait = next.duration_since(now).unsigned_abs();
                nap = nap.min(wait);
            }
        }
        Ok(nap)
    }

    /// A schedule whose time has come. `true` when it is waiting on the
    /// previous run and wants another look soon.
    async fn due(&self, s: &Schedule, now: Timestamp, due: Timestamp) -> Result<bool> {
        let late = now.duration_since(due).unsigned_abs() > MISSED_AFTER;
        if late && s.spec.missed == Missed::Skip {
            self.skipped(s, now, "missed", Some(due)).await?;
            self.plan(s, now, None).await?;
            return Ok(false);
        }
        if let Some(live) = self.live_run_of(&s.name).await? {
            match s.spec.overlap {
                Overlap::Skip => {
                    self.skipped(s, now, "overlap", Some(due)).await?;
                    self.plan(s, now, None).await?;
                    return Ok(false);
                }
                Overlap::Queue => return Ok(true),
                Overlap::Kill => {
                    debug!(schedule = %s.name, run = %live, "overlap: stopping the previous run");
                    if let Err(e) = self.supervisor.stop(&live, false).await {
                        warn!(schedule = %s.name, run = %live, error = %format!("{e:#}"), "overlap: stop failed");
                    }
                    return Ok(true);
                }
            }
        }
        if let Err(e) = self.fire(s, now).await {
            warn!(schedule = %s.name, error = %format!("{e:#}"), "schedule failed to start");
            self.skipped(s, now, &format!("error: {e:#}"), Some(due)).await?;
            self.plan(s, now, None).await?;
        }
        Ok(false)
    }

    /// A live run this schedule started, if any.
    async fn live_run_of(&self, name: &str) -> Result<Option<String>> {
        let filter = RunFilter {
            active_only: true,
            cause: Some(Cause::Schedule(name.to_string()).to_string()),
            ..Default::default()
        };
        let runs = self.ledger.call(move |c| runs::list(c, &filter)).await?;
        Ok(runs
            .into_iter()
            .map(|r| r.id)
            .find(|id| self.supervisor.live(id).is_some()))
    }

    async fn fire(&self, s: &Schedule, now: Timestamp) -> Result<Run> {
        let dir = s.source.parent().map(Path::to_path_buf).unwrap_or_default();
        let workspace = (s.scope != GLOBAL_SCOPE).then(|| PathBuf::from(&s.scope));
        let mut spec = run_spec(&s.name, &s.spec, &dir, workspace.as_deref(), &self.registry)?;
        // A probe can ask "what is new since last time" without keeping its own
        // clock: the previous firing, empty the first time round.
        if let Some(last) = s.last_fired_at {
            spec.env.push(("RELAY_LAST_RUN".into(), last.to_string()));
        }
        let run = self.supervisor.start(spec).await?;
        info!(schedule = %s.name, scope = %s.scope_label(), run = %run.id, "schedule fired");
        // The run is alive from here on: if recording that it fired fails,
        // that is a bookkeeping problem, not grounds to tell the caller this
        // firing never happened. Reporting it as a failure here would have
        // `due()` mark the schedule "skipped" for a run that is in fact
        // running, and replan with `last_fired: None`, which also breaks
        // `every`'s cadence.
        let (scope, name, hash, id) = (s.scope.clone(), s.name.clone(), s.hash.clone(), run.id.clone());
        let run_for_event = run.clone();
        if let Err(e) = self
            .ledger
            .call(move |c| {
                schedules::set_fired(c, &scope, &name, now, &id)?;
                events::append(
                    c,
                    &NewEvent::for_run(
                        &run_for_event,
                        now,
                        event_kind::SCHEDULE_FIRED,
                        format!("schedule `{name}` fired"),
                    )
                    .with_data(json!({ "schedule": name, "scope": scope, "hash": hash, "run": id })),
                )
            })
            .await
        {
            warn!(schedule = %s.name, run = %run.id, error = %format!("{e:#}"), "schedule fired but recording it failed");
        }
        self.plan(s, now, Some(now)).await?;
        Ok(run)
    }

    async fn skipped(&self, s: &Schedule, now: Timestamp, reason: &str, due: Option<Timestamp>) -> Result<()> {
        info!(schedule = %s.name, reason, "schedule skipped");
        let cwd = match (s.spec.cwd.as_deref().map(expand_home), s.scope.as_str()) {
            (Some(c), _) if c.is_absolute() => c,
            (Some(c), scope) if scope != GLOBAL_SCOPE => Path::new(scope).join(c),
            (_, scope) if scope != GLOBAL_SCOPE => PathBuf::from(scope),
            _ => PathBuf::new(),
        };
        let ev = NewEvent {
            at: now,
            workspace: cwd.clone(),
            run: None,
            group_key: s
                .spec
                .ref_
                .clone()
                .unwrap_or_else(|| cwd.to_string_lossy().into_owned()),
            kind: event_kind::SCHEDULE_SKIPPED.into(),
            summary: format!("schedule `{}` skipped: {reason}", s.name),
            data: Some(json!({ "schedule": s.name, "scope": s.scope, "hash": s.hash, "reason": reason, "due": due })),
        };
        self.ledger.call(move |c| events::append(c, &ev).map(|_| ())).await
    }

    /// Writes the next firing time after `after`.
    async fn plan(&self, s: &Schedule, after: Timestamp, last_fired: Option<Timestamp>) -> Result<()> {
        let next = next_fire(&s.spec, after, last_fired);
        let (scope, name) = (s.scope.clone(), s.name.clone());
        self.ledger
            .call(move |c| schedules::set_next(c, &scope, &name, next))
            .await
    }
}

impl RuleStore for Scheduler {
    type Item = Schedule;

    fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    fn kind() -> &'static str {
        "schedule"
    }

    fn home_file(&self) -> PathBuf {
        self.home.schedules_file()
    }

    fn workspace_file(workspace: &Path) -> PathBuf {
        workspace_file(workspace)
    }

    fn load_file(path: &Path, in_workspace: bool) -> Result<(Vec<Loaded>, Vec<String>)> {
        load_file(path, in_workspace)
    }

    fn sync(
        conn: &Connection,
        scope: &str,
        source: &Path,
        loaded: &[Loaded],
        trusted: bool,
        now: Timestamp,
    ) -> Result<ReloadReport> {
        schedules::sync(conn, scope, source, loaded, trusted, now)
    }

    fn get_by_scope(conn: &Connection, scope: &str, name: &str) -> Result<Option<Schedule>> {
        schedules::get(conn, scope, name)
    }

    fn find_by_name(conn: &Connection, name: &str) -> Result<Vec<Schedule>> {
        schedules::find(conn, name)
    }

    fn scope_label(item: &Schedule) -> String {
        item.scope_label()
    }

    /// Reloading also plans every newly-approved schedule.
    async fn plan_after_reload(&self) -> Result<()> {
        self.plan_all().await
    }

    fn log_and_wake(&self, report: &ReloadReport) {
        info!(
            loaded = report.loaded,
            added = report.added.len(),
            changed = report.changed.len(),
            removed = report.removed.len(),
            errors = report.errors.len(),
            "schedules reloaded"
        );
        self.wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::types::RunKind;

    fn spec(yaml: &str) -> ScheduleSpec {
        serde_norway::from_str(yaml).unwrap()
    }

    #[test]
    fn hash_ignores_key_order_and_sees_every_field() {
        let a = spec("every: 1h\nrun: [true]\ncwd: /tmp\ntags: [x]");
        let b = spec("tags: [x]\ncwd: /tmp\nrun: [true]\nevery: 1h");
        assert_eq!(hash(&a), hash(&b));
        let c = spec("every: 2h\nrun: [true]\ncwd: /tmp\ntags: [x]");
        assert_ne!(hash(&a), hash(&c));
    }

    #[test]
    fn validation_says_what_is_wrong() {
        let err = |y: &str| validate("x", &spec(y), false).unwrap_err().to_string();
        assert!(err("run: [true]\ncwd: /tmp").contains("cron"));
        assert!(err("every: 1h\ncwd: /tmp").contains("run"));
        assert!(err("every: 1h\nrun: [true]").contains("cwd"));
        assert!(err("every: 0s\nrun: [true]\ncwd: /tmp").contains("every"));
        assert!(err("cron: '* * *'\nrun: [true]\ncwd: /tmp").contains("cron"));
        assert!(validate("bad name", &spec("every: 1h\nrun: [true]\ncwd: /tmp"), false).is_err());
        assert!(validate("ok-1", &spec("every: 1h\nrun: [true]\ncwd: /tmp"), false).is_ok());
        assert!(
            validate("ws", &spec("every: 1h\nrun: [true]"), true).is_ok(),
            "a workspace has a cwd"
        );
    }

    /// What `run` takes is the parser's business, and it says so itself.
    #[test]
    fn a_bad_run_is_refused_where_it_is_read() {
        let err = |y: &str| serde_norway::from_str::<ScheduleSpec>(y).unwrap_err().to_string();
        assert!(err("every: 1h\nagent: claude\ncwd: /tmp").contains("unknown field"));
        assert!(err("every: 1h\nrun:\n  agent: claude\ncwd: /tmp").contains("needs `prompt`"));
        assert!(err("every: 1h\nrun:\n  shell: ls\ncwd: /tmp").contains("no `shell`"));
    }

    #[test]
    fn every_counts_from_the_last_firing() {
        let s = spec("every: 30m\nrun: [true]\ncwd: /tmp");
        let now: Timestamp = "2026-09-09T10:00:00Z".parse().unwrap();
        assert_eq!(next_fire(&s, now, None), Some("2026-09-09T10:30:00Z".parse().unwrap()));
        let last: Timestamp = "2026-09-09T09:50:00Z".parse().unwrap();
        assert_eq!(
            next_fire(&s, now, Some(last)),
            Some("2026-09-09T10:20:00Z".parse().unwrap())
        );
    }

    #[test]
    fn agents_run_headless_with_the_profile_arguments() {
        let reg = Registry::builtin();
        let s = spec(concat!(
            "every: 1h\n",
            "run:\n",
            "  agent: claude\n",
            "  prompt: look at the logs\n",
            "  args: [--model, haiku]\n",
            "cwd: ~/ops\n",
            "tags: [ops]\n",
        ));
        let r = run_spec("logs", &s, Path::new("/x"), None, &reg).unwrap();
        assert_eq!(r.argv, ["claude", "-p", "--model", "haiku", "look at the logs"]);
        assert_eq!(r.kind, RunKind::Agent);
        assert_eq!(r.title.as_deref(), Some("logs"));
        assert_eq!(r.cause, Cause::Schedule("logs".into()));
        assert!(r.cwd.ends_with("ops") && !r.cwd.starts_with("~"));

        // A list is argv; a string is a shell line.
        let c = spec("every: 1h\nrun: [cargo, test]\ncwd: /tmp");
        let r = run_spec("t", &c, Path::new("/x"), None, &reg).unwrap();
        assert_eq!(r.argv, ["cargo", "test"]);
        assert_eq!(r.kind, RunKind::Command);
        let c = spec("every: 1h\nrun: cargo test | tee out.txt\ncwd: /tmp");
        let r = run_spec("t", &c, Path::new("/x"), None, &reg).unwrap();
        assert_eq!(r.argv, ["sh", "-c", "cargo test | tee out.txt"]);
        assert_eq!(r.kind, RunKind::Command);
    }

    #[test]
    fn a_workspace_schedule_runs_in_its_folder() {
        let reg = Registry::builtin();
        let w = Path::new("/w/repo");
        let s = spec("every: 1h\nrun: [make]");
        assert_eq!(run_spec("m", &s, w, Some(w), &reg).unwrap().cwd, w);
        let s = spec("every: 1h\nrun: [make]\ncwd: sub/dir");
        assert_eq!(run_spec("m", &s, w, Some(w), &reg).unwrap().cwd, w.join("sub/dir"));
        let s = spec("every: 1h\nrun: [make]\ncwd: /elsewhere");
        assert_eq!(
            run_spec("m", &s, w, Some(w), &reg).unwrap().cwd,
            Path::new("/elsewhere")
        );
    }
}
