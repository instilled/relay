//! `rl flow`: the flows in `~/.relay/flows` and what they are doing; edit
//! one (and say yes to it, if it loads), run, pause, resume, approve, test, reload, park and bring
//! back; and, inside a step, its firing's state.

use crate::client::Client;
use crate::{daemon, render};
use anyhow::{Result, bail};
use clap::Subcommand;
use jiff::Timestamp;
use rl_core::time::{fmt_age, now};
use rl_core::{Home, display_path};
use rl_flow::def::FlowDef;
use rl_proto::flow::{
    Firing, FiringFilter, Flow, FlowApproval, FlowApprove, FlowErrors, FlowFilter, FlowFirings, FlowList, FlowName,
    FlowPark, FlowParkedList, FlowPause, FlowReload, FlowResume, FlowRun, FlowTest, FlowTestParams, FlowUnpark, Parked,
    ParkedName, Reloaded,
};

mod edit;
mod new;
mod state;
pub mod tested;
pub use edit::{Edited, Editing, TEMPLATE, drop_new, finish_edit, restore, start_changed, start_edit, write_changed};
pub use new::{NewArgs, NewFile, NewName, check_name, problems, projects, start_new, write_new};
pub use state::StateCmd;

#[derive(Subcommand)]
pub enum FlowCmd {
    /// List flows: when they start, what the last firing did, and what did not load.
    #[command(alias = "ls")]
    List {
        /// Include flows whose folder is gone.
        #[arg(short, long)]
        all: bool,
    },
    /// Write a new flow: from plain words (an assistant drafts it), or the bare template; then $EDITOR, and a yes when it loads.
    New(NewArgs),
    /// Open a flow's flow.md in $VISUAL / $EDITOR; saved, and if it loads, it is approved.
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
    /// Say yes to a flow's folder, read again now; it may fire from then on.
    Approve {
        name: String,
        /// The version looked at: its hash as `rl flow list` shows it, or 6 of it or more. Refused when
        /// the folder is another version now. Without it, the version read now, which is printed.
        hash: Option<String>,
    },
    /// What firing a flow would do now — what would fire it, which steps would run — starting nothing.
    Test {
        name: String,
        /// An event, by id (rl events): whether it would fire the flow, and what its steps would make of it.
        #[arg(long)]
        event: Option<i64>,
    },
    /// Read ~/.relay/flows again (rld does it on start too).
    Reload,
    /// Put a flow away: its folder renamed _<name>, its runs archived, its yes taken. Refused while it fires or waits on a question.
    Park { name: String },
    /// Bring a parked flow back under its name, needing a yes: by name, its newest parked folder; or a folder, `_nightly.2`.
    Unpark {
        #[arg(value_name = "NAME|FOLDER")]
        what: String,
    },
    /// The parked flows: folder, name, and when each was parked.
    Parked,
    /// What a step leaves for the steps after it, and reads of theirs: set, get, write, read. Inside a step only.
    State {
        #[command(subcommand)]
        cmd: StateCmd,
    },
}

const HEADERS: &[&str] = &["NAME", "HASH", "STATUS", "WHEN", "LAST", "IN"];

pub async fn flow(home: &Home, cmd: FlowCmd, json: bool) -> Result<()> {
    let cmd = match cmd {
        FlowCmd::State { cmd } => return state::state(home, cmd, json).await,
        FlowCmd::New(a) => return new::new(home, a, json).await,
        cmd => cmd,
    };
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
                    "no flows ({}); `rl flow new <name>` writes one",
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
        FlowCmd::Approve { name, hash } => {
            let f = approve(&mut rpc, &name, hash.as_deref()).await?;
            if json {
                println!("{}", serde_json::to_string(&f)?);
            } else {
                println!("approved {} · {}", f.name, short(&f.hash));
            }
        }
        FlowCmd::Test { name, event } => {
            let t = rpc.call::<FlowTest>(FlowTestParams { name, event }).await?;
            if json {
                println!("{}", serde_json::to_string(&t)?);
            } else {
                for l in tested::lines(&t, now()) {
                    println!("{l}");
                }
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
        FlowCmd::Park { name } => {
            let p = rpc.call::<FlowPark>(named(name)).await?;
            if json {
                println!("{}", serde_json::to_string(&p)?);
            } else {
                let runs = if p.archived == 1 { "run" } else { "runs" };
                println!("{} parked as {} · {} {runs} archived", p.name, p.folder, p.archived);
            }
        }
        FlowCmd::Unpark { what } => {
            let parked = if what.starts_with('_') {
                Vec::new()
            } else {
                rpc.call::<FlowParkedList>(()).await?
            };
            let Some(folder) = parked_folder(&parked, &what) else {
                bail!("nothing parked as {what} · rl flow parked lists them");
            };
            let u = rpc.call::<FlowUnpark>(ParkedName { folder }).await?;
            if json {
                println!("{}", serde_json::to_string(&u)?);
            } else if let Some(f) = &u.flow {
                println!("{} back · it needs a yes: rl flow approve {}", f.name, f.name);
            } else {
                let (name, why) = (&u.name, u.problem.as_deref().unwrap_or("not loaded"));
                bail!(
                    "{name} is back in flows/{name} but did not load: {why} · rl flow edit {name}, or rl flow reload"
                );
            }
        }
        FlowCmd::Parked => {
            let parked = rpc.call::<FlowParkedList>(()).await?;
            if json {
                println!("{}", serde_json::to_string(&parked)?);
            } else if parked.is_empty() {
                println!("nothing parked");
            } else {
                let now = now();
                for p in &parked {
                    let mut line = format!("{}  {}  {} ago", p.folder, p.name, fmt_age(p.at, now));
                    if let Some(i) = &p.in_ {
                        line.push_str(&format!("  in {}", display_path(i)));
                    }
                    println!("{line}");
                }
            }
        }
        FlowCmd::Edit { .. } | FlowCmd::New(_) | FlowCmd::State { .. } => unreachable!("handled above"),
    }
    Ok(())
}

/// The folder `unpark` brings back: one named as a folder (`_nightly.2`) is itself; a flow's
/// name is its newest parked folder in `parked`. None when nothing is parked under the name.
pub fn parked_folder(parked: &[Parked], what: &str) -> Option<String> {
    if what.starts_with('_') {
        return Some(what.to_string());
    }
    parked
        .iter()
        .filter(|p| p.name == what)
        .max_by_key(|p| p.at)
        .map(|p| p.folder.clone())
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

/// Says yes to `name` as a reload reads it now, by its hash. `shown`, the hash `rl flow list`
/// showed or a prefix of it, must be that version; without it, the version read now is the one
/// approved, and said.
async fn approve(rpc: &mut Client, name: &str, shown: Option<&str>) -> Result<Flow> {
    let shown = shown.map(str::to_ascii_lowercase);
    if let Some(shown) = &shown
        && shown.len() < MIN_SHOWN
    {
        bail!("give at least {MIN_SHOWN} of the hash rl flow list shows, not {shown}");
    }
    let shown = shown.as_deref();
    rpc.call::<FlowReload>(()).await?;
    let flows = rpc.call::<FlowList>(FlowFilter { all: false }).await?;
    let hash = flows
        .into_iter()
        .find(|f| f.name == name)
        .map(|f| f.hash)
        .unwrap_or_default();
    if let Some(shown) = shown
        && !hash.is_empty()
        && !hash.starts_with(shown)
    {
        bail!(
            "`{name}` is {} now, not {shown}: look at it again (rl flow list)",
            short(&hash)
        );
    }
    rpc.call::<FlowApprove>(FlowApproval {
        name: name.into(),
        hash,
    })
    .await
}

/// How much of a hash `rl flow approve` takes as naming a version: fewer could match another
/// by chance.
const MIN_SHOWN: usize = 6;

/// A hash as it is shown: its first eight.
fn short(hash: &str) -> String {
    hash.chars().take(8).collect()
}

/// Opens the flow's `flow.md`, then reads the folder again and says what came of it; one that
/// did not load, or was saved and is left needing a yes, exits 1.
async fn edit(home: &Home, name: &str, json: bool) -> Result<()> {
    in_editor(home, start_edit(home, name)?, json).await
}

/// `flow.md` in `$EDITOR`, then the edit finished and said. However the editor ends, a new flow
/// left as the template is not kept: taken away before the daemon is reached, so it goes even
/// when the daemon cannot be started.
async fn in_editor(home: &Home, editing: Editing, json: bool) -> Result<()> {
    if let Err(e) = crate::env::open_in_editor(&editing.file, 1) {
        if let Err(left) = drop_new(home, &editing, true) {
            bail!("{e:#}; and what was written is left: {left:#}");
        }
        return Err(e);
    }
    let name = editing.name.clone();
    if drop_new(home, &editing, false)? {
        let said = Edited::Dropped.words(&name);
        if json {
            println!("{}", serde_json::json!({ "name": name, "kept": false, "said": said }));
        } else {
            println!("{said}");
        }
        return Ok(());
    }
    let mut rpc = daemon::ensure(home, !json).await?;
    let (edited, reloaded) = finish_edit(&mut rpc, editing).await?;
    told(&edited, &reloaded, &name, json)
}

/// What came of an edit or a new flow, said; one that did not load, or was saved and is left
/// needing a yes, exits 1.
pub(crate) fn told(edited: &Edited, reloaded: &Reloaded, name: &str, json: bool) -> Result<()> {
    if let Edited::Approved(f) = edited {
        return said(f, "approved", json);
    }
    if json {
        println!("{}", serde_json::to_string(reloaded)?);
    } else {
        match edited {
            Edited::NotLoaded(said) => {
                for n in said {
                    eprintln!("not loaded: {n}");
                }
            }
            Edited::NeedsYes(_) => eprintln!("{}", edited.words(name)),
            _ => println!("{}", edited.words(name)),
        }
    }
    if matches!(edited, Edited::NotLoaded(_) | Edited::NeedsYes(_)) {
        std::process::exit(1)
    }
    Ok(())
}

/// `NAME HASH STATUS WHEN LAST IN` for one flow, and its latest firing if any.
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
    vec![
        f.name.clone(),
        short(&f.hash),
        f.status().to_string(),
        f.when(),
        last,
        in_,
    ]
}
