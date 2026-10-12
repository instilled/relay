//! Reading — never writing — git to place a run: which repo, which branch;
//! and a folder's checkout as it stands, for the list.

use rl_proto::types::Checkout;
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Placement {
    /// Repository top level, if `cwd` is inside one.
    pub toplevel: Option<PathBuf>,
    /// Current branch, if on one (detached HEAD → `None`).
    pub branch: Option<String>,
}

pub async fn placement(cwd: &Path) -> Placement {
    let toplevel = git(cwd, &["rev-parse", "--show-toplevel"]).await.map(PathBuf::from);
    if toplevel.is_none() {
        return Placement::default();
    }
    // symbolic-ref works on an unborn branch (fresh repo) and fails on a
    // detached HEAD, which is exactly the distinction we want.
    let branch = git(cwd, &["symbolic-ref", "--short", "-q", "HEAD"]).await;
    Placement { toplevel, branch }
}

async fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(cwd).args(args).output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// A folder's checkout as it stands, read from the files under `.git` — no
/// `git` to start, so the list can ask on every refresh. `None` outside a repo.
/// The folder or any above it may hold the `.git`; a linked worktree's is a
/// file naming its real one. Linked worktrees are counted on the main
/// checkout only, and one whose folder is gone does not count.
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

    #[tokio::test]
    async fn outside_a_repo_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(placement(tmp.path()).await, Placement::default());
    }

    #[tokio::test]
    async fn inside_a_repo_reports_toplevel_and_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(args)
                .status()
                .unwrap();
            assert!(st.success());
        };
        run(&["init", "-q", "-b", "trunk"]);
        let sub = tmp.path().join("deep/er");
        std::fs::create_dir_all(&sub).unwrap();
        let p = placement(&sub).await;
        assert_eq!(
            p.toplevel.as_deref().map(|t| t.canonicalize().unwrap()),
            Some(tmp.path().canonicalize().unwrap())
        );
        assert_eq!(p.branch.as_deref(), Some("trunk"));
    }
}
