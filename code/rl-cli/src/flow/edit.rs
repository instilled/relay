//! A flow's `flow.md` in an editor, and the yes that writing it through `rl` is: given only when
//! what was saved loads, the folder had its yes before (or held nothing), and nothing else in it
//! changed meanwhile — to exactly that version, by its hash.

use crate::client::Client;
use anyhow::{Result, bail};
use rl_core::Home;
use rl_flow::def::valid_name;
use rl_flow::hash;
use rl_proto::flow::{Flow, FlowApproval, FlowApprove, FlowFilter, FlowList, FlowReload, NotLoaded, Reloaded};
use std::path::{Path, PathBuf};

/// What `rl flow edit` starts a new flow from.
pub const TEMPLATE: &str = "---\nwhen: every 1h\nsteps:\n  first:\n    run: echo hello\n---\n";

/// A flow's `flow.md` about to be edited, and its folder as it was: what a yes after the edit
/// may cover, and no more.
#[derive(Clone)]
pub struct Editing {
    pub name: String,
    pub file: PathBuf,
    /// The folder before the editor opened, a new flow's files in it.
    pub(super) before: Vec<(PathBuf, Vec<u8>)>,
    /// `flow.md` before the editor opened.
    pub(super) was: Vec<u8>,
    /// The folder held nothing before this edit wrote to it.
    pub(super) fresh: bool,
    /// What was written is the person's — a template they filled, a draft they asked for — so it
    /// is kept and said yes to even saved unchanged. The bare template is not.
    pub(super) yours: bool,
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
            Edited::Approved(_) => format!("{name} approved"),
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
        yours: false,
    })
}

/// Back from the editor: every flow read again, and the one edited said yes to only when
/// `flow.md` was saved changed, the folder before it had its yes (or held nothing), and nothing
/// else in it changed meanwhile — the yes is to exactly that version, by its hash. A new flow
/// left as the template is not kept. A new flow the person wrote — a filled template, a draft —
/// is said yes to saved unchanged as well.
pub async fn finish_edit(rpc: &mut Client, home: &Home, e: Editing) -> Result<(Edited, Reloaded)> {
    let now = std::fs::read(&e.file).ok();
    let untouched = now.as_deref() == Some(e.was.as_slice());
    if drop_new(home, &e, false)? {
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
    let Some(now) = now.filter(|_| e.yours || !untouched) else {
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

/// Takes a new flow away as it was written, when nothing since is the person's: its folder held
/// nothing before, `flow.md` is untouched and the folder is as written. Back from the editor
/// (`unseen` false) only the bare template goes — a filled template or a draft is the person's;
/// when the editor did not open or failed (`unseen`), whatever was written goes, for no yes was
/// given to it. What a firing left in `runs/`, or anything the hash passes over, stays, and so
/// does the folder then. Whether it went.
pub fn drop_new(home: &Home, e: &Editing, unseen: bool) -> Result<bool> {
    let dir = home.flow_dir(&e.name);
    let untouched = std::fs::read(&e.file).is_ok_and(|now| now == e.was);
    if !(e.fresh && (unseen || !e.yours) && untouched && folder_files(&dir)? == e.before) {
        return Ok(false);
    }
    for (p, _) in &e.before {
        std::fs::remove_file(dir.join(p))?;
    }
    // Not empty: something not the edit's is there, and stays.
    let _ = std::fs::remove_dir(&dir);
    Ok(true)
}

/// A flow's folder as its hash reads it; an empty one when there is none.
pub(super) fn folder_files(dir: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    match rl_core::folder::read(dir) {
        Ok(f) => Ok(f.files),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(anyhow::Error::new(e).context(format!("reading {}", dir.display()))),
    }
}
