//! `~/.relay/flows` read into the ledger: each folder hashed, its `flow.md`
//! checked, and what did not load said with its file and line.

use super::engine::{CHANGED, CHANGED_SINCE_BEGAN};
use super::{Flows, canonical_in};
use anyhow::{Context, Result};
use jiff::Timestamp;
use rl_flow::{def, doc, hash};
use rl_ledger::flows::{self, Loaded};
use rl_ledger::workspaces;
use rl_proto::flow::{NotLoaded, Reloaded};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// What one folder came to.
enum Read {
    Loaded(Loaded),
    /// It was read, and is wrong: left out.
    Broken(Vec<NotLoaded>),
    /// It could not be read: left as it last loaded.
    Unreadable(NotLoaded),
}

impl Flows {
    /// Reads every flow folder again. Good flows are written to the ledger,
    /// a changed one waiting for a yes; one that does not load is left out
    /// and said; one that cannot be read stays as it last loaded.
    pub async fn reload(&self) -> Result<Reloaded> {
        let _reading = self.reloading.lock().await;
        let dir = self.home.flows_dir();
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            std::fs::write(&ignore, "runs/\n").with_context(|| format!("writing {}", ignore.display()))?;
        }
        let known: BTreeSet<PathBuf> = self
            .ledger
            .call(|c| workspaces::list(c))
            .await?
            .into_iter()
            .map(|w| w.path)
            .collect();
        let last: BTreeMap<String, Loaded> = self
            .ledger
            .call(|c| flows::list(c, false))
            .await?
            .into_iter()
            .map(|f| {
                let l = Loaded {
                    name: f.name.clone(),
                    hash: f.hash,
                    def_json: f.def.to_string(),
                };
                (f.name, l)
            })
            .collect();
        let mut loaded = Vec::new();
        let mut kept = Vec::new();
        let mut broken = Vec::new();
        for name in folders(&dir)? {
            match read(&self.home.flow_dir(&name), &name, &known) {
                Read::Loaded(l) => loaded.push(l),
                Read::Broken(said) => broken.extend(said),
                Read::Unreadable(said) => {
                    broken.push(said);
                    kept.extend(last.get(&name).cloned());
                }
            }
        }
        // Loaded changed or back: planned from now, not from a firing long past. Marked before
        // the sync clears their `next_at`.
        for l in loaded
            .iter()
            .filter(|l| last.get(&l.name).is_none_or(|was| was.hash != l.hash))
        {
            self.plan_afresh(&l.name);
        }
        let fresh = loaded.len();
        loaded.extend(kept);
        let now = Timestamp::now();
        let mut synced = self.ledger.call(move |c| flows::sync(c, &loaded, now)).await?;
        // A flow that did not load is left out, not gone: its folder is still there.
        synced.removed.retain(|name| !broken.iter().any(|n| n.flow == *name));
        for n in &broken {
            warn!(flow = %n, "flow not loaded");
        }
        info!(
            loaded = fresh,
            added = synced.added.len(),
            changed = synced.changed.len(),
            removed = synced.removed.len(),
            "flows reloaded"
        );
        *self.broken.lock().unwrap() = broken.clone();
        self.wake.notify_one();
        Ok(Reloaded {
            loaded: fresh,
            added: synced.added,
            changed: synced.changed,
            removed: synced.removed,
            not_loaded: broken,
        })
    }

    /// Why nothing of `name` may start now, if nothing may: its folder, read and hashed now, is
    /// not the one said yes to ([`CHANGED`]: nothing watches it between reloads), or the yes is
    /// no longer to the version a firing `began` on ([`CHANGED_SINCE_BEGAN`]). Asked before a
    /// firing starts and before each step does. A folder gone, unreadable, or holding a link to
    /// a folder never has its yes.
    pub(super) async fn why_not_run(&self, name: &str, began: Option<&str>) -> Option<&'static str> {
        let n = name.to_string();
        let approved = match self.ledger.call(move |c| flows::get(c, &n)).await {
            Ok(f) => f.filter(|f| f.removed_at.is_none()).and_then(|f| f.approved_hash),
            Err(e) => {
                warn!(flow = name, error = %format!("{e:#}"), "reading a flow's yes");
                None
            }
        };
        let Some(yes) = approved else { return Some(CHANGED) };
        if hash_now(&self.home.flow_dir(name)).is_none_or(|now| now != yes) {
            return Some(CHANGED);
        }
        began.is_some_and(|b| b != yes).then_some(CHANGED_SINCE_BEGAN)
    }

    /// The folders read again once one is found changed under its yes, so it says it needs one.
    pub(super) async fn changed_since_yes(&self, name: &str) {
        info!(flow = name, "flow changed since its yes");
        if let Err(e) = self.reload().await {
            warn!(flow = name, error = %format!("{e:#}"), "reading the flows again");
        }
    }
}

/// The hash of `folder` as it is now; `None` when it cannot be read or holds a link to a folder.
fn hash_now(folder: &Path) -> Option<String> {
    let read = rl_core::folder::read(folder).ok()?;
    read.linked.is_empty().then(|| hash::folder(&read.files))
}

/// The folders under `dir` that can be flows, by name: no dotfiles, no names
/// a flow cannot have.
fn folders(dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !def::valid_name(&name) || !entry.path().is_dir() {
            continue;
        }
        names.push(name);
    }
    names.sort();
    Ok(names)
}

/// One folder: hashed, its `flow.md` read and checked. `known` holds the
/// registered workspaces, by their canonical paths.
fn read(folder: &Path, name: &str, known: &BTreeSet<PathBuf>) -> Read {
    let file = folder.join("flow.md");
    let said = |line: Option<usize>, column: Option<usize>, message: String| NotLoaded {
        flow: name.to_string(),
        file: file.clone(),
        line,
        column,
        message,
    };
    let (files, linked) = match rl_core::folder::read(folder) {
        Ok(f) => (f.files, f.linked),
        Err(e) => {
            return Read::Unreadable(NotLoaded {
                file: folder.to_path_buf(),
                ..said(None, None, format!("cannot be read: {e:#}"))
            });
        }
    };
    if !linked.is_empty() {
        let message = "a link to a folder: what is in it would run without a yes — copy it in instead";
        return Read::Broken(
            linked
                .iter()
                .map(|p| NotLoaded {
                    file: folder.join(p),
                    ..said(None, None, message.to_string())
                })
                .collect(),
        );
    }
    let Some((_, bytes)) = files.iter().find(|(p, _)| p == Path::new("flow.md")) else {
        return Read::Broken(vec![said(None, None, "a flow's folder needs a flow.md".to_string())]);
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Read::Broken(vec![said(None, None, "flow.md is not UTF-8 text".to_string())]);
    };
    let read = match doc::read(text) {
        Ok(d) => d,
        Err(p) => return Read::Broken(vec![said(p.line, p.column, p.message)]),
    };
    let is_known = |p: &Path| known.contains(&canonical_in(p));
    match def::check(name, read, &is_known) {
        Ok(d) => Read::Loaded(Loaded {
            name: name.to_string(),
            hash: hash::folder(&files),
            def_json: serde_json::to_string(&d).expect("a flow serialises"),
        }),
        Err(problems) => Read::Broken(
            problems
                .into_iter()
                .map(|p| said(p.line, p.column, p.message))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reload waits for the one going: it never reads the folders while another is still to sync.
    #[tokio::test]
    async fn a_reload_waits_for_the_one_going() {
        use crate::flows::Flows;
        use rl_core::Home;
        use rl_ledger::Ledger;
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("home"));
        std::fs::create_dir_all(home.flows_dir()).unwrap();
        let ledger = Ledger::open(&home.ledger()).unwrap();
        let registry = Arc::new(rl_detect::Registry::builtin());
        let token = CancellationToken::new();
        let supervisor = Arc::new(crate::supervisor::Supervisor::new(
            home.clone(),
            ledger.clone(),
            PathBuf::from("rl-session"),
            registry.clone(),
            token.child_token(),
            std::time::Duration::from_secs(1),
        ));
        let decisions = Arc::new(crate::decisions::Decisions::new(
            ledger.clone(),
            supervisor.clone(),
            registry.clone(),
            token.child_token(),
        ));
        let flows = Flows::new(home, ledger, supervisor, decisions, registry, None, token.child_token());

        let held = flows.reloading.lock().await;
        let other = tokio::spawn({
            let flows = flows.clone();
            async move { flows.reload().await.map(|_| ()) }
        });
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!other.is_finished(), "a reload ran while another held the lock");
        drop(held);
        other.await.unwrap().unwrap();
        token.cancel();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_folder_does_not_load() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("linked");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("apply.sh"), "rm -rf /").unwrap();
        std::fs::write(dir.join("flow.md"), "---\nsteps:\n  a:\n    run: lib/apply.sh\n---\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("lib")).unwrap();
        let Read::Broken(said) = read(&dir, "linked", &BTreeSet::new()) else {
            panic!("loaded")
        };
        assert_eq!(said[0].file, dir.join("lib"));
        assert!(said[0].message.contains("a link to a folder"), "{said:?}");
    }

    #[test]
    fn a_folder_without_a_flow_md_is_said() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "x").unwrap();
        let Read::Broken(said) = read(tmp.path(), "empty", &BTreeSet::new()) else {
            panic!("not broken")
        };
        assert_eq!(said[0].message, "a flow's folder needs a flow.md");
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_read_is_said() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("locked");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("flow.md"), "---\nsteps:\n  a:\n    run: x\n---\n").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads it all the same, and then there is nothing to say.
        let readable = std::fs::read_dir(&dir).is_ok();
        let got = read(&dir, "locked", &BTreeSet::new());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        if readable {
            return;
        }
        let Read::Unreadable(said) = got else { panic!("read") };
        assert_eq!(said.file, dir);
        assert!(said.message.starts_with("cannot be read"), "{said:?}");
    }
}
