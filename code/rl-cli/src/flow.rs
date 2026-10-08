//! `rl flow`: the flows in `~/.relay/flows` and what they are doing; edit
//! one (and say yes to it, if it loads), run, pause, resume, approve, reload.

use crate::client::Client;
use crate::{daemon, render};
use anyhow::{Result, bail};
use clap::Subcommand;
use jiff::Timestamp;
use rl_core::time::{fmt_age, now};
use rl_core::{Home, display_path};
use rl_flow::def::{FlowDef, valid_name};
use rl_flow::hash;
use rl_proto::flow::{
    Firing, FiringFilter, Flow, FlowApproval, FlowApprove, FlowErrors, FlowFilter, FlowFirings, FlowList, FlowName,
    FlowPause, FlowReload, FlowResume, FlowRun, NotLoaded, Reloaded,
};
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum FlowCmd {
    /// List flows: when they start, what the last firing did, and what did not load.
    #[command(alias = "ls")]
    List {
        /// Include flows whose folder is gone.
        #[arg(short, long)]
        all: bool,
    },
    /// Open a flow's flow.md in $VISUAL / $EDITOR (a new one starts from a template); saved, and if it loads, it is approved.
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
    /// Say yes to a flow's folder as it was last loaded; it may fire from then on.
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
            let f = approve(&mut rpc, &name).await?;
            if json {
                println!("{}", serde_json::to_string(&f)?);
            } else {
                println!("approved {} · {}", f.name, short(&f.hash));
            }
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

/// Says yes to `name` as the daemon has it now, by its hash: the version this shows.
async fn approve(rpc: &mut Client, name: &str) -> Result<Flow> {
    let flows = rpc.call::<FlowList>(FlowFilter { all: false }).await?;
    let hash = flows
        .into_iter()
        .find(|f| f.name == name)
        .map(|f| f.hash)
        .unwrap_or_default();
    rpc.call::<FlowApprove>(FlowApproval {
        name: name.into(),
        hash,
    })
    .await
}

/// A hash as it is shown: its first eight.
fn short(hash: &str) -> String {
    hash.chars().take(8).collect()
}

/// Opens the flow's `flow.md`, then reads the folder again and says what came of it; one that
/// did not load, or was saved and is left needing a yes, exits 1.
async fn edit(home: &Home, name: &str, json: bool) -> Result<()> {
    let editing = start_edit(home, name)?;
    crate::env::open_in_editor(&editing.file, 1)?;
    let mut rpc = daemon::ensure(home, !json).await?;
    let (edited, reloaded) = finish_edit(&mut rpc, home, editing).await?;
    if let Edited::Approved(f) = &edited {
        return said(f, "approved", json);
    }
    if json {
        println!("{}", serde_json::to_string(&reloaded)?);
    }
    let failed = matches!(edited, Edited::NotLoaded(_) | Edited::NeedsYes(_));
    if !json {
        match &edited {
            Edited::NotLoaded(said) => {
                for n in said {
                    eprintln!("not loaded: {n}");
                }
            }
            Edited::NeedsYes(_) => eprintln!("{}", edited.words(name)),
            _ => println!("{}", edited.words(name)),
        }
    }
    if failed {
        std::process::exit(1)
    }
    Ok(())
}

/// A flow's `flow.md` about to be edited, and its folder as it was: what a yes after the edit
/// may cover, and no more.
pub struct Editing {
    pub name: String,
    pub file: PathBuf,
    /// The folder before the editor opened, a new flow's template in it.
    before: Vec<(PathBuf, Vec<u8>)>,
    /// `flow.md` before the editor opened.
    was: Vec<u8>,
    /// The folder held nothing before this edit wrote the template.
    fresh: bool,
}

/// What came of an edit.
pub enum Edited {
    /// Saved, loaded, and approved.
    Approved(Box<Flow>),
    /// It did not load, and why.
    NotLoaded(Vec<NotLoaded>),
    /// A new flow left as the template: its folder is gone again.
    Dropped,
    /// `flow.md` as it was: no yes given, and whether it needs one.
    Unchanged { needs_yes: bool },
    /// Saved and loaded, but left needing a yes: why.
    NeedsYes(&'static str),
}

impl Edited {
    /// The line that says it, for the CLI and the panel alike.
    pub fn words(&self, name: &str) -> String {
        match self {
            Edited::Approved(_) => format!("approved {name}"),
            Edited::NotLoaded(said) => match said.first() {
                Some(n) => format!("not loaded: {n}"),
                None => format!("{name} did not load"),
            },
            Edited::Dropped => format!("{name} left as the template: not kept"),
            Edited::Unchanged { needs_yes: false } => "flow.md unchanged: no yes given".into(),
            Edited::Unchanged { needs_yes: true } => {
                format!("flow.md unchanged: no yes given · {name} needs one: rl flow approve {name}")
            }
            Edited::NeedsYes(why) => format!("{name} needs a yes: {why}; look, then rl flow approve {name}"),
        }
    }
}

/// A flow's `flow.md`, for an editor, and its folder as it is: a new one starts from a template.
pub fn start_edit(home: &Home, name: &str) -> Result<Editing> {
    if !valid_name(name) {
        bail!("`{name}` is not a flow name; use lowercase letters, digits and dashes");
    }
    let dir = home.flow_dir(name);
    let file = dir.join("flow.md");
    let fresh = folder_files(&dir)?.is_empty();
    if !file.exists() {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&file, TEMPLATE)?;
    }
    Ok(Editing {
        name: name.to_string(),
        before: folder_files(&dir)?,
        was: std::fs::read(&file)?,
        file,
        fresh,
    })
}

/// Back from the editor: every flow read again, and the one edited said yes to only when
/// `flow.md` was saved changed, the folder before it had its yes (or held nothing), and nothing
/// else in it changed meanwhile — the yes is to exactly that version, by its hash. A new flow
/// left as the template is not kept.
pub async fn finish_edit(rpc: &mut Client, home: &Home, e: Editing) -> Result<(Edited, Reloaded)> {
    let dir = home.flow_dir(&e.name);
    let now = std::fs::read(&e.file).ok();
    let untouched = now.as_deref() == Some(e.was.as_slice());
    if untouched && e.fresh && now.as_deref() == Some(TEMPLATE.as_bytes()) && folder_files(&dir)? == e.before {
        std::fs::remove_dir_all(&dir)?;
        return Ok((Edited::Dropped, rpc.call::<FlowReload>(()).await?));
    }
    let reloaded = rpc.call::<FlowReload>(()).await?;
    let mine: Vec<NotLoaded> = reloaded
        .not_loaded
        .iter()
        .filter(|n| n.flow == e.name)
        .cloned()
        .collect();
    if !mine.is_empty() {
        return Ok((Edited::NotLoaded(mine), reloaded));
    }
    let flows = rpc.call::<FlowList>(FlowFilter { all: false }).await?;
    let Some(flow) = flows.into_iter().find(|f| f.name == e.name) else {
        return Ok((Edited::NotLoaded(Vec::new()), reloaded));
    };
    let Some(now) = now.filter(|_| !untouched) else {
        let needs_yes = !flow.approved();
        return Ok((Edited::Unchanged { needs_yes }, reloaded));
    };
    let had_yes = e.fresh || flow.approved_hash.as_deref() == Some(hash::folder(&e.before).as_str());
    if !had_yes {
        return Ok((Edited::NeedsYes("its folder changed since its last yes"), reloaded));
    }
    let mut edited = e.before;
    edited.retain(|(p, _)| p != Path::new("flow.md"));
    edited.push((PathBuf::from("flow.md"), now));
    let hash = hash::folder(&edited);
    if flow.hash != hash {
        let why = "something else in its folder changed while flow.md was open";
        return Ok((Edited::NeedsYes(why), reloaded));
    }
    let f = rpc.call::<FlowApprove>(FlowApproval { name: e.name, hash }).await?;
    Ok((Edited::Approved(Box::new(f)), reloaded))
}

/// A flow's folder as its hash reads it; an empty one when there is none.
fn folder_files(dir: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    match rl_core::folder::read(dir) {
        Ok(f) => Ok(f.files),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(anyhow::Error::new(e).context(format!("reading {}", dir.display()))),
    }
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
