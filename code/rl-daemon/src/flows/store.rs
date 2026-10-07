//! `~/.relay/flows` read into the ledger: each folder hashed, its `flow.md`
//! checked, and what did not load said with its file and line.

use super::Flows;
use crate::action::expand_home;
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
    let files = match files(folder) {
        Ok(f) => f,
        Err(e) => {
            return Read::Unreadable(NotLoaded {
                file: folder.to_path_buf(),
                ..said(None, None, format!("cannot be read: {e:#}"))
            });
        }
    };
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
    let is_known = |p: &Path| std::fs::canonicalize(expand_home(p)).is_ok_and(|c| known.contains(&c));
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

/// Every file under `folder` but `runs/`, by its path in the folder. A link
/// to a file counts as the file; a link to a folder is not followed, so it
/// cannot loop; a pipe, socket or device is passed over, so reading never
/// waits on one.
fn files(folder: &Path) -> std::io::Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut todo = vec![PathBuf::new()];
    while let Some(rel) = todo.pop() {
        for entry in std::fs::read_dir(folder.join(&rel))? {
            let entry = entry?;
            let path = rel.join(entry.file_name());
            if path == Path::new("runs") {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                todo.push(path);
                continue;
            }
            let is_file =
                kind.is_file() || (kind.is_symlink() && std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file()));
            if is_file {
                out.push((path, std::fs::read(entry.path())?));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_every_file_in_it_but_its_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("runs/f1")).unwrap();
        std::fs::write(dir.join("flow.md"), "x").unwrap();
        std::fs::write(dir.join("lib/a.sh"), "a").unwrap();
        std::fs::write(dir.join("runs/f1/probe.out"), "o").unwrap();
        let mut got: Vec<PathBuf> = files(dir).unwrap().into_iter().map(|(p, _)| p).collect();
        got.sort();
        assert_eq!(got, [PathBuf::from("flow.md"), PathBuf::from("lib/a.sh")]);
    }

    #[cfg(unix)]
    #[test]
    fn a_pipe_or_a_link_back_up_is_passed_over() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("flow.md"), "x").unwrap();
        std::fs::write(dir.join("lib/a.sh"), "a").unwrap();
        nix::unistd::mkfifo(&dir.join("pipe"), nix::sys::stat::Mode::S_IRWXU).unwrap();
        std::os::unix::fs::symlink(dir, dir.join("lib/up")).unwrap();
        std::os::unix::fs::symlink(dir.join("lib/a.sh"), dir.join("b.sh")).unwrap();
        let mut got: Vec<PathBuf> = files(dir).unwrap().into_iter().map(|(p, _)| p).collect();
        got.sort();
        assert_eq!(got, ["b.sh", "flow.md", "lib/a.sh"].map(PathBuf::from));
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
