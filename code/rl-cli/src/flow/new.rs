//! `rl flow new`: a new flow's folder written, then handed to the edit path for its yes.

use super::edit::{Editing, TEMPLATE, drop_new, finish_edit, folder_files};
use super::templates;
use crate::context::Want;
use crate::daemon;
use crate::interpret::Answer;
use anyhow::{Context as _, Result, bail};
use rl_core::{Home, display_path};
use rl_flow::def::valid_name;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct NewArgs {
    /// The new flow's folder in ~/.relay/flows: lowercase letters, digits and dashes.
    pub name: String,
    /// What it should do, in plain words: assistants.interpret drafts its flow.md.
    #[arg(conflicts_with = "from")]
    pub words: Vec<String>,
    /// Start from a template Relay ships: prod-logs, morning-brief.
    #[arg(long, value_name = "TEMPLATE")]
    pub from: Option<String>,
    /// A template's blank: --set project=my-prod. Asked for when missing, on a terminal.
    #[arg(long = "set", value_name = "KEY=VALUE", requires = "from")]
    pub set: Vec<String>,
    /// The project the flow works in (a template's `in`); default here.
    #[arg(long = "in", value_name = "FOLDER")]
    pub folder: Option<PathBuf>,
    /// Write it and say yes to it without opening $EDITOR.
    #[arg(long)]
    pub go: bool,
}

/// A file a new flow starts with: its path in the folder, its bytes, and whether a step runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub exec: bool,
}

impl NewFile {
    /// The flow's `flow.md`.
    pub fn flow_md(text: impl Into<String>) -> Self {
        NewFile {
            path: PathBuf::from("flow.md"),
            bytes: text.into().into_bytes(),
            exec: false,
        }
    }
}

/// Whether `name` may be a new flow: a flow's name, and no flow by it yet.
pub fn check_name(home: &Home, name: &str) -> Result<()> {
    if !valid_name(name) {
        bail!("`{name}` is not a flow name; use lowercase letters, digits and dashes");
    }
    if home.flow_dir(name).join("flow.md").exists() {
        bail!("`{name}` is a flow already; rl flow edit {name} opens it");
    }
    Ok(())
}

/// A new flow's folder written — `files`, `flow.md` among them, none there before — and its
/// `flow.md` ready for an editor. `yours`: what was written is the person's (a template they
/// filled, a draft they asked for), kept and said yes to even saved unchanged; else it is the
/// bare template, kept only once changed.
pub fn start_new(home: &Home, name: &str, files: Vec<NewFile>, yours: bool) -> Result<Editing> {
    check_name(home, name)?;
    if !files.iter().any(|f| f.path == Path::new("flow.md")) {
        bail!("a new flow needs a flow.md");
    }
    let dir = home.flow_dir(name);
    if let Some(f) = files.iter().find(|f| dir.join(&f.path).exists()) {
        bail!(
            "{} is there already: a new flow writes only what is not",
            display_path(&dir.join(&f.path))
        );
    }
    let fresh = folder_files(&dir)?.is_empty();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    for f in &files {
        write(&dir.join(&f.path), &f.bytes, f.exec)?;
    }
    let file = dir.join("flow.md");
    Ok(Editing {
        name: name.to_string(),
        before: folder_files(&dir)?,
        was: std::fs::read(&file)?,
        file,
        fresh,
        yours,
    })
}

/// A file written, and made executable when a step runs it.
fn write(path: &Path, bytes: &[u8], exec: bool) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    if exec {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// `rl flow new`: the bare template, a template filled, or a draft from words — written to a new
/// folder, opened in `$EDITOR` unless `--go`, and said yes to as an edit is, when it loads.
pub async fn new(home: &Home, a: NewArgs, json: bool) -> Result<()> {
    let folder = match &a.folder {
        Some(f) => f.clone(),
        None => std::env::current_dir()?,
    };
    let folder = std::fs::canonicalize(&folder).unwrap_or(folder);
    check_name(home, &a.name)?;
    let (files, yours) = match &a.from {
        Some(t) => (template_files(t, &a.set, &folder)?, true),
        None if !a.words.is_empty() => (
            drafted(home, &a.name, &folder, &a.words.join(" "), a.go, json).await?,
            true,
        ),
        None if a.go => bail!("--go writes a template or a draft as it is; the bare template needs $EDITOR"),
        None => (vec![NewFile::flow_md(TEMPLATE)], false),
    };
    let editing = start_new(home, &a.name, files, yours)?;
    if !a.go
        && let Err(e) = crate::env::open_in_editor(&editing.file, 1)
    {
        if let Err(left) = drop_new(home, &editing, true) {
            bail!("{e:#}; and what was written is left: {left:#}");
        }
        return Err(e);
    }
    let mut rpc = daemon::ensure(home, !json).await?;
    let (edited, reloaded) = finish_edit(&mut rpc, home, editing).await?;
    super::told(&edited, &reloaded, &a.name, json)
}

/// The flow `name` drafted from `words` by `assistants.interpret`: its `flow.md`, the draft's own
/// name passed over for the person's.
async fn drafted(home: &Home, name: &str, folder: &Path, words: &str, go: bool, json: bool) -> Result<Vec<NewFile>> {
    daemon::ensure(home, !json).await?;
    if !json {
        eprintln!("asking…");
    }
    let want = Want::Flow { name: name.to_string() };
    let (answer, out) = crate::interpret::ask(home, folder, words, &want).await?;
    let Answer::Flow(d) = answer else {
        bail!("asked for a flow; {} answered a run (run {})", out.agent, out.run);
    };
    if !json {
        eprintln!("# drafted by {} · run {}", out.agent, out.run);
        // No editor will show it: what is said yes to is shown first.
        if go {
            eprint!("{}", d.text);
        }
    }
    Ok(vec![NewFile::flow_md(d.text)])
}

/// A template's files, its blanks from `--set`, `in` from the folder, and — on a terminal — the
/// rest asked for; off one, what is missing is said.
fn template_files(name: &str, set: &[String], folder: &Path) -> Result<Vec<NewFile>> {
    let t = templates::find(name).map_err(anyhow::Error::msg)?;
    let mut given = templates::given_in(t, set, &display_path(folder)).map_err(anyhow::Error::msg)?;
    let missing = templates::missing(t, &given);
    if !missing.is_empty() && !std::io::stdin().is_terminal() {
        bail!("{}", templates::needs(t, &missing, "--set "));
    }
    for b in missing {
        eprint!("{} (like {}): ", b.what, b.like);
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        given.insert(b.key.to_string(), line.trim().to_string());
    }
    templates::fill(t, &given).map_err(anyhow::Error::msg)
}
