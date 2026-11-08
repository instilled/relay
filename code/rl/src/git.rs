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
/// file a linked worktree has — so a worktree is a root of its own. One at
/// `$HOME` itself is a dotfiles repo, not a project's: passed over.
pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    toplevel_under(dir, home.as_deref())
}

fn toplevel_under(dir: &Path, home: Option<&Path>) -> Option<PathBuf> {
    dir.ancestors()
        .find(|d| Some(*d) != home && d.join(".git").exists())
        .map(Path::to_path_buf)
}

/// The project a folder belongs to: its `toplevel`, except that a linked
/// worktree belongs to its main checkout.
pub fn project(dir: &Path) -> Option<PathBuf> {
    let top = toplevel(dir)?;
    Some(main_of(&top).unwrap_or(top))
}

/// The main checkout of the linked worktree at `top`: its `.git` file names
/// its entry under the main `.git/worktrees/` (`gitdir:`, relative to the
/// worktree), whose `commondir` names the common `.git` (relative to the
/// entry); the project is that folder's parent. None for anything else — a
/// `.git` folder, or a submodule's file, whose entry has no `commondir`.
fn main_of(top: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(top.join(".git")).ok()?;
    let entry = clean(&top.join(text.trim().strip_prefix("gitdir:")?.trim()));
    let common = std::fs::read_to_string(entry.join("commondir")).ok()?;
    let common = clean(&entry.join(common.trim()));
    if !common.is_dir() {
        return None;
    }
    common.parent().map(Path::to_path_buf)
}

/// A path with its `.` and `..` worked out by name, links left as they are,
/// so a project reads the way its folders were named.
fn clean(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// A folder's checkout as it stands, read from the files under `.git`.
/// `None` outside a repo. The folder or any above it may hold the `.git`; a
/// linked worktree's is a file naming its real one. Linked worktrees are
/// counted on the main checkout only, and one whose folder is gone does not
/// count.
pub fn checkout(dir: &Path) -> Option<Checkout> {
    let dot = toplevel(dir)?.join(".git");
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
    fn a_worktree_belongs_to_its_main_checkout_and_a_submodule_stands_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        // The main checkout, and a linked worktree inside it, as git lays
        // them out: `.git/worktrees/foo/{gitdir,commondir}` and the
        // worktree's `.git` file pointing there.
        let main = root.join("relay");
        let entry = main.join(".git/worktrees/foo");
        std::fs::create_dir_all(&entry).unwrap();
        let wt = main.join(".claude/worktrees/foo");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", entry.display())).unwrap();
        std::fs::write(entry.join("gitdir"), format!("{}\n", wt.join(".git").display())).unwrap();
        std::fs::write(entry.join("commondir"), "../..\n").unwrap();
        assert_eq!(project(&wt.join("src")), Some(main.clone()));
        assert_eq!(project(&wt), Some(main.clone()));
        assert_eq!(project(&main), Some(main.clone()));
        // A relative `gitdir:` is the worktree's own.
        let wt2 = root.join("elsewhere/bar");
        let entry2 = main.join(".git/worktrees/bar");
        std::fs::create_dir_all(&entry2).unwrap();
        std::fs::create_dir_all(&wt2).unwrap();
        std::fs::write(wt2.join(".git"), "gitdir: ../../relay/.git/worktrees/bar\n").unwrap();
        std::fs::write(entry2.join("commondir"), "../..\n").unwrap();
        assert_eq!(project(&wt2), Some(main.clone()));
        // A submodule's `.git` points into its parent's `.git/modules`, with
        // no `commondir`: a project of its own.
        let sub = main.join("vendor/lib");
        let module = main.join(".git/modules/lib");
        std::fs::create_dir_all(&module).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../../.git/modules/lib\n").unwrap();
        assert_eq!(project(&sub), Some(sub.clone()));
        // A `.git` file that names nothing there is its own folder's.
        let odd = root.join("odd");
        std::fs::create_dir_all(&odd).unwrap();
        std::fs::write(odd.join(".git"), "gitdir: /nowhere\n").unwrap();
        assert_eq!(project(&odd), Some(odd.clone()));
        assert_eq!(project(&root.join("none")), None);
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
        // A dotfiles repo at home is no project's root.
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".git")).unwrap();
        std::fs::create_dir_all(home.join("src/x")).unwrap();
        assert_eq!(toplevel_under(&home.join("src/x"), Some(&home)), None);
        assert_eq!(toplevel_under(&home, Some(&home)), None);
        assert_eq!(toplevel_under(&home.join("src/x"), None), Some(home.clone()));
        std::fs::create_dir_all(home.join("src/.git")).unwrap();
        assert_eq!(toplevel_under(&home.join("src/x"), Some(&home)), Some(home.join("src")));
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
