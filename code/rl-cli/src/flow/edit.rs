//! A flow's `flow.md` in an editor, and the yes that writing it through `rl` is: given only when
//! what was saved loads, the folder had its yes before (or held nothing), and nothing else in it
//! changed meanwhile — to exactly that version, by its hash.

use super::new::problems;
use crate::client::Client;
use anyhow::{Context as _, Result, bail};
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
    /// What this edit wrote, as written: a new flow's files, and no one else's.
    pub(super) wrote: Vec<(PathBuf, Vec<u8>)>,
    /// What was written is the person's — a draft they asked for — so it
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
    let mut wrote = Vec::new();
    if !file.exists() {
        std::fs::create_dir_all(&dir)?;
        write_new_file(&file, TEMPLATE.as_bytes(), false)?;
        wrote.push((PathBuf::from("flow.md"), TEMPLATE.as_bytes().to_vec()));
    }
    Ok(Editing {
        name: name.to_string(),
        before: folder_files(&dir)?,
        was: std::fs::read(&file)?,
        file,
        fresh,
        wrote,
        yours: false,
    })
}

/// Back from the editor: every flow read again, and the one edited said yes to only when
/// `flow.md` was saved changed, the folder before it had its yes (or held nothing), and nothing
/// else in it changed meanwhile — the yes is to exactly that version, by its hash. A new flow the
/// person wrote — a draft — is said yes to saved unchanged as well. A new flow
/// left as the template is the caller's to take away first, before the daemon is reached, so it
/// goes even when the daemon cannot be (`drop_new`).
pub async fn finish_edit(rpc: &mut Client, e: Editing) -> Result<(Edited, Reloaded)> {
    let now = std::fs::read(&e.file).ok();
    let untouched = now.as_deref() == Some(e.was.as_slice());
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

/// A flow's `flow.md` changed to `text` — words typed on it, answered — and said yes to as an
/// edit is, with no editor between: only while the file is still `base`, what the change was
/// asked of, and only when `text` loads. What does not load, or cannot be said yes to for want
/// of the daemon, leaves `base` back in place; a yes the folder's state withholds (`NeedsYes`)
/// keeps the text, as `rl flow edit` does.
pub async fn write_changed(
    rpc: &mut Client,
    home: &Home,
    name: &str,
    base: &str,
    text: &str,
    known: &[PathBuf],
) -> Result<Edited> {
    as_asked(home, name, base)?;
    if let Some(first) = problems(name, text, known).first() {
        bail!("{name} would not load: {first}");
    }
    let editing = start_edit(home, name)?;
    let written = editing.clone();
    if let Err(e) = std::fs::write(&editing.file, text) {
        restore(&written, base)?;
        return Err(anyhow::Error::new(e).context(format!("writing {}", editing.file.display())));
    }
    match finish_edit(rpc, editing).await {
        Ok((Edited::NotLoaded(said), _)) => {
            restore(&written, base)?;
            // The daemon reads it back as it was, when it is up.
            let _ = rpc.call::<FlowReload>(()).await;
            Ok(Edited::NotLoaded(said))
        }
        Ok((edited, _)) => Ok(edited),
        Err(e) => {
            restore(&written, base)?;
            // The daemon may have read the new text before it failed: it reads the old back.
            let _ = rpc.call::<FlowReload>(()).await;
            Err(e)
        }
    }
}

/// A flow's `flow.md` changed to `text`, ready for an editor — `^e` on a change proposed: the
/// person reads it there, and saving it is their yes (`finish_edit`). Only while the file is
/// still `base`, and a write that fails puts `base` back. A caller whose editor does not open,
/// fails, or leaves `text` as it was, writes `base` back (`restore`): no yes was given to the
/// change.
pub fn start_changed(home: &Home, name: &str, base: &str, text: &str) -> Result<Editing> {
    as_asked(home, name, base)?;
    let mut editing = start_edit(home, name)?;
    if let Err(e) = std::fs::write(&editing.file, text) {
        // Half written is no flow: what it was asked of goes back.
        restore(&editing, base)?;
        return Err(anyhow::Error::new(e).context(format!("writing {}", editing.file.display())));
    }
    // `was` stays the old text: what the editor saves is a change to it.
    editing.yours = true;
    Ok(editing)
}

/// A changed `flow.md` put back as it was asked of: `base`.
pub fn restore(e: &Editing, base: &str) -> Result<()> {
    std::fs::write(&e.file, base).with_context(|| format!("writing {} back", e.file.display()))
}

/// A flow by a flow's name, its `flow.md` still `base`: what a change was asked of is what it
/// would be written over.
fn as_asked(home: &Home, name: &str, base: &str) -> Result<()> {
    if !valid_name(name) {
        bail!("`{name}` is not a flow name; use lowercase letters, digits and dashes");
    }
    let now = std::fs::read_to_string(home.flow_dir(name).join("flow.md")).ok();
    if now.as_deref() != Some(base) {
        bail!("{name}'s flow.md changed since it was asked · type to ask again");
    }
    Ok(())
}

/// Takes a new flow away as it was written, when nothing since is the person's: this edit wrote
/// its `flow.md`, and it is untouched. Back from the editor (`unseen` false) only the bare
/// template goes — a draft is the person's; when the editor did not open or
/// failed (`unseen`), whatever was written goes, for no yes was given to it. Only what the edit
/// wrote goes (`take_back`): a file the person had there, or what a firing left in `runs/`,
/// stays, and so does the folder then. Whether it went.
pub fn drop_new(home: &Home, e: &Editing, unseen: bool) -> Result<bool> {
    let wrote_md = e.wrote.iter().any(|(p, _)| p == Path::new("flow.md"));
    let untouched = std::fs::read(&e.file).is_ok_and(|now| now == e.was);
    if !(wrote_md && (unseen || !e.yours) && untouched) {
        return Ok(false);
    }
    take_back(home, e)?;
    Ok(true)
}

/// What an edit wrote, removed — each file only while it is as written — and then its folder,
/// only if nothing else is in it.
pub(super) fn take_back(home: &Home, e: &Editing) -> Result<()> {
    let dir = home.flow_dir(&e.name);
    for (p, bytes) in &e.wrote {
        let file = dir.join(p);
        if std::fs::read(&file).is_ok_and(|now| &now == bytes) {
            std::fs::remove_file(&file).with_context(|| format!("removing {}", file.display()))?;
        }
    }
    // Not empty: something not the edit's is there, and stays.
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

/// A file written where there is none — never over a file, nor through a link, dangling or not —
/// and made executable when a step runs it.
pub(super) fn write_new_file(path: &Path, bytes: &[u8], exec: bool) -> Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    let written = f.write_all(bytes).and_then(|()| {
        #[cfg(unix)]
        if exec {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    });
    // The file is ours from the open on: a half-written one is not left behind.
    if written.is_err() {
        let _ = std::fs::remove_file(path);
    }
    written.with_context(|| format!("writing {}", path.display()))
}

/// A flow's folder as its hash reads it; an empty one when there is none.
pub(super) fn folder_files(dir: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    match rl_core::folder::read(dir) {
        Ok(f) => Ok(f.files),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(anyhow::Error::new(e).context(format!("reading {}", dir.display()))),
    }
}
