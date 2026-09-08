//! Reading — never writing — git to place a run: which repo, which branch.

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

#[cfg(test)]
mod tests {
    use super::*;

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
