//! Parking: a flow put away without losing it. Its folder is renamed `_<name>`, which no flow can
//! be called, so the next reload finds it gone; its ended runs are archived and its yes taken, so
//! brought back it needs one again however alike its folder is.

use super::{Flows, canonical_in};
use anyhow::{Context, Result};
use jiff::Timestamp;
use rl_flow::doc;
use rl_ledger::{firings, flows, runs};
use rl_proto::flow::{Parked, parked_name, valid_name};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Why a flow was not parked or brought back: the asker's to fix, said as it is.
#[derive(Debug)]
pub enum Unmoved {
    /// There is no flow folder by that name.
    Missing(String),
    /// A firing of it is still open.
    Firing(String),
    /// The folder is not one parking makes.
    NotParked(String),
    /// A flow has the name it would come back as.
    Taken(String),
}

impl fmt::Display for Unmoved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unmoved::Missing(name) => write!(f, "no flow `{name}`"),
            Unmoved::Firing(name) => write!(f, "{name} is firing · /stop its run, then park it"),
            Unmoved::NotParked(folder) => write!(f, "{folder} is not a parked flow"),
            Unmoved::Taken(name) => write!(f, "a flow named {name} is there now · park or rename it first"),
        }
    }
}

impl std::error::Error for Unmoved {}

impl Flows {
    /// Parks `name`: refused while a firing of it is open. Once the folder is renamed it stays
    /// parked: a step after that going wrong is said, and the folder is not put back.
    pub async fn park(&self, name: &str) -> Result<Parked> {
        // Held throughout, as `fire` holds it, so no firing starts between the look and the rename.
        let _going = self.going.lock().await;
        let from = self.home.flow_dir(name);
        if !valid_name(name) || !from.is_dir() {
            return Err(Unmoved::Missing(name.to_string()).into());
        }
        let n = name.to_string();
        let firing = self
            .ledger
            .call(move |c| Ok(firings::open(c)?.into_iter().any(|f| f.flow == n)))
            .await?;
        if firing {
            return Err(Unmoved::Firing(name.to_string()).into());
        }
        let dir = self.home.flows_dir();
        let folder = free_folder(&dir, name);
        let to = dir.join(&folder);
        std::fs::rename(&from, &to).with_context(|| format!("renaming {} to {}", from.display(), to.display()))?;
        let at = touched(&to);
        let n = name.to_string();
        let archived = self
            .ledger
            .call(move |c| {
                let archived = runs::archive_flow_runs(c, &n, Timestamp::now())?;
                flows::unapprove(c, &n)?;
                Ok(archived)
            })
            .await
            .with_context(|| format!("{name} is parked as {folder}, but its runs and its yes are as they were"))?;
        self.reload()
            .await
            .with_context(|| format!("{name} is parked as {folder}, but the flows were not read again"))?;
        Ok(Parked {
            name: name.to_string(),
            in_: read_in(&to),
            folder,
            at,
            archived,
        })
    }

    /// Brings the flow parked in `folder` back under its name, and reads the flows again. The
    /// name it comes back as.
    pub async fn unpark(&self, folder: &str) -> Result<String> {
        let dir = self.home.flows_dir();
        let from = dir.join(folder);
        let Some(name) = parked_name(folder).filter(|_| from.is_dir()) else {
            return Err(Unmoved::NotParked(folder.to_string()).into());
        };
        let to = self.home.flow_dir(name);
        if to.symlink_metadata().is_ok() {
            return Err(Unmoved::Taken(name.to_string()).into());
        }
        std::fs::rename(&from, &to).with_context(|| format!("renaming {} to {}", from.display(), to.display()))?;
        self.reload()
            .await
            .with_context(|| format!("{name} is back, but the flows were not read again"))?;
        Ok(name.to_string())
    }

    /// The parked folders in `~/.relay/flows`, by name and then by when.
    pub fn parked(&self) -> Result<Vec<Parked>> {
        let dir = self.home.flows_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
        };
        let mut all = Vec::new();
        for entry in entries {
            let entry = entry?;
            let folder = entry.file_name().to_string_lossy().into_owned();
            let Some(name) = parked_name(&folder) else { continue };
            let Ok(meta) = std::fs::metadata(entry.path()) else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            all.push(Parked {
                name: name.to_string(),
                in_: read_in(&entry.path()),
                at: meta
                    .modified()
                    .ok()
                    .and_then(timestamp)
                    .unwrap_or(Timestamp::UNIX_EPOCH),
                folder,
                archived: 0,
            });
        }
        all.sort_by(|a, b| (&a.name, a.at).cmp(&(&b.name, b.at)));
        Ok(all)
    }
}

/// `_<name>`, or while that is taken `_<name>.2`, `_<name>.3`…: never one that is there.
fn free_folder(dir: &Path, name: &str) -> String {
    let first = format!("_{name}");
    if dir.join(&first).symlink_metadata().is_err() {
        return first;
    }
    (2..)
        .map(|n| format!("_{name}.{n}"))
        .find(|f| dir.join(f).symlink_metadata().is_err())
        .expect("a free number")
}

/// The folder's time set to now, so a list says when it was parked; now, if it cannot be.
fn touched(folder: &Path) -> Timestamp {
    let now = SystemTime::now();
    let set = std::fs::File::open(folder).and_then(|f| f.set_modified(now));
    let at = set.ok().and_then(|()| std::fs::metadata(folder).ok()?.modified().ok());
    timestamp(at.unwrap_or(now)).unwrap_or_else(Timestamp::now)
}

fn timestamp(t: SystemTime) -> Option<Timestamp> {
    Timestamp::try_from(t).ok()
}

/// The `in` a parked folder's `flow.md` names, resolved as a flow's is. `None` when it names
/// none, or cannot be read.
fn read_in(folder: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(folder.join("flow.md")).ok()?;
    let read = doc::read(&text).ok()?;
    read.spec.in_.map(|p| canonical_in(&p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_folder_takes_the_next_number() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(free_folder(tmp.path(), "foo"), "_foo");
        std::fs::create_dir(tmp.path().join("_foo")).unwrap();
        assert_eq!(free_folder(tmp.path(), "foo"), "_foo.2");
        std::fs::write(tmp.path().join("_foo.2"), "a file holds the name too").unwrap();
        assert_eq!(free_folder(tmp.path(), "foo"), "_foo.3");
    }
}
