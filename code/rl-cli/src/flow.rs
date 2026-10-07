//! `rl flow`: the flows in `~/.relay/flows` and what they are doing; edit
//! one (and say yes to it, if it loads), run, pause, resume, approve, reload.

use crate::{daemon, render};
use anyhow::{Result, bail};
use clap::Subcommand;
use jiff::Timestamp;
use rl_core::time::{fmt_age, now};
use rl_core::{Home, display_path};
use rl_flow::def::{FlowDef, valid_name};
use rl_proto::flow::{
    Firing, FiringFilter, Flow, FlowApprove, FlowErrors, FlowFilter, FlowFirings, FlowList, FlowName, FlowPause,
    FlowReload, FlowResume, FlowRun, NotLoaded,
};

#[derive(Subcommand)]
pub enum FlowCmd {
    /// List flows: when they start, what the last firing did, and what did not load.
    #[command(alias = "ls")]
    List {
        /// Include flows whose folder is gone.
        #[arg(short, long)]
        all: bool,
    },
    /// Open a flow's flow.md in $VISUAL / $EDITOR (a new one starts from a template); if it loads, it is approved.
    Edit {
        /// The flow's folder in ~/.relay/flows: lowercase letters, digits and dashes.
        name: String,
    },
    /// Fire a flow now, by hand, even if it is paused; it needs a yes.
    Run { name: String },
    /// Stop a flow from firing by itself until resumed.
    Pause { name: String },
    /// Let a paused flow fire again.
    Resume { name: String },
    /// Say yes to a flow's folder as it is now; it may fire from then on.
    Approve { name: String },
    /// Read ~/.relay/flows again (rld does it on start too).
    Reload,
}

const HEADERS: &[&str] = &["NAME", "STATUS", "WHEN", "LAST", "IN"];

/// What `rl flow edit` starts a new flow from.
const TEMPLATE: &str = "---\nwhen: every 1h\nsteps:\n  first:\n    run: echo hello\n---\n";

pub async fn flow(home: &Home, cmd: FlowCmd, json: bool) -> Result<()> {
    if let FlowCmd::Edit { name } = &cmd {
        return edit(home, name, json).await;
    }
    let mut rpc = daemon::ensure(home, !json).await?;
    let named = |name: String| FlowName { name };
    match cmd {
        FlowCmd::List { all } => {
            let flows = rpc.call::<FlowList>(FlowFilter { all }).await?;
            let not_loaded = rpc.call::<FlowErrors>(()).await?;
            let mut last = Vec::with_capacity(flows.len());
            for f in &flows {
                let filter = FiringFilter {
                    flow: f.name.clone(),
                    limit: Some(1),
                };
                last.push(rpc.call::<FlowFirings>(filter).await?.into_iter().next());
            }
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "flows": flows, "firings": last, "not_loaded": not_loaded })
                );
                return Ok(());
            }
            if flows.is_empty() {
                println!(
                    "no flows ({}); `rl flow edit <name>` writes one",
                    display_path(&home.flows_dir())
                );
            } else {
                let now = now();
                let rows: Vec<Vec<String>> = flows.iter().zip(&last).map(|(f, l)| row(f, l.as_ref(), now)).collect();
                print!("{}", render::table(HEADERS, &rows));
            }
            if !not_loaded.is_empty() {
                println!("\nnot loaded:");
                for n in &not_loaded {
                    println!("  {n}");
                }
            }
        }
        FlowCmd::Run { name } => {
            let f = rpc.call::<FlowRun>(named(name)).await?;
            if json {
                println!("{}", serde_json::to_string(&f)?);
            } else {
                println!("fired {} · firing {}", f.flow, f.id);
            }
        }
        FlowCmd::Pause { name } => {
            let f = rpc.call::<FlowPause>(named(name)).await?;
            said(&f, "paused", json)?;
        }
        FlowCmd::Resume { name } => {
            let f = rpc.call::<FlowResume>(named(name)).await?;
            said(&f, "resumed", json)?;
        }
        FlowCmd::Approve { name } => {
            let f = rpc.call::<FlowApprove>(named(name)).await?;
            said(&f, "approved", json)?;
        }
        FlowCmd::Reload => {
            let r = rpc.call::<FlowReload>(()).await?;
            if json {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                println!(
                    "{} loaded · {} added · {} changed · {} removed",
                    r.loaded,
                    r.added.len(),
                    r.changed.len(),
                    r.removed.len()
                );
                for n in &r.not_loaded {
                    eprintln!("not loaded: {n}");
                }
            }
        }
        FlowCmd::Edit { .. } => unreachable!("handled above"),
    }
    Ok(())
}

/// One line for a flow that was changed, or the flow itself as JSON.
fn said(f: &Flow, what: &str, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(f)?);
    } else {
        println!("{what} {}", f.name);
    }
    Ok(())
}

/// Opens the flow's `flow.md`, then reads the folder again. A flow that
/// loaded gets a yes, since the one who edited it is the one saying it; one
/// that did not is said why, and the exit is 1.
async fn edit(home: &Home, name: &str, json: bool) -> Result<()> {
    let file = file_to_edit(home, name)?;
    crate::env::open_in_editor(&file, 1)?;
    let mut rpc = daemon::ensure(home, !json).await?;
    let reloaded = rpc.call::<FlowReload>(()).await?;
    let mine: Vec<&NotLoaded> = reloaded.not_loaded.iter().filter(|n| n.flow == name).collect();
    if mine.is_empty() {
        let f = rpc.call::<FlowApprove>(FlowName { name: name.into() }).await?;
        return said(&f, "approved", json);
    }
    if json {
        println!("{}", serde_json::to_string(&reloaded)?);
    } else {
        for n in mine {
            eprintln!("not loaded: {n}");
        }
    }
    std::process::exit(1)
}

/// A flow's `flow.md`, for an editor: a new one starts from a template.
pub fn file_to_edit(home: &Home, name: &str) -> Result<std::path::PathBuf> {
    if !valid_name(name) {
        bail!("`{name}` is not a flow name; use lowercase letters, digits and dashes");
    }
    let file = home.flow_dir(name).join("flow.md");
    if !file.exists() {
        std::fs::create_dir_all(home.flow_dir(name))?;
        std::fs::write(&file, TEMPLATE)?;
    }
    Ok(file)
}

/// `NAME STATUS WHEN LAST IN` for one flow, and its latest firing if any.
fn row(f: &Flow, last: Option<&Firing>, now: Timestamp) -> Vec<String> {
    let last = match last {
        Some(x) => format!("{} {} ago", x.status, fmt_age(x.started_at, now)),
        None => "never".to_string(),
    };
    let in_ = serde_json::from_value::<FlowDef>(f.def.clone())
        .ok()
        .and_then(|d| d.in_)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "—".to_string());
    vec![f.name.clone(), f.status().to_string(), f.when(), last, in_]
}
