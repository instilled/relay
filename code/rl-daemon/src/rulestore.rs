//! What `Scheduler` and `Reactor` share: both keep a table of rules in the
//! ledger addressed by scope (`global`, or a registered workspace's path)
//! and name, loaded from a user file and a per-workspace file the same way.
//! Only the item type (`Schedule` vs `Reaction`) and the ledger calls that
//! read and write it differ; this trait carries the rest — scope
//! resolution, name lookup, and the per-workspace reload loop.

use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use rl_ledger::entries::Loaded;
use rl_ledger::{Ledger, workspaces};
use rl_proto::schedule::{GLOBAL_SCOPE, ReloadReport};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub(crate) trait RuleStore {
    /// `Schedule` or `Reaction`.
    type Item: Send + 'static;

    fn ledger(&self) -> &Ledger;
    /// "schedule" or "reaction", for error messages.
    fn kind() -> &'static str;
    /// Where the user's own file lives.
    fn home_file(&self) -> PathBuf;
    /// Where a workspace keeps its file.
    fn workspace_file(workspace: &Path) -> PathBuf;

    fn load_file(path: &Path, in_workspace: bool) -> Result<(Vec<Loaded>, Vec<String>)>;
    fn sync(
        conn: &Connection,
        scope: &str,
        source: &Path,
        loaded: &[Loaded],
        trusted: bool,
        now: Timestamp,
    ) -> Result<ReloadReport>;
    fn get_by_scope(conn: &Connection, scope: &str, name: &str) -> Result<Option<Self::Item>>;
    fn find_by_name(conn: &Connection, name: &str) -> Result<Vec<Self::Item>>;
    fn scope_label(item: &Self::Item) -> String;

    /// Logs the reload, in each one's own words, and wakes its loop.
    fn log_and_wake(&self, report: &ReloadReport);

    /// Extra work a reload needs beyond loading, run before it is logged.
    /// The scheduler plans newly-approved schedules here; a reactor needs
    /// nothing more.
    async fn plan_after_reload(&self) -> Result<()> {
        Ok(())
    }

    /// `global`, or a workspace path (any spelling of it) as registered.
    async fn scope_of(&self, scope: &str) -> Result<String> {
        if scope == GLOBAL_SCOPE {
            return Ok(scope.to_string());
        }
        let path = std::fs::canonicalize(scope).with_context(|| format!("workspace {scope}"))?;
        let w = self
            .ledger()
            .call(move |c| workspaces::get(c, &path))
            .await?
            .ok_or_else(|| anyhow!("{scope} is not a registered workspace; `rl up` there first"))?;
        Ok(w.path.to_string_lossy().into_owned())
    }

    /// An item by name; `scope` settles it when the name exists in several.
    async fn get(&self, name: &str, scope: Option<&str>) -> Result<Self::Item> {
        let n = name.to_string();
        if let Some(scope) = scope {
            let scope = self.scope_of(scope).await?;
            let s = scope.clone();
            return self
                .ledger()
                .call(move |c| Self::get_by_scope(c, &s, &n))
                .await?
                .ok_or_else(|| anyhow!("no {} named `{name}` in {scope}", Self::kind()));
        }
        let found = self.ledger().call(move |c| Self::find_by_name(c, &n)).await?;
        match found.len() {
            0 => bail!("no {} named `{name}`", Self::kind()),
            1 => Ok(found.into_iter().next().unwrap()),
            _ => {
                let scopes: Vec<String> = found.iter().map(Self::scope_label).collect();
                Err(crate::AmbiguousName(format!(
                    "`{name}` exists in {}; say which with --in",
                    scopes.join(" and ")
                ))
                .into())
            }
        }
    }

    /// One file into one scope. Errors in entries are reported, not fatal;
    /// a file that does not parse is.
    async fn load_scope(&self, scope: &str, path: &Path, trusted: bool) -> Result<ReloadReport> {
        let (loaded, errors) = Self::load_file(path, !trusted)?;
        let now = Timestamp::now();
        let (scope, path) = (scope.to_string(), path.to_path_buf());
        let mut report = self
            .ledger()
            .call(move |c| Self::sync(c, &scope, &path, &loaded, trusted, now))
            .await?;
        report.errors = errors;
        Ok(report)
    }

    /// Re-reads every file (the user's, then each workspace's) into the
    /// ledger.
    async fn reload(&self) -> Result<ReloadReport> {
        let mut report = self.load_scope(GLOBAL_SCOPE, &self.home_file(), true).await?;
        let workspaces = self.ledger().call(|c| workspaces::list(c)).await?;
        for w in workspaces {
            let scope = w.path.to_string_lossy().into_owned();
            match self.load_scope(&scope, &Self::workspace_file(&w.path), false).await {
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
        self.plan_after_reload().await?;
        self.log_and_wake(&report);
        Ok(report)
    }
}
