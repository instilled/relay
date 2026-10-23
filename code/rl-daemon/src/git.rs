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

/// The commit `HEAD` names. `None` outside a repo or on an unborn branch.
pub async fn head(cwd: &Path) -> Option<String> {
    git(cwd, &["rev-parse", "HEAD"]).await
}

/// What a run changed between two commits.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiffStat {
    pub commits: u32,
    pub added: u32,
    pub removed: u32,
    pub files: u32,
    /// Up to three top-level dirs (or root files) by churn, biggest first.
    pub top_paths: Vec<String>,
}

/// The commits in `from..to` and the summed diff-stat between them. Empty when
/// not a repo, either commit is unknown, or `from == to`.
pub async fn delta(cwd: &Path, from: &str, to: &str) -> DiffStat {
    if from == to {
        return DiffStat::default();
    }
    let range = format!("{from}..{to}");
    let Some(log) = git(cwd, &["log", "--oneline", &range]).await else {
        return DiffStat::default();
    };
    let mut stat = DiffStat {
        commits: log.lines().count() as u32,
        ..DiffStat::default()
    };
    let Some(numstat) = git(cwd, &["diff", "--numstat", from, to]).await else {
        return stat;
    };
    let mut churn: Vec<(String, u32)> = Vec::new();
    for line in numstat.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(a), Some(r), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        // A binary file shows `-` for both numbers: a changed file, no churn.
        let (a, r) = (a.parse::<u32>().unwrap_or(0), r.parse::<u32>().unwrap_or(0));
        stat.added += a;
        stat.removed += r;
        stat.files += 1;
        let top = path.split_once('/').map_or(path, |(dir, _)| dir).to_string();
        match churn.iter_mut().find(|(p, _)| *p == top) {
            Some((_, n)) => *n += a + r,
            None => churn.push((top, a + r)),
        }
    }
    churn.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
    stat.top_paths = churn.into_iter().take(3).map(|(p, _)| p).collect();
    stat
}

/// The one-line subjects of the commits in `from..to`, newest first. Empty
/// when not a repo, either commit is unknown, or `from == to`.
pub async fn log(cwd: &Path, from: &str, to: &str) -> Vec<String> {
    if from == to {
        return Vec::new();
    }
    let range = format!("{from}..{to}");
    git(cwd, &["log", "--oneline", &range])
        .await
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Whether `branch` has an upstream that already holds `HEAD`. Reads local
/// refs only — no fetch — so it is as fresh as the last one. `false` on any failure.
pub async fn pushed(cwd: &Path, branch: &str) -> bool {
    let spec = format!("{branch}@{{u}}");
    let Some(upstream) = git(cwd, &["rev-parse", "--abbrev-ref", &spec]).await else {
        return false;
    };
    git(cwd, &["branch", "-r", "--contains", "HEAD", "--list", &upstream])
        .await
        .is_some()
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

    /// Run git in a throwaway repo, independent of the host's git config.
    fn sh(dir: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(st.success());
    }

    #[tokio::test]
    async fn delta_counts_commits_and_churn() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        sh(d, &["init", "-q", "-b", "trunk"]);
        std::fs::write(d.join("keep.txt"), "a\nb\n").unwrap();
        sh(d, &["add", "."]);
        sh(d, &["commit", "-q", "-m", "base"]);
        let from = head(d).await.unwrap();

        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/a.rs"), "1\n2\n3\n").unwrap();
        sh(d, &["add", "."]);
        sh(d, &["commit", "-q", "-m", "one"]);
        std::fs::write(d.join("keep.txt"), "a\n").unwrap();
        std::fs::write(d.join("src/b.rs"), "x\n").unwrap();
        sh(d, &["add", "."]);
        sh(d, &["commit", "-q", "-m", "two"]);
        let to = head(d).await.unwrap();

        let s = delta(d, &from, &to).await;
        assert_eq!((s.commits, s.added, s.removed, s.files), (2, 4, 1, 3));
        assert_eq!(s.top_paths, vec!["src".to_string(), "keep.txt".to_string()]);
        assert_eq!(delta(d, &to, &to).await, DiffStat::default());
        assert_eq!(delta(d, "nope", &to).await, DiffStat::default());
    }

    #[tokio::test]
    async fn delta_outside_repo_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(delta(tmp.path(), "a", "b").await, DiffStat::default());
    }

    #[tokio::test]
    async fn head_outside_repo_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(head(tmp.path()).await, None);
    }

    #[tokio::test]
    async fn pushed_is_false_without_an_upstream() {
        let tmp = tempfile::tempdir().unwrap();
        sh(tmp.path(), &["init", "-q", "-b", "trunk"]);
        sh(tmp.path(), &["commit", "-q", "--allow-empty", "-m", "x"]);
        assert!(!pushed(tmp.path(), "trunk").await);
    }
}
