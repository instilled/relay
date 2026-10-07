//! Picking ended runs back up — most often the ones a restart took, which
//! reconcile marks `run.lost` when Relay comes back without their holders.
//!
//! Each is carried on as a new run in its folder, under its title and in its
//! chain: an agent's conversation where its CLI can resume one (`claude
//! --resume <session>`, `codex resume <session>`), the session named exactly
//! by the transcript a hook reported, else the one found by folder and start;
//! a shell as a fresh shell; a command run again. The run picked up is
//! archived, so the list shows the one that carries it on, where it stood.

use crate::server::Daemon;
use crate::transcript::{self, Agent};
use anyhow::Result;
use jiff::Timestamp;
use rl_ledger::{order, runs};
use rl_proto::methods::{Resumed, RunResumeParams, RunSpec};
use rl_proto::types::{Cause, Run, RunKind};

/// The tag a run carries naming the run it picked up.
pub const RESUMES: &str = "resumes:";

/// Picks up the runs asked about — or every lost one — each on its own: one
/// that cannot be resumed says why and the rest go on.
pub async fn resume(d: &Daemon, p: RunResumeParams) -> Result<Vec<Resumed>> {
    let ids = if p.lost {
        d.ledger.call(|c| runs::lost(c)).await?
    } else {
        p.ids.clone()
    };
    let mut out = Vec::new();
    for id in ids {
        let run = d
            .ledger
            .call({
                let id = id.clone();
                move |c| runs::get(c, &id)
            })
            .await?;
        let Some(run) = run else {
            out.push(not(&id, &id, "no such run"));
            continue;
        };
        if p.lost && run.kind == RunKind::Command && !p.commands {
            out.push(not(&run.id, &run.title, rl_proto::methods::RESUME_LEFT_OUT));
            continue;
        }
        out.push(one(d, &run, &p).await);
    }
    Ok(out)
}

async fn one(d: &Daemon, run: &Run, p: &RunResumeParams) -> Resumed {
    let session = match Agent::of(run) {
        Some(agent) if run.kind == RunKind::Agent => match crate::summary::locate(d, run).await {
            Ok(file) => file.and_then(|f| transcript::session_id(agent, &f)),
            Err(e) => return not(&run.id, &run.title, &format!("finding its conversation: {e:#}")),
        },
        _ => None,
    };
    let argv = match argv_for(run, session.as_deref()) {
        Ok(a) => a,
        Err(why) => return not(&run.id, &run.title, &why),
    };
    let spec = RunSpec {
        argv,
        kind: run.kind,
        cwd: run.cwd.clone(),
        title: Some(run.title.clone()),
        ref_: run.ref_.clone(),
        tags: tags_for(run),
        tty: true,
        cols: p.cols,
        rows: p.rows,
        env: p.env.clone(),
        inherit_env: p.env.is_empty(),
        input: true,
        cause: Cause::Manual,
        chain: Some(run.chain.clone()),
    };
    match d.supervisor.start(spec).await {
        Ok(new) => {
            let old = run.id.clone();
            let to = format!("r:{}", new.id);
            let now = Timestamp::now();
            let archived = d.ledger.call(move |c| {
                order::pass_on(c, &format!("r:{old}"), &to)?;
                runs::archive(c, &[old], now)
            });
            if let Err(e) = archived.await {
                tracing::warn!(error = %e, id = %run.id, "archiving a resumed run");
            }
            Resumed {
                from: run.id.clone(),
                title: run.title.clone(),
                run: Some(new),
                why_not: None,
            }
        }
        Err(e) => not(&run.id, &run.title, &format!("{e:#}")),
    }
}

fn not(id: &str, title: &str, why: &str) -> Resumed {
    Resumed {
        from: id.to_string(),
        title: title.to_string(),
        run: None,
        why_not: Some(why.to_string()),
    }
}

/// What picking `run` back up runs, or why nothing can. `session` is its
/// agent's conversation, where one was found.
pub fn argv_for(run: &Run, session: Option<&str>) -> Result<Vec<String>, String> {
    if !run.state.is_terminal() {
        return Err("it is still going".into());
    }
    let Some(bin) = run.argv.first().cloned() else {
        return Err("it has no command line to start again".into());
    };
    match run.kind {
        RunKind::Agent => {
            let agent = Agent::of(run).ok_or_else(|| {
                format!(
                    "Relay does not know how {} resumes a conversation",
                    run.cli.as_deref().unwrap_or("this agent")
                )
            })?;
            let session = session.ok_or("its agent left no conversation to resume")?;
            Ok(match agent {
                Agent::Claude => vec![bin, "--resume".into(), session.into()],
                Agent::Codex => vec![bin, "resume".into(), session.into()],
            })
        }
        // A shell has nothing to resume but its place; a command runs again.
        RunKind::Shell | RunKind::Command => Ok(run.argv.clone()),
        RunKind::Plugin => Err("a plugin's run is not picked back up".into()),
    }
}

/// Its tags, naming the run it picks up (and only that one: a run resumed
/// twice names the last).
fn tags_for(run: &Run) -> Vec<String> {
    let mut tags: Vec<String> = run.tags.iter().filter(|t| !t.starts_with(RESUMES)).cloned().collect();
    tags.push(format!("{RESUMES}{}", run.id));
    tags
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::types::RunState;
    use std::path::PathBuf;

    fn run(kind: RunKind, cli: Option<&str>, argv: &[&str], state: RunState) -> Run {
        let now: Timestamp = "2026-09-23T10:00:00Z".parse().unwrap();
        let mut r = Run::new(
            "ab12",
            kind,
            "t",
            argv.iter().map(|s| s.to_string()).collect(),
            PathBuf::from("/w"),
            PathBuf::from("/w"),
            now,
        );
        r.cli = cli.map(Into::into);
        r.state = state;
        r
    }

    #[test]
    fn claude_is_resumed_by_its_session_without_the_hook_arguments() {
        // The stored argv carries the hook settings `start` splices in; those
        // come back by themselves, so only the binary is kept.
        let r = run(
            RunKind::Agent,
            Some("claude"),
            &["claude", "--settings", "/h/hooks.json", "fix it"],
            RunState::Failed,
        );
        assert_eq!(argv_for(&r, Some("8af0")).unwrap(), ["claude", "--resume", "8af0"]);
    }

    #[test]
    fn codex_is_resumed_by_its_session() {
        let r = run(RunKind::Agent, Some("codex"), &["codex"], RunState::Failed);
        assert_eq!(argv_for(&r, Some("01a0")).unwrap(), ["codex", "resume", "01a0"]);
    }

    #[test]
    fn an_agent_without_a_conversation_says_so() {
        let r = run(RunKind::Agent, Some("claude"), &["claude"], RunState::Failed);
        assert!(argv_for(&r, None).unwrap_err().contains("no conversation"));
        let other = run(RunKind::Agent, Some("aider"), &["aider"], RunState::Failed);
        assert!(argv_for(&other, Some("x")).unwrap_err().contains("aider"));
    }

    #[test]
    fn a_shell_or_a_command_starts_again_as_it_was() {
        let sh = run(RunKind::Shell, None, &["/bin/zsh", "-i"], RunState::Failed);
        assert_eq!(argv_for(&sh, None).unwrap(), ["/bin/zsh", "-i"]);
        let cmd = run(RunKind::Command, None, &["npm", "run", "dev"], RunState::Failed);
        assert_eq!(argv_for(&cmd, None).unwrap(), ["npm", "run", "dev"]);
    }

    #[test]
    fn a_run_still_going_is_left_alone() {
        let r = run(RunKind::Shell, None, &["/bin/zsh", "-i"], RunState::Idle);
        assert!(argv_for(&r, None).unwrap_err().contains("still going"));
    }

    #[test]
    fn the_new_run_names_the_one_it_picks_up_and_only_that_one() {
        let mut r = run(RunKind::Shell, None, &["/bin/zsh"], RunState::Failed);
        r.tags = vec!["shell".into(), "resumes:old1".into()];
        assert_eq!(tags_for(&r), ["shell", "resumes:ab12"]);
    }
}
