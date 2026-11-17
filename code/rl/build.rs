//! `rl --version` says the commit it was built from, so a bug report can
//! name it: `RL_GIT_COMMIT` is `git rev-parse --short=9 HEAD`, `unknown`
//! outside a git checkout (or without git). Cargo runs this again when HEAD
//! moves: a commit, a checkout, a pull.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn main() {
    let commit = git(&["rev-parse", "--short=9", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=RL_GIT_COMMIT={commit}");
    // The whole version: the release (rl_core::RELEASE's source) and the
    // commit, `2026.11-17-a1b2c3d4e` (the release's tag too), or `dev-a1b2c3d4e`.
    println!("cargo:rerun-if-env-changed=RELAY_RELEASE");
    let release = std::env::var("RELAY_RELEASE").unwrap_or_else(|_| "dev".into());
    println!("cargo:rustc-env=RL_VERSION={release}-{commit}");
    // HEAD itself (a checkout), the branch it points at (a commit), and
    // packed-refs (a gc or a fetch that packs it). In a worktree these live
    // in different git dirs; `--git-path` knows where.
    // A path that does not exist would make cargo run this on every build.
    let mut watch = vec!["HEAD".to_string(), "packed-refs".to_string()];
    watch.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    for path in watch {
        if let Some(p) = git(&["rev-parse", "--git-path", &path])
            && std::path::Path::new(&p).exists()
        {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
