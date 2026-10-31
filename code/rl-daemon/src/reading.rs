//! What is read from a session for its queue item: what it last said, its
//! log since you looked, and what changed in git. The report asks on every
//! tick, so what is costly to read is kept until its source moves.

use crate::git;
use crate::server::Daemon;
use crate::summary;
use crate::transcript::{self, Agent};
use anyhow::Result;
use jiff::Timestamp;
use rl_ledger::session_io;
use rl_proto::queue::{Changed, How, LogLine};
use rl_proto::types::Run;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How much of a run's output is read for its last lines.
const SCREEN_BYTES: usize = 4096;
/// How many of a run's last output lines stand for what it last said.
const SCREEN_LINES: usize = 6;
/// How much of a run's output since you looked is its log.
const LOG_BYTES: usize = 64 * 1024;

/// How long a folder's HEAD is taken as read.
const HEAD_FOR: Duration = Duration::from_secs(5);
/// How long what changed is kept at one HEAD: whether it is pushed moves
/// without a commit.
const CHANGED_FOR: Duration = Duration::from_secs(30);
/// How long a session file that was not found is not looked for again.
const MISS_FOR: Duration = Duration::from_secs(30);

/// What had changed in a folder by a HEAD, and when it was read.
struct ChangedAt {
    head: String,
    read: Instant,
    changed: Changed,
}

/// A run's last message, and the session file it was read from at what
/// length.
#[derive(Clone)]
struct LastSaid {
    file: PathBuf,
    len: u64,
    said: Option<String>,
}

/// What was last read, by where it was read from.
#[derive(Default)]
pub struct Reading {
    /// By folder and the HEAD you last saw.
    changed: Mutex<HashMap<(PathBuf, String), ChangedAt>>,
    /// By folder: when its HEAD was read, and what it was.
    heads: Mutex<HashMap<PathBuf, (Instant, Option<String>)>>,
    /// By run.
    last: Mutex<HashMap<String, LastSaid>>,
    /// By run: when its session file was last looked for and not found.
    misses: Mutex<HashMap<String, Instant>>,
}

impl Reading {
    /// Git in `cwd` since `from_head`: its commits, their subjects, `+X/−Y`,
    /// the branch and whether it is pushed. `None` with no HEAD to count
    /// from, or no repo.
    pub async fn changed(&self, cwd: &Path, from_head: Option<&str>) -> Option<Changed> {
        let from = from_head?;
        let head = self.head(cwd).await?;
        let key = (cwd.to_path_buf(), from.to_string());
        if let Some(kept) = self.changed.lock().unwrap().get(&key)
            && kept.head == head
            && kept.read.elapsed() < CHANGED_FOR
        {
            return Some(kept.changed.clone());
        }
        let stat = git::delta(cwd, from, &head).await;
        let subjects = git::log(cwd, from, &head)
            .await
            .into_iter()
            // `--oneline`: the short hash, then the subject.
            .map(|l| l.split_once(' ').map_or(l.clone(), |(_, s)| s.to_string()))
            .collect();
        let branch = git::placement(cwd).await.branch;
        let pushed = match &branch {
            Some(b) => git::pushed(cwd, b).await,
            None => false,
        };
        let c = Changed {
            commits: stat.commits,
            added: stat.added.into(),
            removed: stat.removed.into(),
            subjects,
            branch,
            pushed,
        };
        self.changed.lock().unwrap().insert(
            key,
            ChangedAt {
                head,
                read: Instant::now(),
                changed: c.clone(),
            },
        );
        Some(c)
    }

    /// `cwd`'s HEAD, read again once it is a few seconds old.
    async fn head(&self, cwd: &Path) -> Option<String> {
        if let Some((read, head)) = self.heads.lock().unwrap().get(cwd)
            && read.elapsed() < HEAD_FOR
        {
            return head.clone();
        }
        let head = git::head(cwd).await;
        self.heads
            .lock()
            .unwrap()
            .insert(cwd.to_path_buf(), (Instant::now(), head.clone()));
        head
    }

    /// `run`'s session file was looked for and not found.
    fn missed(&self, run: &str) {
        self.misses.lock().unwrap().insert(run.to_string(), Instant::now());
    }

    /// Whether `run`'s session file was looked for lately and not found.
    fn missed_lately(&self, run: &str) -> bool {
        self.misses
            .lock()
            .unwrap()
            .get(run)
            .is_some_and(|at| at.elapsed() < MISS_FOR)
    }

    /// Forgets all but what is kept for `queued`: each run, with the HEAD
    /// its stretch starts from.
    pub fn prune<'a>(&self, queued: impl IntoIterator<Item = (&'a Run, Option<&'a str>)>) {
        let (mut ids, mut cwds, mut stretches) = (HashSet::new(), HashSet::new(), HashSet::new());
        for (run, from) in queued {
            ids.insert(run.id.as_str());
            cwds.insert(run.cwd.as_path());
            if let Some(from) = from {
                stretches.insert((run.cwd.as_path(), from));
            }
        }
        self.changed
            .lock()
            .unwrap()
            .retain(|(cwd, from), _| stretches.contains(&(cwd.as_path(), from.as_str())));
        self.heads.lock().unwrap().retain(|cwd, _| cwds.contains(cwd.as_path()));
        self.last.lock().unwrap().retain(|id, _| ids.contains(id.as_str()));
        self.misses.lock().unwrap().retain(|id, _| ids.contains(id.as_str()));
    }
}

/// What `run` last said, its first paragraph: for a permission it waits on,
/// what it asks to do; else the agent's last message in its log, else the
/// last lines of its output.
pub async fn last(d: &Daemon, run: &Run, how: How) -> Result<Option<String>> {
    if how == How::Permission
        && let Some(detail) = &run.detail
    {
        return Ok(Some(detail.clone()));
    }
    if let Some(agent) = Agent::of(run)
        && let Some(file) = session_file(d, run).await?
        && let Ok(len) = {
            let path = file.clone();
            tokio::task::spawn_blocking(move || std::fs::metadata(path).map(|m| m.len())).await?
        }
    {
        let kept = d.reading.last.lock().unwrap().get(&run.id).cloned();
        let said = match kept {
            Some(kept) if kept.file == file && kept.len == len => kept.said,
            _ => {
                let path = file.clone();
                let text = tokio::task::spawn_blocking(move || std::fs::read_to_string(path)).await??;
                let said = transcript::last_message(agent, &text).and_then(|m| first_paragraph(&m));
                d.reading.last.lock().unwrap().insert(
                    run.id.clone(),
                    LastSaid {
                        file,
                        len,
                        said: said.clone(),
                    },
                );
                said
            }
        };
        if said.is_some() {
            return Ok(said);
        }
    }
    let id = run.id.clone();
    let bytes = d
        .ledger
        .call_read(move |c| session_io::tail(c, &id, SCREEN_BYTES))
        .await?;
    let lines = screen_lines(&bytes);
    let last = &lines[lines.len().saturating_sub(SCREEN_LINES)..];
    Ok((!last.is_empty()).then(|| last.join("\n")))
}

/// `run`'s session file. One not found lately is not looked for again yet,
/// so the report's every tick does not search for it.
async fn session_file(d: &Daemon, run: &Run) -> Result<Option<PathBuf>> {
    if d.reading.missed_lately(&run.id) {
        return Ok(None);
    }
    let found = summary::locate(d, run).await?;
    if found.is_none() {
        d.reading.missed(&run.id);
    }
    Ok(found)
}

/// `run`'s log from `from` on: the messages in the agent's log, else its
/// output's lines, each said by `shell` at `at`.
pub async fn log(d: &Daemon, run: &Run, from: Timestamp, at: Timestamp) -> Result<Vec<LogLine>> {
    if let Some(agent) = Agent::of(run)
        && let Some(file) = summary::locate(d, run).await?
    {
        let text = tokio::task::spawn_blocking(move || std::fs::read_to_string(file)).await?;
        if let Ok(text) = text {
            return Ok(transcript::log_lines(agent, &text, Some(from)));
        }
    }
    let id = run.id.clone();
    let bytes = d
        .ledger
        .call_read(move |c| session_io::tail_since(c, &id, from, LOG_BYTES))
        .await?;
    Ok(screen_lines(&bytes)
        .into_iter()
        .map(|text| LogLine {
            at,
            who: "shell".into(),
            text,
        })
        .collect())
}

/// Output as plain lines, blank ones left out.
fn screen_lines(bytes: &[u8]) -> Vec<String> {
    rl_detect::ansi::strip(bytes)
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// The first paragraph of `text`: its lines up to the first blank one.
fn first_paragraph(text: &str) -> Option<String> {
    let para: Vec<&str> = text
        .lines()
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .collect();
    (!para.is_empty()).then(|| para.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[tokio::test]
    async fn changed_outside_a_repo_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let reading = Reading::default();
        assert_eq!(reading.changed(tmp.path(), Some("abc1234")).await, None);
        assert_eq!(reading.changed(tmp.path(), None).await, None);
    }

    #[tokio::test]
    async fn changed_counts_commits_since_from() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        let from = git(repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        git(repo, &["add", "a.txt"]);
        git(repo, &["commit", "-q", "-m", "add a"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(repo, &["commit", "-q", "-am", "trim a"]);

        let reading = Reading::default();
        let c = reading.changed(repo, Some(&from)).await.unwrap();
        assert_eq!(c.commits, 2);
        assert_eq!((c.added, c.removed), (1, 0), "the net diff: a.txt with one line");
        assert_eq!(c.subjects, ["trim a", "add a"]);
        assert_eq!(c.branch.as_deref(), Some("main"));
        assert!(!c.pushed, "no upstream");
        // Read again at the same HEAD: kept.
        assert_eq!(reading.changed(repo, Some(&from)).await, Some(c));
        // With no HEAD to count from, nothing is said.
        assert_eq!(reading.changed(repo, None).await, None);
    }

    #[tokio::test]
    async fn changed_is_kept_while_its_head_is_fresh() {
        // The report asks every second: a commit is seen once HEAD is read
        // again, not on every tick.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        let from = git(repo, &["rev-parse", "HEAD"]);
        let reading = Reading::default();
        assert_eq!(reading.changed(repo, Some(&from)).await.unwrap().commits, 0);
        git(repo, &["commit", "-q", "--allow-empty", "-m", "second"]);
        assert_eq!(reading.changed(repo, Some(&from)).await.unwrap().commits, 0, "kept");
        reading.heads.lock().unwrap().clear();
        assert_eq!(
            reading.changed(repo, Some(&from)).await.unwrap().commits,
            1,
            "read again"
        );
    }

    #[tokio::test]
    async fn what_is_kept_is_only_for_queued_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        let from = git(repo, &["rev-parse", "HEAD"]);
        let reading = Reading::default();
        reading.changed(repo, Some(&from)).await;
        reading.missed("gone");
        reading.missed("here");
        let mut here = Run::new(
            "here",
            rl_proto::types::RunKind::Agent,
            "t",
            vec![],
            repo,
            repo,
            Timestamp::now(),
        );
        here.cli = None;
        reading.prune([(&here, Some(from.as_str()))]);
        assert!(reading.misses.lock().unwrap().contains_key("here"));
        assert!(!reading.misses.lock().unwrap().contains_key("gone"));
        assert_eq!(reading.changed.lock().unwrap().len(), 1, "here's folder and HEAD");
        reading.prune([]);
        assert!(reading.changed.lock().unwrap().is_empty());
        assert!(reading.heads.lock().unwrap().is_empty());
        assert!(reading.misses.lock().unwrap().is_empty());
    }

    #[test]
    fn a_miss_is_remembered() {
        let reading = Reading::default();
        assert!(!reading.missed_lately("r1"));
        reading.missed("r1");
        assert!(reading.missed_lately("r1"));
    }

    #[test]
    fn the_first_paragraph_is_kept() {
        assert_eq!(
            first_paragraph("\n\nFixed it.\nAll green.\n\nDetails follow.").as_deref(),
            Some("Fixed it.\nAll green.")
        );
        assert_eq!(first_paragraph("  \n"), None);
    }
}
