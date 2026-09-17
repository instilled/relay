//! A job Relay hands to an agent: an ordinary run of `assistants.<job>`,
//! headless, with the prompt as its last argument, waited for, its output
//! handed back. It is a run like any other — in the list, in the ledger,
//! stoppable — with `cause: assistant:<job>` and the tag `relay:assistant`.

use crate::server::Daemon;
use anyhow::{Result, bail};
use rl_core::time::parse_duration;
use rl_ledger::{runs, session_io};
use rl_proto::methods::{AssistantOutput, AssistantParams, RunSpec};
use rl_proto::types::{Cause, RunKind};
use std::time::{Duration, Instant};

/// The tag every assistant run carries.
pub const TAG: &str = "relay:assistant";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// The most output kept for the answer.
const OUTPUT_MAX: usize = 1 << 20;

pub async fn run(d: &Daemon, p: AssistantParams) -> Result<AssistantOutput> {
    let named = {
        let cfg = d.config.read().unwrap();
        match cfg.config.assistants.for_job(&p.job) {
            Some(a) => a.cloned(),
            None => bail!(
                "`{}` is not a job Relay hands to an assistant ({})",
                p.job,
                rl_proto::config::Assistants::JOBS.join(", ")
            ),
        }
    };
    let agents = crate::config::agents(&d.registry);
    let resolved = match crate::config::resolve(&p.job, named.as_ref(), &agents) {
        Ok(Some(r)) => r,
        Ok(None) => bail!("no agent on this machine to ask; `rl agents` says which Relay knows"),
        Err(e) => bail!("{e}"),
    };
    if !resolved.available {
        bail!(
            "assistants.{}: `{}` is not on this machine; `rl agents`",
            p.job,
            resolved.agent
        );
    }
    let timeout = match &p.timeout {
        Some(t) => parse_duration(t)
            .ok_or_else(|| anyhow::anyhow!("`{t}` is not a duration"))?
            .unsigned_abs(),
        None => DEFAULT_TIMEOUT,
    };
    let mut argv = resolved.argv.clone();
    argv.push(p.prompt);
    let spec = RunSpec {
        argv,
        kind: RunKind::Agent,
        cwd: p.cwd,
        title: Some(p.title),
        ref_: None,
        tags: vec![TAG.into()],
        tty: false,
        cols: None,
        rows: None,
        env: vec![],
        inherit_env: true,
        // `claude -p` waits for input it is never given, then warns into the
        // answer; an assistant run has none.
        input: false,
        cause: Cause::Assistant(p.job),
        flow: None,
    };
    let run = d.supervisor.start(spec).await?;
    let id = run.id.clone();
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let ended = loop {
        let found = d
            .ledger
            .call({
                let id = id.clone();
                move |c| runs::get(c, &id)
            })
            .await?;
        match found {
            Some(r) if r.state.is_terminal() => break r,
            Some(_) if Instant::now() > deadline => {
                timed_out = true;
                let _ = d.supervisor.stop(&id, true).await;
                // The holder reports the exit; give it a moment, then read what there is.
                tokio::time::sleep(Duration::from_millis(300)).await;
                break d
                    .ledger
                    .call({
                        let id = id.clone();
                        move |c| runs::get(c, &id)
                    })
                    .await?
                    .unwrap_or(run.clone());
            }
            Some(_) => tokio::time::sleep(Duration::from_millis(150)).await,
            None => bail!("run {id} vanished"),
        }
    };
    let bytes = d
        .ledger
        .call({
            let id = id.clone();
            move |c| session_io::tail(c, &id, OUTPUT_MAX)
        })
        .await?;
    Ok(AssistantOutput {
        run: id,
        agent: resolved.agent,
        output: String::from_utf8_lossy(&bytes).into_owned(),
        exit: ended.exit_code,
        timed_out,
    })
}
