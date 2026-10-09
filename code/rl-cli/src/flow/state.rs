//! `rl flow state set|get|write|read`: what a step leaves for the steps after it, and reads of
//! what theirs left. Only inside a step, and only through `rld`, which finds the step from
//! `$RELAY_RUN` — so one allow rule, `Bash(rl flow state:*)`, gives an agent its firing's state
//! and nothing else.

use crate::daemon;
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use rl_core::Home;
use rl_flow::state::FILE_MAX;
use rl_proto::flow::{FlowStateGet, FlowStateRead, FlowStateSet, FlowStateWrite, StateFile, StateName, StateValues};
use std::io::{Read, Write};

#[derive(Subcommand)]
pub enum StateCmd {
    /// Set values for the steps after this one: `rl flow state set signature=abc found=3`.
    Set {
        #[arg(required = true, value_name = "KEY=VALUE")]
        values: Vec<String>,
    },
    /// Print what a step left: `probe.signature`, `review.choice`, `probe.out`, or this step's own `key`.
    Get { name: String },
    /// Write stdin to a file in the firing's folder, for the steps after this one: `rl flow state write report.md < x`.
    Write { name: String },
    /// Print a file from the firing's folder, or what a step printed (`probe.out`).
    Read { name: String },
}

/// The run of the step this is called in, from what every step starts with; none outside one.
fn here() -> Result<String> {
    let set = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    match (set("RELAY_RUN"), set("RELAY_FIRING"), set("RELAY_STEP")) {
        (Some(run), Some(_), Some(_)) => Ok(run),
        _ => bail!("not in a flow step"),
    }
}

pub async fn state(home: &Home, cmd: StateCmd, json: bool) -> Result<()> {
    let run = here()?;
    // Nothing is said of rld — not that it started, nor that it is another release — as what
    // this prints is the step's out.
    let mut rpc = daemon::ensure_quiet(home).await?;
    match cmd {
        StateCmd::Set { values } => {
            let values = values
                .iter()
                .map(|kv| {
                    kv.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .with_context(|| format!("`{kv}` is not key=value"))
                })
                .collect::<Result<Vec<_>>>()?;
            rpc.call::<FlowStateSet>(StateValues { run, values }).await?;
        }
        StateCmd::Get { name } => {
            let value = rpc
                .call::<FlowStateGet>(StateName {
                    run,
                    name: name.clone(),
                })
                .await?;
            if json {
                println!("{}", serde_json::json!({ "value": value }));
                return Ok(());
            }
            match value {
                Some(v) => println!("{v}"),
                None => bail!("`{name}` is not set"),
            }
        }
        StateCmd::Write { name } => {
            let mut bytes = Vec::new();
            std::io::stdin().take(FILE_MAX as u64 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > FILE_MAX {
                bail!("`{name}` is over 10 MB: a file is 10 MB at most");
            }
            rpc.call::<FlowStateWrite>(StateFile { run, name, bytes }).await?;
        }
        StateCmd::Read { name } => {
            let bytes = rpc.call::<FlowStateRead>(StateName { run, name }).await?;
            std::io::stdout().write_all(&bytes)?;
        }
    }
    Ok(())
}
