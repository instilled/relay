//! What `Scheduler` and `Reactor` share: both keep a table of rules in the
//! ledger addressed by scope (`global`, or a registered workspace's path)
//! and name, loaded from a user file and a per-workspace file the same way.
//! Only the item type (`Schedule` vs `Reaction`) and the ledger calls that
//! read and write it differ; this trait carries the rest — scope
//! resolution, name lookup, and the per-workspace reload loop.

use anyhow::{Result, anyhow, bail};
use jiff::Timestamp;
use rl_ledger::entries::Loaded;
use rl_ledger::{Ledger, workspaces};
use rl_proto::rule::NotLoaded;
use rl_proto::schedule::{GLOBAL_SCOPE, ReloadReport};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A folder as the ledger keys it: its canonical path, or — once it is gone
/// from disk — its canonical parent with its own name, or else just as given.
/// Registered paths are canonical, so a folder deleted since can still be
/// named the way it was named then.
pub(crate) fn resolve(path: &Path) -> PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return p;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(p) = std::fs::canonicalize(parent)
    {
        return p.join(name);
    }
    path.to_path_buf()
}

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

    /// The rules a file holds, and `(name, what was wrong)` for those that
    /// do not check out. A file that cannot be read or parsed is an error.
    fn load_file(path: &Path, in_workspace: bool) -> Result<FileRead>;
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

    /// `global`, or a workspace path (any spelling of it) as registered. A
    /// folder that is gone from disk is still found by the path it was
    /// registered under.
    async fn scope_of(&self, scope: &str) -> Result<String> {
        if scope == GLOBAL_SCOPE {
            return Ok(scope.to_string());
        }
        let path = resolve(Path::new(scope));
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

    /// What did not load at each scope's last load, by scope. Kept until the
    /// scope loads clean, so a broken file stays said, not just flashed.
    fn broken(&self) -> &Broken;

    /// One file into one scope, `name` being what the scope is called. A
    /// rule that does not check out is left out and reported; a file that
    /// cannot be read or parsed is reported and changes nothing, so its rules
    /// stay as they last loaded. Only the ledger failing is an error.
    async fn load_scope(&self, scope: &str, name: &str, path: &Path, trusted: bool) -> Result<ReloadReport> {
        let not_loaded = |rule: Option<String>, line, column, message| NotLoaded {
            scope: scope.to_string(),
            name: name.to_string(),
            file: path.to_path_buf(),
            rule,
            line,
            column,
            message,
        };
        let mut report = match Self::load_file(path, !trusted) {
            Ok((loaded, errors)) => {
                let now = Timestamp::now();
                let (sc, p) = (scope.to_string(), path.to_path_buf());
                let mut report = self
                    .ledger()
                    .call(move |c| Self::sync(c, &sc, &p, &loaded, trusted, now))
                    .await?;
                let text = if errors.is_empty() {
                    String::new()
                } else {
                    std::fs::read_to_string(path).unwrap_or_default()
                };
                let top = format!("{}s", Self::kind());
                report.not_loaded = errors
                    .into_iter()
                    .map(|(rule, why)| {
                        let line = entry_line(&text, &top, &rule);
                        not_loaded(Some(rule), line, None, why)
                    })
                    .collect();
                report
            }
            Err(e) => {
                let (line, column, message) = file_error(&e);
                ReloadReport {
                    not_loaded: vec![not_loaded(None, line, column, message)],
                    ..Default::default()
                }
            }
        };
        report.errors = report.not_loaded.iter().map(ToString::to_string).collect();
        let mut broken = self.broken().lock().unwrap();
        if report.not_loaded.is_empty() {
            broken.remove(scope);
        } else {
            broken.insert(scope.to_string(), report.not_loaded.clone());
        }
        Ok(report)
    }

    /// Everything that did not load at the last load of each scope.
    fn not_loaded(&self) -> Vec<NotLoaded> {
        self.broken().lock().unwrap().values().flatten().cloned().collect()
    }

    /// A scope that is gone: nothing of it is broken any more.
    fn forget(&self, scope: &str) {
        self.broken().lock().unwrap().remove(scope);
    }

    /// Re-reads every file (the user's, then each workspace's) into the
    /// ledger. One that does not parse, the user's too, is reported and the
    /// rest load all the same.
    async fn reload(&self) -> Result<ReloadReport> {
        let mut report = self
            .load_scope(GLOBAL_SCOPE, GLOBAL_SCOPE, &self.home_file(), true)
            .await?;
        let workspaces = self.ledger().call(|c| workspaces::list(c)).await?;
        let mut scopes = vec![GLOBAL_SCOPE.to_string()];
        for w in workspaces {
            let scope = w.path.to_string_lossy().into_owned();
            let file = Self::workspace_file(&w.path);
            scopes.push(scope.clone());
            match self.load_scope(&scope, &w.name, &file, false).await {
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
                    report.errors.extend(r.errors);
                    report.not_loaded.extend(r.not_loaded);
                }
                Err(e) => {
                    let n = NotLoaded {
                        scope,
                        name: w.name.clone(),
                        file,
                        rule: None,
                        line: None,
                        column: None,
                        message: format!("{e:#}"),
                    };
                    report.errors.push(n.to_string());
                    report.not_loaded.push(n);
                }
            }
        }
        // A folder no longer registered has nothing broken to say.
        self.broken().lock().unwrap().retain(|s, _| scopes.contains(s));
        self.plan_after_reload().await?;
        self.log_and_wake(&report);
        Ok(report)
    }
}

/// What did not load, by scope; see [`RuleStore::broken`].
pub(crate) type Broken = Mutex<BTreeMap<String, Vec<NotLoaded>>>;

/// What a rules file holds: the rules that check out, and `(name, what was
/// wrong)` for those that do not.
pub type FileRead = (Vec<Loaded>, Vec<(String, String)>);

/// A file that did not load, as line, column and why. A YAML error says
/// where itself; that is taken out of the words so it is not said twice. The
/// file's path is the report's to say, so it is not in the words either.
fn file_error(e: &anyhow::Error) -> (Option<usize>, Option<usize>, String) {
    if let Some(y) = e.chain().find_map(|c| c.downcast_ref::<serde_norway::Error>()) {
        let text = y.to_string();
        return match y.location() {
            Some(l) => {
                let mark = format!(" at line {} column {}", l.line(), l.column());
                (Some(l.line()), Some(l.column()), text.replacen(&mark, "", 1))
            }
            None => (None, None, text),
        };
    }
    let io = e.chain().find_map(|c| c.downcast_ref::<std::io::Error>());
    (None, None, io.map_or_else(|| format!("{e:#}"), ToString::to_string))
}

/// The line, from 1, where `name:` opens its block in the `top:` mapping of
/// a rules file: the first key at the mapping's own indent that is `name`.
fn entry_line(text: &str, top: &str, name: &str) -> Option<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| {
        l.strip_prefix(top)
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
    })?;
    let mut indent = None;
    for (i, l) in lines.iter().enumerate().skip(start + 1) {
        let body = l.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let at = l.len() - body.len();
        if at == 0 {
            break;
        }
        let want = *indent.get_or_insert(at);
        if at != want {
            continue;
        }
        let key = body.split(':').next().unwrap_or("").trim().trim_matches(['"', '\'']);
        if key == name {
            return Some(i + 1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rule_is_found_at_its_own_key() {
        let text = "# mine\nschedules:\n  # the first\n  run:\n    every: 1h\n    run: [true]\n  'bad':\n    every: 1h\nother:\n  bad: 1\n";
        assert_eq!(
            entry_line(text, "schedules", "run"),
            Some(4),
            "a rule named like a field"
        );
        assert_eq!(entry_line(text, "schedules", "bad"), Some(7));
        assert_eq!(entry_line(text, "schedules", "gone"), None);
        assert_eq!(entry_line(text, "reactions", "bad"), None);
    }

    #[test]
    fn a_yaml_error_says_where_once() {
        let e = serde_norway::from_str::<serde_norway::Value>("a:\n  b: 1\n c: [\n").unwrap_err();
        let (line, column, why) = file_error(&anyhow::Error::from(e).context("parsing /x.yaml"));
        assert_eq!((line, column), (Some(3), Some(2)));
        assert!(!why.contains("/x.yaml") && !why.starts_with("parsing"), "{why}");
        assert!(!why.contains("at line 3 column 2"), "{why}");
        let io = std::fs::read_to_string("/nope/not/here").unwrap_err();
        let (line, _, why) = file_error(&anyhow::Error::from(io).context("reading /nope/not/here"));
        assert_eq!(line, None);
        assert!(why.contains("No such file"), "{why}");
    }
}
