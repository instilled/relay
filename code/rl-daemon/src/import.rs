//! Bringing agents' own sessions into Relay. `claude --bg` sessions, and any
//! agent's open in a terminal, are the agent's to supervise; moving one
//! carries it on as an ordinary Relay run, in the same folder, which becomes
//! a workspace, as the agent's profile resumes a conversation (`claude
//! --resume`, `codex resume`). A Claude background process is stopped
//! first — the conversation stays in its transcript; one in a terminal is
//! carried on only while idle, and that terminal keeps its own. Only `claude
//! agents --json` and `claude stop` are asked: no model is called.

use crate::server::Daemon;
use anyhow::{Result, anyhow};
use rl_detect::Profile;
use rl_ledger::meta;
use rl_proto::methods::{ImportListParams, ImportMoveParams, ImportReport, Outside, RunSpec};
use rl_proto::types::{Cause, Run, RunKind};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Claude's `outside::tag`, `claude-session:<id>`, on the run that carries a
/// session on: one moved here, and any Claude run whose `SessionStart` hook
/// names it.
pub const TAG: &str = "claude-session:";
/// The `meta` key: sessions the panel has mentioned at start already.
const MENTIONED: &str = "import.mentioned";
/// `claude stop`, and `claude agents` for the import guide, get this long.
const ASK_TIMEOUT: Duration = Duration::from_secs(20);
/// `claude agents` on the way to carrying one on: it takes a tenth of a
/// second, and someone is waiting.
const LIST_NOW: Duration = Duration::from_secs(3);
/// A session told of by hooks that was heard from this recently may be
/// between words: it is not carried on yet.
pub const HEARD_QUIET: Duration = Duration::from_secs(3);
/// Nor one whose conversation's file changed this recently: a working agent
/// keeps writing it, whatever word was lost.
pub const WRITTEN_QUIET: Duration = Duration::from_secs(5);

fn claude_bin(d: &Daemon) -> Result<PathBuf> {
    crate::outside::claude::claude_bin(&d.registry)
}

async fn ask(bin: &Path, args: &[&str]) -> Result<String> {
    crate::outside::lister::ask(bin, args, ASK_TIMEOUT).await
}

/// Claude's sessions now, each with the run that carries it on, if any, and
/// whether the panel has mentioned it before. One that went on from an
/// ended run (`outside::went_from`: a run sent to Claude's background) is
/// Claude's again, not that run's: it can be moved back, and says which
/// run it was rather than being news.
pub async fn list(d: &Daemon, p: ImportListParams) -> ImportReport {
    match sessions(d, p.mark, ASK_TIMEOUT).await {
        Ok(sessions) => ImportReport { sessions, error: None },
        Err(e) => ImportReport {
            sessions: vec![],
            error: Some(format!("{e:#}")),
        },
    }
}

async fn sessions(d: &Daemon, mark: bool, wait: Duration) -> Result<Vec<Outside>> {
    let bin = claude_bin(d)?;
    let listed = crate::outside::lister::ask(&bin, &["agents", "--json"], wait).await?;
    let mut all = crate::outside::claude::parse(&listed)?;
    let listed = all.clone();
    let (carried, was, mentioned) = d
        .ledger
        .call_read(move |c| {
            let carried = crate::outside::carried(c, &listed)?;
            let was = crate::outside::went_from(c, &listed, &carried)?;
            let mentioned: HashSet<String> = meta::get(c, MENTIONED)?
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_default();
            Ok((carried, was, mentioned))
        })
        .await?;
    for s in &mut all {
        let key = s.key();
        s.was = was.get(&key).cloned();
        s.run = carried.get(&key).filter(|_| s.was.is_none()).cloned();
        s.new = s.run.is_none() && s.was.is_none() && !mentioned.contains(&s.session_id);
    }
    if mark {
        let mut now: Vec<String> = mentioned.into_iter().collect();
        now.extend(all.iter().filter(|s| s.new).map(|s| s.session_id.clone()));
        now.sort();
        now.dedup();
        let text = serde_json::to_string(&now)?;
        d.ledger.call(move |c| meta::set(c, MENTIONED, &text)).await?;
    }
    Ok(all)
}

/// How one of an agent's own sessions is carried on here: the background
/// process stopped first, if any (`claude stop <id>`), what the run starts
/// with, and its tags: the one that says which session it carries on, and,
/// for one left open in its terminal, the ones that say so and which
/// process held it there, where known (`outside::left_open`,
/// `outside::left_open_pid`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carry {
    pub stop: Option<String>,
    pub argv: Vec<String>,
    pub tags: Vec<String>,
}

/// How `s` is carried on, by `bin` as `profile` resumes a conversation, or
/// why it is not: one a run carries already, or one at work in its
/// terminal. A background one (Claude's, with an attach id) is stopped
/// first, whatever it is doing: two processes on one conversation would both
/// write it. One in a terminal cannot be stopped from here, so it is carried
/// on only while idle as far as is known (`quiet` narrows that for the
/// ones hooks tell of), and that terminal keeps its copy: the list shows it
/// again should it go back to work there.
pub fn carry(s: &Outside, profile: &Profile, bin: &Path) -> std::result::Result<Carry, String> {
    let name = s.title();
    if let Some(run) = &s.run {
        return Err(format!("{name} is in Relay already, as run {run}"));
    }
    if s.in_a_terminal() && s.at_work() {
        return Err(if s.agent == crate::outside::claude::AGENT {
            format!("{name} is working in its terminal; /bg there to open it here, or wait for it to finish")
        } else {
            format!("{name} is working in its terminal; wait for it to finish or close it there")
        });
    }
    let argv = profile
        .resume_argv(bin.to_string_lossy().into_owned(), &s.session_id)
        .ok_or_else(|| format!("Relay does not know how {} resumes a conversation", s.agent))?;
    let mut tags = vec![format!("{}{}", crate::outside::tag(&s.agent), s.session_id)];
    if s.in_a_terminal() {
        tags.push(format!("{}{}", crate::outside::left_open(&s.agent), s.session_id));
        if let Some(pid) = s.pid {
            tags.push(format!(
                "{}{}:{pid}",
                crate::outside::left_open_pid(&s.agent),
                s.session_id
            ));
        }
    }
    Ok(Carry {
        stop: s.id.clone(),
        argv,
        tags,
    })
}

/// Whether one in a terminal, told of by hooks, has been quiet long enough
/// to carry on: heard from no sooner than `HEARD_QUIET` ago (a word may be
/// on its way: codex sends them in the background), its conversation's
/// file untouched for `WRITTEN_QUIET` (a working agent keeps writing it,
/// even where the word that said so was lost). How long ago each was, where
/// known.
pub fn quiet(name: &str, heard: Option<Duration>, written: Option<Duration>) -> std::result::Result<(), String> {
    if heard.is_some_and(|h| h < HEARD_QUIET) || written.is_some_and(|w| w < WRITTEN_QUIET) {
        return Err(format!("{name} just moved in its terminal; try again in a moment"));
    }
    Ok(())
}

/// The session `p` names, as it stands now, with the run that carries it:
/// Claude's asked of `claude`, others' as their hooks last told — and for
/// those, how long ago it was last heard from and its conversation's file
/// last written, where known.
async fn the_one(d: &Daemon, p: &ImportMoveParams) -> Result<(Outside, Option<Recent>)> {
    if p.agent == crate::outside::claude::AGENT {
        let s = sessions(d, false, LIST_NOW)
            .await?
            .into_iter()
            .find(|s| s.session_id == p.session_id)
            .ok_or_else(|| anyhow!("Claude has no session {} now", p.session_id))?;
        return Ok((s, None));
    }
    let last = d
        .outside
        .told_of(&p.agent, &p.session_id)
        .ok_or_else(|| anyhow!("{} has no session {} now", p.agent, p.session_id))?;
    let mut s = last.session;
    let listed = vec![s.clone()];
    let carried = d.ledger.call_read(move |c| crate::outside::carried(c, &listed)).await?;
    s.run = carried.get(&s.key()).cloned();
    let written = last
        .transcript
        .and_then(|f| std::fs::metadata(f).ok()?.modified().ok())
        .map(|t| t.elapsed().unwrap_or_default());
    let recent = Recent {
        heard: last.heard.elapsed(),
        written,
    };
    Ok((s, Some(recent)))
}

/// How recently a session told of by hooks was heard from, and its
/// conversation's file written.
struct Recent {
    heard: Duration,
    written: Option<Duration>,
}

/// Carries one session on as a Relay run (`carry`): stopped first where it
/// runs in its agent's background, its folder registered, its conversation
/// resumed there. One ask at a time a session: a second, while the first is
/// on its way, is turned away (`Outsiders::moving`).
pub async fn move_one(d: &Daemon, p: ImportMoveParams) -> Result<Run> {
    let _held = d
        .outside
        .moving(&format!("o:{}:{}", p.agent, p.session_id))
        .ok_or_else(|| anyhow!("{} is being carried on already", p.session_id))?;
    let (s, recent) = the_one(d, &p).await?;
    let profile = d
        .registry
        .profile(&s.agent)
        .ok_or_else(|| anyhow!("there is no {} profile", s.agent))?;
    let bin =
        rl_detect::registry::find_bin(&profile.bins).ok_or_else(|| anyhow!("`{}` is not on this machine", s.agent))?;
    let carry = carry(&s, profile, &bin).map_err(|e| anyhow!(e))?;
    if let Some(r) = recent {
        quiet(&s.title(), Some(r.heard), r.written).map_err(|e| anyhow!(e))?;
    }
    if let Some(id) = &carry.stop {
        ask(&bin, &["stop", id]).await?;
    }
    // The folder's checkout, a worktree its own; else the folder itself.
    let root = crate::git::checkout(&s.cwd)
        .map(|c| c.path)
        .unwrap_or_else(|| s.cwd.clone());
    if let Err(e) = crate::workspace::up(d, &root).await {
        tracing::warn!(error = %e, root = %root.display(), "import: registering the folder");
    }
    d.supervisor
        .start(RunSpec {
            argv: carry.argv,
            kind: RunKind::Agent,
            cwd: s.cwd.clone(),
            title: Some(s.title()),
            ref_: None,
            tags: carry.tags,
            tty: true,
            cols: p.cols,
            rows: p.rows,
            inherit_env: p.env.is_empty(),
            env: p.env,
            input: true,
            cause: Cause::Manual,
            chain: None,
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_detect::Registry;

    fn session(agent: &str, id: Option<&str>, status: &str) -> Outside {
        Outside {
            agent: agent.into(),
            id: id.map(Into::into),
            kind: if id.is_some() { "background" } else { "interactive" }.into(),
            status: status.into(),
            state: id.map(|_| "working".into()),
            ..crate::outside::claude::parse(
                r#"[{"cwd":"/w","kind":"interactive","sessionId":"sid-1","name":"fix it"}]"#,
            )
            .unwrap()
            .remove(0)
        }
    }

    fn carried(s: &Outside) -> std::result::Result<Carry, String> {
        let reg = Registry::builtin();
        carry(s, reg.profile(&s.agent).unwrap(), Path::new("/bin/agent"))
    }

    #[test]
    fn a_background_session_is_stopped_then_resumed_even_at_work() {
        let c = carried(&session("claude", Some("a1"), "busy")).unwrap();
        assert_eq!(c.stop.as_deref(), Some("a1"));
        assert_eq!(c.argv, ["/bin/agent", "--resume", "sid-1"]);
        assert_eq!(c.tags, ["claude-session:sid-1"], "nothing left open: it was stopped");
    }

    #[test]
    fn an_idle_one_in_a_terminal_carries_on_as_its_profile_resumes() {
        let c = carried(&session("claude", None, "idle")).unwrap();
        assert_eq!(c.stop, None);
        assert_eq!(c.argv, ["/bin/agent", "--resume", "sid-1"]);
        let c = carried(&session("codex", None, "idle")).unwrap();
        assert_eq!(c.argv, ["/bin/agent", "resume", "sid-1"]);
        assert_eq!(c.tags, ["codex-session:sid-1", "codex-left-open:sid-1"]);
        let c = carried(&session("gemini", None, "idle")).unwrap();
        assert_eq!(c.argv, ["/bin/agent", "--resume", "sid-1"]);
        assert_eq!(c.tags, ["gemini-session:sid-1", "gemini-left-open:sid-1"]);
        // Its process, where known, is the terminal's: the run says which.
        let held = Outside {
            pid: Some(8),
            ..session("claude", None, "idle")
        };
        assert_eq!(
            carried(&held).unwrap().tags,
            [
                "claude-session:sid-1",
                "claude-left-open:sid-1",
                "claude-left-open-pid:sid-1:8"
            ]
        );
    }

    #[test]
    fn one_at_work_in_its_terminal_is_refused_in_its_agents_words() {
        for status in ["busy", "waiting"] {
            let e = carried(&session("claude", None, status)).unwrap_err();
            assert_eq!(
                e,
                "fix it is working in its terminal; /bg there to open it here, or wait for it to finish"
            );
            let e = carried(&session("codex", None, status)).unwrap_err();
            assert_eq!(
                e,
                "fix it is working in its terminal; wait for it to finish or close it there"
            );
        }
    }

    #[test]
    fn one_a_run_carries_already_is_refused() {
        let s = Outside {
            run: Some("r1".into()),
            name: String::new(),
            session_id: "0123456789".into(),
            ..session("codex", None, "idle")
        };
        assert_eq!(
            carried(&s).unwrap_err(),
            "session 01234567 is in Relay already, as run r1"
        );
    }

    #[test]
    fn an_agent_that_cannot_resume_says_so() {
        let reg = Registry::builtin();
        let mut p = reg.profile("codex").unwrap().clone();
        p.resume.clear();
        let e = carry(&session("codex", None, "idle"), &p, Path::new("/bin/agent")).unwrap_err();
        assert!(e.contains("does not know how codex resumes"), "{e}");
    }

    #[test]
    fn one_told_of_by_hooks_waits_for_a_quiet_moment() {
        let secs = Duration::from_secs;
        let just = "fix it just moved in its terminal; try again in a moment";
        assert_eq!(quiet("fix it", Some(secs(1)), None).unwrap_err(), just);
        assert_eq!(quiet("fix it", Some(secs(60)), Some(secs(2))).unwrap_err(), just);
        assert!(quiet("fix it", Some(secs(4)), Some(secs(6))).is_ok());
        assert!(
            quiet("fix it", Some(secs(4)), None).is_ok(),
            "no file named: the word alone"
        );
    }
}
