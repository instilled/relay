//! Workspace badges: what a configured command says beside a git folder's
//! branch on its header (`PR #95`). Provisional — the contract the plugin host
//! takes over: a command in, its first line out.
//!
//! Asked for on a list refresh and answered from memory. A folder and branch not
//! asked about yet, or not for `every`, is asked again in the background, and
//! a later refresh carries the answer; a new branch is a new question. Nothing
//! runs unless something is showing the list, and one folder is never asked
//! twice at once.
//!
//! A pull request a harness's session opened is asked of the same way, by
//! `gh pr view <url> --json state`: what colours its `#N` on the list.

use crate::git::Checkout;
use rl_core::Home;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::info;

/// How long an answer stands before it is asked again.
pub const EVERY: Duration = Duration::from_secs(5 * 60);
/// How long a badge command may take before it counts as saying nothing.
pub const WAIT: Duration = Duration::from_secs(10);
/// The longest a badge may be, in characters.
const LONGEST: usize = 40;

/// The badge Relay ships: the open pull request, by `gh`.
pub const GH_PR: &str = include_str!("../plugins/gh-pr");

/// Writes the shipped badges into `plugins/` when they are missing; never
/// over one that is there, which is yours to edit.
pub fn seed(home: &Home) -> anyhow::Result<()> {
    let file = home.plugins_dir().join("gh-pr");
    if file.exists() {
        return Ok(());
    }
    rl_core::atomic::write(&file, GH_PR.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))?;
    }
    info!(file = %file.display(), "badge seeded");
    Ok(())
}

type Key = (PathBuf, String, Vec<String>);

#[derive(Default)]
struct Kept {
    said: Vec<String>,
    at: Option<Instant>,
    asking: bool,
}

pub struct Badges {
    plugins: PathBuf,
    every: Duration,
    wait: Duration,
    kept: Arc<Mutex<HashMap<Key, Kept>>>,
    /// What `gh` said of each pull request, by its URL.
    prs: Arc<Mutex<HashMap<String, Kept>>>,
    gh: PathBuf,
}

impl Badges {
    pub fn new(plugins: PathBuf, every: Duration, wait: Duration) -> Self {
        Self {
            plugins,
            every,
            wait,
            kept: Arc::default(),
            prs: Arc::default(),
            gh: PathBuf::from("gh"),
        }
    }

    /// The same, asking this `gh`.
    pub fn with_gh(mut self, gh: PathBuf) -> Self {
        self.gh = gh;
        self
    }

    /// How the pull request at `href` stands as `gh` last said — `OPEN`,
    /// `MERGED`, `CLOSED` — asking again when due. Nothing until it has
    /// said, and nothing where `gh` is not here or would not say.
    pub fn pr_state(&self, href: &str) -> Option<String> {
        let mut kept = self.prs.lock().unwrap();
        let k = kept.entry(href.to_string()).or_default();
        if !k.asking && k.at.is_none_or(|t| t.elapsed() >= self.every) {
            k.asking = true;
            let (all, gh, wait, href) = (self.prs.clone(), self.gh.clone(), self.wait, href.to_string());
            tokio::spawn(async move {
                let said = ask_gh(&gh, &href, wait).await;
                if let Some(k) = all.lock().unwrap().get_mut(&href) {
                    *k = Kept {
                        said: said.into_iter().collect(),
                        at: Some(Instant::now()),
                        asking: false,
                    };
                }
            });
        }
        k.said.first().cloned()
    }

    /// What `commands` last said of a checkout, asking again when due. A
    /// detached HEAD has no branch to ask about.
    pub fn of(&self, c: &Checkout, commands: &[String]) -> Vec<String> {
        let Some(branch) = &c.branch else {
            return vec![];
        };
        if commands.is_empty() {
            return vec![];
        }
        let key: Key = (c.path.clone(), branch.clone(), commands.to_vec());
        let mut kept = self.kept.lock().unwrap();
        let k = kept.entry(key.clone()).or_default();
        if !k.asking && k.at.is_none_or(|t| t.elapsed() >= self.every) {
            k.asking = true;
            let (all, plugins, wait) = (self.kept.clone(), self.plugins.clone(), self.wait);
            tokio::spawn(async move {
                let (dir, branch, commands) = &key;
                let mut said = Vec::new();
                for cmd in commands {
                    if let Some(line) = ask(cmd, dir, branch, &plugins, wait).await {
                        said.push(line);
                    }
                }
                if let Some(k) = all.lock().unwrap().get_mut(&key) {
                    *k = Kept {
                        said,
                        at: Some(Instant::now()),
                        asking: false,
                    };
                }
            });
        }
        k.said.clone()
    }
}

/// One badge command's first line, or nothing: it failed, printed nothing,
/// or took longer than `wait`.
async fn ask(cmd: &str, dir: &Path, branch: &str, plugins: &Path, wait: Duration) -> Option<String> {
    let path = match std::env::var_os("PATH") {
        Some(p) => {
            let mut all = vec![plugins.to_path_buf()];
            all.extend(std::env::split_paths(&p));
            std::env::join_paths(all).ok()?
        }
        None => plugins.as_os_str().to_owned(),
    };
    let child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .env("PATH", path)
        .env("RELAY_WORKSPACE", dir)
        .env("RELAY_BRANCH", branch)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(wait, child).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(LONGEST).collect())
}

/// `gh pr view <href> --json state`'s word, or nothing.
async fn ask_gh(gh: &Path, href: &str, wait: Duration) -> Option<String> {
    let child = Command::new(gh)
        .args(["pr", "view", href, "--json", "state", "--jq", ".state"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(wait, child).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let word = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(word.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout(dir: &Path, branch: Option<&str>) -> Checkout {
        Checkout {
            path: dir.to_path_buf(),
            branch: branch.map(Into::into),
            detached: None,
            worktrees: 0,
            badges: vec![],
        }
    }

    /// Waits until nothing is being asked: on that, not on a time, though not past a minute.
    async fn idle(b: &Badges) {
        let end = Instant::now() + Duration::from_secs(60);
        while Instant::now() < end {
            if !b.kept.lock().unwrap().values().any(|k| k.asking) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the badges never answered");
    }

    #[tokio::test]
    async fn a_badge_is_a_commands_first_line_asked_again_when_due() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("repo");
        let plugins = tmp.path().join("plugins");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&plugins).unwrap();
        // A badge in plugins/, found by name, counting how often it is asked.
        let count = tmp.path().join("count");
        std::fs::write(
            plugins.join("pr"),
            format!(
                "#!/bin/sh\necho x >> {}\nprintf '\\n  PR #7 on %s  \\nmore\\n' \"$RELAY_BRANCH\"\n",
                count.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(plugins.join("pr"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let asked = || std::fs::read_to_string(&count).map(|s| s.lines().count()).unwrap_or(0);
        let cmds: Vec<String> = ["pr", "exit 3", "true", "basename \"$RELAY_WORKSPACE\""]
            .map(String::from)
            .to_vec();
        // The usual wait: what answers here answers well inside it, however busy the machine.
        // A script run for the first time can take a while — a third of a second on a Mac.
        let b = Badges::new(plugins.clone(), Duration::from_secs(3600), WAIT);
        let main = checkout(&dir, Some("main"));

        // Nothing yet: the first refresh starts the asking.
        assert!(b.of(&main, &cmds).is_empty());
        idle(&b).await;
        // The first non-empty line, trimmed; a failure and silence say nothing;
        // the folder is in the environment.
        assert_eq!(b.of(&main, &cmds), ["PR #7 on main", "repo"]);
        idle(&b).await;
        // Kept, not asked again before it is due.
        assert_eq!(b.of(&main, &cmds), ["PR #7 on main", "repo"]);
        assert_eq!(asked(), 1);

        // A new branch is a new question.
        let feat = checkout(&dir, Some("feat"));
        assert!(b.of(&feat, &cmds).is_empty());
        idle(&b).await;
        assert_eq!(b.of(&feat, &cmds), ["PR #7 on feat", "repo"]);
        assert_eq!(asked(), 2);

        // Once due, asked again — the kept answer stands in the meantime.
        let soon = Badges::new(plugins, Duration::ZERO, WAIT);
        let one = vec!["pr".to_string()];
        soon.of(&main, &one);
        idle(&soon).await;
        assert_eq!(asked(), 3);
        assert_eq!(soon.of(&main, &one), ["PR #7 on main"]);
        idle(&soon).await;
        assert_eq!(asked(), 4);

        // A detached HEAD, or no badges set: nothing asked.
        assert!(b.of(&checkout(&dir, None), &cmds).is_empty());
        assert!(b.of(&main, &[]).is_empty());
    }

    async fn pr_said(b: &Badges, href: &str) -> Option<String> {
        b.pr_state(href);
        let end = Instant::now() + Duration::from_secs(60);
        while b.prs.lock().unwrap().values().any(|k| k.asking) {
            assert!(Instant::now() < end, "gh never answered");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        b.pr_state(href)
    }

    #[tokio::test]
    async fn a_pull_requests_state_is_asked_of_gh_and_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let gh = tmp.path().join("gh");
        let count = tmp.path().join("count");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\necho x >> {}\n[ \"$1 $2 $4 $5 $6 $7\" = 'pr view --json state --jq .state' ] || exit 2\ncase \"$3\" in */9) echo merged ;; *) exit 1 ;; esac\n",
                count.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let b = Badges::new(tmp.path().into(), Duration::from_secs(3600), WAIT).with_gh(gh);
        assert_eq!(b.pr_state("https://x/pull/9"), None, "nothing until it has said");
        assert_eq!(pr_said(&b, "https://x/pull/9").await.as_deref(), Some("MERGED"));
        assert_eq!(pr_said(&b, "https://x/pull/10").await, None, "gh would not say");
        let asked = std::fs::read_to_string(&count).unwrap().lines().count();
        assert_eq!(asked, 2, "kept, not asked again before it is due");
        // No gh here at all: no colour, and no error.
        let none = Badges::new(tmp.path().into(), Duration::from_secs(3600), WAIT).with_gh(tmp.path().join("no-gh"));
        assert_eq!(pr_said(&none, "https://x/pull/9").await, None);
    }

    #[tokio::test]
    async fn a_command_that_takes_too_long_says_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let at = Instant::now();
        let said = ask(
            "sleep 5; echo late",
            tmp.path(),
            "main",
            tmp.path(),
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(said, None);
        assert!(
            at.elapsed() < Duration::from_secs(30),
            "given up at the wait, not at its end"
        );
    }

    #[test]
    fn the_shipped_badge_is_seeded_once_and_then_yours() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        home.ensure_dirs().unwrap();
        seed(&home).unwrap();
        let file = home.plugins_dir().join("gh-pr");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), GH_PR);
        std::fs::write(&file, "#!/bin/sh\necho mine\n").unwrap();
        seed(&home).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "#!/bin/sh\necho mine\n");
    }
}
