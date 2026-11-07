//! A folder's git checkout as it stands, read from the files under `.git` —
//! no `git` to start, so the list can ask on every refresh.

use crate::badges::Badges;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A folder's git checkout as it stands now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    pub path: PathBuf,
    /// The branch checked out; none on a detached HEAD.
    pub branch: Option<String>,
    /// The commit, short, when HEAD is detached.
    pub detached: Option<String>,
    /// Linked worktrees, counted on the main checkout only.
    pub worktrees: usize,
    /// What the configured badges said of it (`PR #95`), in their order.
    pub badges: Vec<String>,
}

impl Checkout {
    /// What HEAD is, in a word: the branch, or the detached commit.
    pub fn head(&self) -> &str {
        self.branch.as_deref().or(self.detached.as_deref()).unwrap_or("")
    }
}

/// The checkouts of some folders, by folder.
pub type Checkouts = BTreeMap<PathBuf, Checkout>;

/// The folder at or above `dir` that holds a `.git` — a directory, or the
/// file a linked worktree has — so a worktree is a root of its own.
pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

/// A folder's checkout as it stands, read from the files under `.git`.
/// `None` outside a repo. The folder or any above it may hold the `.git`; a
/// linked worktree's is a file naming its real one. Linked worktrees are
/// counted on the main checkout only, and one whose folder is gone does not
/// count.
pub fn checkout(dir: &Path) -> Option<Checkout> {
    let dot = dir.ancestors().map(|d| d.join(".git")).find(|g| g.exists())?;
    let (gitdir, main) = if dot.is_dir() {
        (dot, true)
    } else {
        let text = std::fs::read_to_string(&dot).ok()?;
        let to = text.trim().strip_prefix("gitdir:")?.trim();
        (dot.parent()?.join(to), false)
    };
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = head.trim();
    let (branch, detached) = match head.strip_prefix("ref:") {
        Some(r) => {
            let r = r.trim();
            (Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_string()), None)
        }
        None => (None, Some(head.chars().take(7).collect())),
    };
    let worktrees = if main { linked(&gitdir) } else { 0 };
    Some(Checkout {
        path: dir.to_path_buf(),
        branch,
        detached,
        worktrees,
        badges: Vec::new(),
    })
}

/// The linked worktrees a repository knows of whose folders are still there.
fn linked(gitdir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(gitdir.join("worktrees")) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            // `gitdir` in the entry names the worktree's own `.git` file.
            std::fs::read_to_string(e.path().join("gitdir")).is_ok_and(|g| Path::new(g.trim()).exists())
        })
        .count()
}

/// The checkouts of these folders, each with what the badge `commands` last
/// said of it (and the asking started again when due; see `Badges`). Folders
/// outside a repo are left out.
pub fn checkouts(folders: &[PathBuf], badges: &Badges, commands: &[String]) -> Checkouts {
    folders
        .iter()
        .filter_map(|f| {
            let mut c = checkout(f)?;
            c.badges = badges.of(&c, commands);
            Some((f.clone(), c))
        })
        .collect()
}

/// A checkout on a header: `⎇ main · PR #95 · 3 worktrees` — the badges after
/// the branch, the count on the main checkout only and only when there are any.
pub fn checkout_words(c: &Checkout) -> String {
    let mut words = vec![format!("⎇ {}", c.head())];
    words.extend(c.badges.iter().cloned());
    match c.worktrees {
        0 => {}
        1 => words.push("1 worktree".into()),
        n => words.push(format!("{n} worktrees")),
    }
    words.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(st.success());
    }

    #[test]
    fn a_checkout_is_read_from_its_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("deep")).unwrap();
        assert_eq!(checkout(&repo), None);

        git(&repo, &["init", "-q", "-b", "trunk"]);
        let c = checkout(&repo).unwrap();
        assert_eq!((c.branch.as_deref(), c.detached, c.worktrees), (Some("trunk"), None, 0));
        // A folder inside the repo reads the same HEAD.
        assert_eq!(checkout(&repo.join("deep")).unwrap().head(), "trunk");

        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "one",
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                root.join("wt1").to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "fix", root.join("wt2").to_str().unwrap()],
        );
        assert_eq!(checkout(&repo).unwrap().worktrees, 2);
        // A linked worktree has its own branch and no count.
        let wt = checkout(&root.join("wt1")).unwrap();
        assert_eq!((wt.head(), wt.worktrees), ("feat", 0));
        // One whose folder is gone no longer counts.
        std::fs::remove_dir_all(root.join("wt2")).unwrap();
        assert_eq!(checkout(&repo).unwrap().worktrees, 1);

        git(&repo, &["checkout", "-q", "--detach"]);
        let c = checkout(&repo).unwrap();
        assert_eq!(c.branch, None);
        assert_eq!(c.detached.as_deref().map(str::len), Some(7));
        assert_eq!(c.head(), c.detached.as_deref().unwrap());
    }

    #[test]
    fn the_toplevel_is_the_nearest_folder_with_a_dot_git_file_or_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("a/b")).unwrap();
        assert_eq!(toplevel(&repo.join("a/b")), Some(repo.clone()));
        assert_eq!(toplevel(&repo), Some(repo.clone()));
        // A linked worktree's `.git` is a file; its folder is a root too.
        let wt = repo.join("a/wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(toplevel(&wt), Some(wt));
        assert_eq!(toplevel(&root.join("nowhere")), None);
    }

    #[test]
    fn a_checkout_in_words() {
        let co = |branch: Option<&str>, worktrees, badges: &[&str]| Checkout {
            path: "/w".into(),
            branch: branch.map(Into::into),
            detached: branch.is_none().then(|| "abc1234".to_string()),
            worktrees,
            badges: badges.iter().map(|b| b.to_string()).collect(),
        };
        assert_eq!(checkout_words(&co(Some("main"), 0, &[])), "⎇ main");
        assert_eq!(checkout_words(&co(Some("main"), 1, &[])), "⎇ main · 1 worktree");
        assert_eq!(
            checkout_words(&co(Some("main"), 2, &["PR #95"])),
            "⎇ main · PR #95 · 2 worktrees"
        );
        assert_eq!(checkout_words(&co(None, 0, &[])), "⎇ abc1234");
    }
}
