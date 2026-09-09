//! The scheduler: `schedules.yaml` → rows in the ledger → runs, on time.
//!
//! The file is the interface; a reload diffs it against the `schedules`
//! table (identity by name, version by hash). The loop sleeps until the
//! earliest `next_at`, fires what is due as ordinary runs with
//! `cause: schedule:<name>`, and applies the missed and overlap policies.

pub mod cron;

use crate::supervisor::Supervisor;
use anyhow::{Context, Result, anyhow, bail};
use cron::Cron;
use jiff::Timestamp;
use relay_core::Home;
use relay_core::time::parse_duration;
use relay_detect::Registry;
use relay_ledger::schedules::{self, Loaded};
use relay_ledger::{Ledger, events, events::NewEvent, runs};
use relay_proto::methods::{RunFilter, RunSpec};
use relay_proto::schedule::{GLOBAL_SCOPE, Missed, Overlap, ReloadReport, Schedule, ScheduleSpec};
use relay_proto::types::{Cause, Run, RunKind, event_kind};
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
/// Entries that fail validation are reported, the rest load.
pub fn load_file(path: &Path) -> Result<(Vec<Loaded>, Vec<String>)> {
    if !path.is_file() {
        return Ok((Vec::new(), Vec::new()));
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: File = serde_norway::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let mut loaded = Vec::new();
    let mut errors = Vec::new();
    for (name, spec) in file.schedules {
        match validate(&name, &spec) {
            Ok(()) => loaded.push(Loaded {
                hash: hash(&spec),
                name,
                spec,
            }),
            Err(e) => errors.push(format!("{name}: {e:#}")),
        }
    }
    Ok((loaded, errors))
}

fn validate(name: &str, spec: &ScheduleSpec) -> Result<()> {
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
    match (spec.run.is_empty(), &spec.agent) {
        (false, None) => {
            if spec.prompt.is_some() || spec.prompt_file.is_some() {
                bail!("`prompt` needs `agent`");
            }
        }
        (true, Some(_)) => {
            if spec.prompt.is_some() == spec.prompt_file.is_some() {
                bail!("an agent needs `prompt` or `prompt_file`, not both");
            }
        }
        (false, Some(_)) => bail!("`run` or `agent`, not both"),
        (true, None) => bail!("needs `run` or `agent`"),
    }
    if spec.cwd.is_none() {
        bail!("needs `cwd`");
    }
    Ok(())
}

/// sha256 over the spec with keys sorted: the version of a schedule.
pub fn hash(spec: &ScheduleSpec) -> String {
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

fn expand_home(p: &Path) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) => match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(rest),
            None => p.to_path_buf(),
        },
        Err(_) => p.to_path_buf(),
    }
}

/// The run a schedule starts. An agent runs headless: the profile's
/// `headless` arguments, then `args`, then the prompt.
pub fn run_spec(name: &str, spec: &ScheduleSpec, source_dir: &Path, registry: &Registry) -> Result<RunSpec> {
    let (kind, argv) = match &spec.agent {
        Some(cli) => {
            let prompt = match (&spec.prompt, &spec.prompt_file) {
                (Some(p), _) => p.clone(),
                (None, Some(f)) => {
                    let path = if f.is_absolute() { f.clone() } else { source_dir.join(f) };
                    std::fs::read_to_string(&path).with_context(|| format!("prompt_file {}", path.display()))?
                }
                (None, None) => bail!("an agent needs a prompt"),
            };
            let profile = registry.resolve(RunKind::Agent, std::slice::from_ref(cli)).profile;
            let mut argv = vec![cli.clone()];
            argv.extend(profile.headless.iter().cloned());
            argv.extend(spec.args.iter().cloned());
            argv.push(prompt);
            (RunKind::Agent, argv)
        }
        None => (RunKind::Command, spec.run.clone()),
    };
    let cwd = expand_home(spec.cwd.as_deref().ok_or_else(|| anyhow!("needs cwd"))?);
    Ok(RunSpec {
        argv,
        kind,
        cwd,
        title: Some(spec.title.clone().unwrap_or_else(|| name.to_string())),
        ref_: spec.ref_.clone(),
        tags: spec.tags.clone(),
        tty: !spec.no_tty,
        cols: None,
        rows: None,
        env: spec.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        inherit_env: true,
        cause: Cause::Schedule(name.to_string()),
        flow: None,
    })
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

    /// Re-reads the file, diffs it into the ledger, gives every schedule
    /// without a `next_at` one, and wakes the loop.
    pub async fn reload(&self) -> Result<ReloadReport> {
        let path = self.home.schedules_file();
        let (loaded, errors) = load_file(&path)?;
        let now = Timestamp::now();
        let mut report = self
            .ledger
            .call({
                let path = path.clone();
                move |c| schedules::sync(c, GLOBAL_SCOPE, &path, &loaded, now)
            })
            .await?;
        report.errors = errors;
        for s in self.list(false).await? {
            if s.next_at.is_none() && !s.paused {
                self.plan(&s, now, s.last_fired_at).await?;
            }
        }
        info!(
            loaded = report.loaded,
            added = report.added.len(),
            changed = report.changed.len(),
            removed = report.removed.len(),
            errors = report.errors.len(),
            "schedules reloaded"
        );
        self.wake.notify_one();
        Ok(report)
    }

    pub async fn list(&self, include_removed: bool) -> Result<Vec<Schedule>> {
        self.ledger.call(move |c| schedules::list(c, include_removed)).await
    }

    pub async fn get(&self, name: &str) -> Result<Schedule> {
        let name = name.to_string();
        self.ledger
            .call({
                let name = name.clone();
                move |c| schedules::get(c, GLOBAL_SCOPE, &name)
            })
            .await?
            .ok_or_else(|| anyhow!("no schedule named `{name}`"))
    }

    /// Pausing keeps `next_at`; resuming plans afresh from now, so nothing
    /// missed while paused is made up.
    pub async fn set_paused(&self, name: &str, paused: bool) -> Result<Schedule> {
        let s = self.get(name).await?;
        let n = name.to_string();
        self.ledger
            .call(move |c| schedules::set_paused(c, GLOBAL_SCOPE, &n, paused))
            .await?;
        if !paused {
            self.plan(&s, Timestamp::now(), None).await?;
        }
        self.wake.notify_one();
        self.get(name).await
    }

    /// Fires now, whatever the clock or the policies say.
    pub async fn run_now(&self, name: &str) -> Result<Run> {
        let s = self.get(name).await?;
        if s.removed_at.is_some() {
            bail!("`{name}` is no longer in {}", s.source.display());
        }
        self.fire(&s, Timestamp::now()).await
    }

    /// Runs until shutdown. The caller has loaded the file.
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
            if s.paused {
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
            if let (false, Some(next)) = (s.paused, s.next_at) {
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
        let spec = run_spec(&s.name, &s.spec, &dir, &self.registry)?;
        let run = self.supervisor.start(spec).await?;
        info!(schedule = %s.name, run = %run.id, "schedule fired");
        let (scope, name, hash, id) = (s.scope.clone(), s.name.clone(), s.hash.clone(), run.id.clone());
        let run_for_event = run.clone();
        self.ledger
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
                    .with_data(json!({ "schedule": name, "hash": hash, "run": id })),
                )
            })
            .await?;
        self.plan(s, now, Some(now)).await?;
        Ok(run)
    }

    async fn skipped(&self, s: &Schedule, now: Timestamp, reason: &str, due: Option<Timestamp>) -> Result<()> {
        info!(schedule = %s.name, reason, "schedule skipped");
        let cwd = s.spec.cwd.as_deref().map(expand_home).unwrap_or_default();
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
            data: Some(json!({ "schedule": s.name, "hash": s.hash, "reason": reason, "due": due })),
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let err = |y: &str| validate("x", &spec(y)).unwrap_err().to_string();
        assert!(err("run: [true]\ncwd: /tmp").contains("cron"));
        assert!(err("every: 1h\ncwd: /tmp").contains("run"));
        assert!(err("every: 1h\nrun: [true]").contains("cwd"));
        assert!(err("every: 0s\nrun: [true]\ncwd: /tmp").contains("every"));
        assert!(err("cron: '* * *'\nrun: [true]\ncwd: /tmp").contains("cron"));
        assert!(err("every: 1h\nagent: claude\ncwd: /tmp").contains("prompt"));
        assert!(err("every: 1h\nrun: [x]\nagent: claude\nprompt: p\ncwd: /tmp").contains("not both"));
        assert!(validate("bad name", &spec("every: 1h\nrun: [true]\ncwd: /tmp")).is_err());
        assert!(validate("ok-1", &spec("every: 1h\nrun: [true]\ncwd: /tmp")).is_ok());
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
        let s = spec(
            "every: 1h\nagent: claude\nprompt: 'look at the logs'\nargs: [--model, haiku]\ncwd: ~/ops\ntags: [ops]",
        );
        let r = run_spec("logs", &s, Path::new("/x"), &reg).unwrap();
        assert_eq!(r.argv, ["claude", "-p", "--model", "haiku", "look at the logs"]);
        assert_eq!(r.kind, RunKind::Agent);
        assert_eq!(r.title.as_deref(), Some("logs"));
        assert_eq!(r.cause, Cause::Schedule("logs".into()));
        assert!(r.cwd.ends_with("ops") && !r.cwd.starts_with("~"));
        let c = spec("every: 1h\nrun: [cargo, test]\ncwd: /tmp");
        let r = run_spec("t", &c, Path::new("/x"), &reg).unwrap();
        assert_eq!(r.argv, ["cargo", "test"]);
        assert_eq!(r.kind, RunKind::Command);
    }
}
