//! Bringing Claude's own sessions into Relay. `claude --bg` sessions (and
//! ones open in a terminal) are Claude's to supervise; moving one stops
//! Claude's background process — the conversation stays in its transcript —
//! and carries it on with `claude --resume` as an ordinary Relay run, in the
//! same folder, which becomes a workspace. Only `claude agents --json` and
//! `claude stop` are asked: no model is called.

use crate::server::Daemon;
use anyhow::{Context, Result, anyhow, bail};
use rl_ledger::{meta, runs};
use rl_proto::methods::{ClaudeSession, ImportListParams, ImportMoveParams, ImportReport, RunFilter, RunSpec};
use rl_proto::types::{Cause, Run, RunKind};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

/// Tagged on the run that carries a session on: `claude-session:<id>`.
pub const TAG: &str = "claude-session:";
/// The `meta` key: sessions the panel has mentioned at start already.
const MENTIONED: &str = "import.mentioned";
/// `claude agents` and `claude stop` get this long.
const ASK_TIMEOUT: Duration = Duration::from_secs(20);

/// A line of `claude agents --json`, as Claude writes it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listed {
    session_id: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    cwd: PathBuf,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

fn parse(text: &str) -> Result<Vec<ClaudeSession>> {
    let listed: Vec<Listed> = serde_json::from_str(text).context("reading `claude agents --json`")?;
    Ok(listed
        .into_iter()
        .map(|l| {
            let name = l
                .name
                .filter(|n| !n.trim().is_empty() && !n.starts_with("<<"))
                .unwrap_or_else(|| {
                    l.cwd
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_else(|| l.session_id.clone())
                });
            ClaudeSession {
                session_id: l.session_id,
                id: l.id,
                name,
                cwd: l.cwd,
                kind: l.kind.unwrap_or_else(|| "background".into()),
                status: l.status.unwrap_or_default(),
                state: l.state,
                run: None,
                new: false,
            }
        })
        .collect())
}

fn claude_bin(d: &Daemon) -> Result<PathBuf> {
    let p = d
        .registry
        .profile("claude")
        .ok_or_else(|| anyhow!("no claude profile"))?;
    rl_detect::registry::find_bin(&p.bins).ok_or_else(|| anyhow!("`claude` is not on this machine"))
}

async fn ask(bin: &PathBuf, args: &[&str]) -> Result<String> {
    let out = tokio::time::timeout(
        ASK_TIMEOUT,
        tokio::process::Command::new(bin)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow!("`claude {}` did not answer in time", args.join(" ")))?
    .with_context(|| format!("running `claude {}`", args.join(" ")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "`claude {}` failed: {}",
            args.join(" "),
            err.lines().next().unwrap_or("no reason given")
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Claude's sessions now, each with the run that carries it on, if any, and
/// whether the panel has mentioned it before.
pub async fn list(d: &Daemon, p: ImportListParams) -> ImportReport {
    match sessions(d, p.mark).await {
        Ok(sessions) => ImportReport { sessions, error: None },
        Err(e) => ImportReport {
            sessions: vec![],
            error: Some(format!("{e:#}")),
        },
    }
}

async fn sessions(d: &Daemon, mark: bool) -> Result<Vec<ClaudeSession>> {
    let bin = claude_bin(d)?;
    let mut all = parse(&ask(&bin, &["agents", "--json"]).await?)?;
    let ids: Vec<String> = all.iter().map(|s| s.session_id.clone()).collect();
    let (carried, mentioned) = d
        .ledger
        .call(move |c| {
            let mut carried = Vec::new();
            for id in &ids {
                let found = runs::list(
                    c,
                    &RunFilter {
                        tag: Some(format!("{TAG}{id}")),
                        archived: true,
                        limit: Some(1),
                        ..Default::default()
                    },
                )?;
                carried.push(found.into_iter().next().map(|r| r.id));
            }
            let mentioned: HashSet<String> = meta::get(c, MENTIONED)?
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_default();
            Ok((carried, mentioned))
        })
        .await?;
    for (s, run) in all.iter_mut().zip(carried) {
        s.new = run.is_none() && !mentioned.contains(&s.session_id);
        s.run = run;
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

/// Moves one session: stops Claude's background process, registers the
/// folder, and resumes the conversation as a Relay run.
pub async fn move_one(d: &Daemon, p: ImportMoveParams) -> Result<Run> {
    let bin = claude_bin(d)?;
    let all = sessions(d, false).await?;
    let s = all
        .into_iter()
        .find(|s| s.session_id == p.session_id)
        .ok_or_else(|| anyhow!("Claude has no session {} now", p.session_id))?;
    if let Some(run) = &s.run {
        bail!("{} is in Relay already, as run {run}", s.name);
    }
    let Some(id) = &s.id else {
        bail!("{} is open in a terminal; close it there, then import it", s.name);
    };
    // Stopped first: two processes on one conversation would both write it.
    ask(&bin, &["stop", id]).await?;
    // The folder's checkout, a worktree its own; else the folder itself.
    let root = crate::git::checkout(&s.cwd)
        .map(|c| c.path)
        .unwrap_or_else(|| s.cwd.clone());
    match d.scheduler.up(&root, false).await {
        Err(e) => tracing::warn!(error = %e, root = %root.display(), "import: registering the folder"),
        Ok(up) => {
            let _ = d.reactor.up(&up.workspace.path, &up.workspace.name, false).await;
        }
    }
    d.supervisor
        .start(RunSpec {
            argv: vec![
                bin.to_string_lossy().into_owned(),
                "--resume".into(),
                s.session_id.clone(),
            ],
            kind: RunKind::Agent,
            cwd: s.cwd.clone(),
            title: Some(s.name.clone()),
            ref_: None,
            tags: vec![format!("{TAG}{}", s.session_id)],
            tty: true,
            cols: None,
            rows: None,
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

    #[test]
    fn claude_agents_json_reads_as_sessions() {
        let text = r#"[
          {"pid":1,"id":"8af077cd","cwd":"/w/app","kind":"background","startedAt":1789789820657,
           "sessionId":"8af077cd-5fe3","name":"asset list","status":"idle","state":"done"},
          {"pid":2,"cwd":"/w/relay","kind":"interactive","startedAt":1789821153884,
           "sessionId":"42fdfb2d","name":"ground work","status":"busy"},
          {"pid":3,"id":"a8b1e728","cwd":"/w/market","kind":"background",
           "sessionId":"0d06b838","name":"<<placeholder>>","status":"busy","state":"working"}
        ]"#;
        let s = parse(text).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(
            (s[0].id.as_deref(), s[0].name.as_str()),
            (Some("8af077cd"), "asset list")
        );
        assert!(!s[0].in_a_terminal() && !s[0].working());
        assert!(s[1].in_a_terminal() && s[1].working());
        assert_eq!(s[2].name, "market", "a placeholder name gives way to the folder's");
        assert!(s[2].working());
    }
}
