//! `rl flow new`: a new flow's folder written, then handed to the edit path for its yes — or, with
//! no editor between, written and said yes to by the one writer every path shares (`write_new`).

use super::edit::{Edited, Editing, TEMPLATE, finish_edit, folder_files, take_back, write_new_file};
use super::templates;
use crate::client::Client;
use crate::context::Want;
use crate::daemon;
use crate::interpret::Answer;
use anyhow::{Context as _, Result, anyhow, bail};
use rl_core::{Home, display_path};
use rl_flow::def::valid_name;
use rl_proto::flow::{Flow, FlowReload};
use rl_proto::methods::WorkspaceList;
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

/// A name a new flow may take: checked once (`check_name`), before anything slow is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewName(String);

impl NewName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NewName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `name` may be a new flow: a flow's name, and no `flow.md` by it yet — not even a link.
pub fn check_name(home: &Home, name: &str) -> Result<NewName> {
    if !valid_name(name) {
        bail!("`{name}` is not a flow name; use lowercase letters, digits and dashes");
    }
    if std::fs::symlink_metadata(home.flow_dir(name).join("flow.md")).is_ok() {
        bail!("`{name}` is a flow already; rl flow edit {name} opens it");
    }
    Ok(NewName(name.to_string()))
}

/// Why `text`, as the flow `name`'s `flow.md`, would not load — each at its line, as a reload
/// says it — given `known`, the projects Relay knows.
pub fn problems(name: &str, text: &str, known: &[PathBuf]) -> Vec<String> {
    let is_known = |p: &Path| {
        let p = crate::tui::form::expand(&p.to_string_lossy());
        known.contains(&std::fs::canonicalize(&p).unwrap_or(p))
    };
    match rl_flow::doc::read(text) {
        Err(p) => vec![at(p.line, &p.message)],
        Ok(doc) => match rl_flow::def::check(name, doc, &is_known) {
            Ok(_) => Vec::new(),
            Err(found) => found.iter().map(|p| at(p.line, &p.message)).collect(),
        },
    }
}

/// A problem at its line of `flow.md`, when it has one.
fn at(line: Option<usize>, message: &str) -> String {
    match line {
        Some(l) => format!("flow.md:{l} · {message}"),
        None => message.to_string(),
    }
}

/// The projects Relay knows, as a flow's `in:` is checked against them.
pub async fn projects(rpc: &mut Client) -> Result<Vec<PathBuf>> {
    Ok(rpc
        .call::<WorkspaceList>(())
        .await?
        .into_iter()
        .map(|w| w.path)
        .collect())
}

/// A new flow's folder written — `files`, `flow.md` among them, none there before — and its
/// `flow.md` ready for an editor. `yours`: what was written is the person's (a template they
/// filled, a draft they asked for), kept and said yes to even saved unchanged; else it is the
/// bare template, kept only once changed. Each file is written only where there is none; one that
/// cannot be leaves nothing of the others.
pub fn start_new(home: &Home, name: &NewName, files: Vec<NewFile>, yours: bool) -> Result<Editing> {
    if !files.iter().any(|f| f.path == Path::new("flow.md")) {
        bail!("a new flow needs a flow.md");
    }
    let dir = home.flow_dir(name.as_str());
    if let Some(f) = files
        .iter()
        .find(|f| std::fs::symlink_metadata(dir.join(&f.path)).is_ok())
    {
        bail!(
            "{} is there already: a new flow writes only what is not",
            display_path(&dir.join(&f.path))
        );
    }
    let fresh = folder_files(&dir)?.is_empty();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut editing = Editing {
        name: name.to_string(),
        file: dir.join("flow.md"),
        before: Vec::new(),
        was: Vec::new(),
        fresh,
        wrote: Vec::new(),
        yours,
    };
    for f in files {
        if let Err(e) = write_new_file(&dir.join(&f.path), &f.bytes, f.exec) {
            take_back(home, &editing)?;
            return Err(e);
        }
        editing.wrote.push((f.path, f.bytes));
    }
    editing.before = folder_files(&dir)?;
    editing.was = std::fs::read(&editing.file)?;
    Ok(editing)
}

/// A new flow written and said yes to with no editor between: `rl flow new … --go`, `rl interpret
/// --go`, and the panel's enter on a draft. Refused before anything is written when its
/// `flow.md` would not load (`known`: the projects Relay knows), saying the first problem at its
/// line, or when a file is there already. Written but not said yes to — it did not load, it needs
/// a yes, or the daemon did not answer — exactly what was written is taken away again, its folder
/// too if that leaves it empty, and why is the error.
pub async fn write_new(
    rpc: &mut Client,
    home: &Home,
    name: &NewName,
    files: Vec<NewFile>,
    known: &[PathBuf],
) -> Result<Box<Flow>> {
    let md = files
        .iter()
        .find(|f| f.path == Path::new("flow.md"))
        .ok_or_else(|| anyhow!("a new flow needs a flow.md"))?;
    let text = std::str::from_utf8(&md.bytes).map_err(|_| anyhow!("{name}'s flow.md is not UTF-8 text"))?;
    if let Some(first) = problems(name.as_str(), text, known).first() {
        bail!("{name} would not load: {first}");
    }
    let editing = start_new(home, name, files, true)?;
    let written = editing.clone();
    let why = match finish_edit(rpc, editing).await {
        Ok((Edited::Approved(f), _)) => return Ok(f),
        Ok((Edited::NeedsYes(why), _)) => format!("{name} was not said yes to: {why}"),
        Ok((edited, _)) => edited.words(name.as_str()),
        Err(e) => format!("write {name}: {e:#}"),
    };
    let left = take_back(home, &written);
    // The daemon forgets it too, when it is up.
    let _ = rpc.call::<FlowReload>(()).await;
    match left {
        Ok(()) => bail!("{why} · nothing kept"),
        Err(e) => bail!("{why} · and what was written is left: {e:#}"),
    }
}

/// `rl flow new`: the bare template, a template filled, or a draft from words — written to a new
/// folder, opened in `$EDITOR` and said yes to as an edit is, when it loads; or with `--go`
/// written and said yes to at once.
pub async fn new(home: &Home, a: NewArgs, json: bool) -> Result<()> {
    let folder = match &a.folder {
        Some(f) => f.clone(),
        None => std::env::current_dir()?,
    };
    let folder = std::fs::canonicalize(&folder).unwrap_or(folder);
    let name = check_name(home, &a.name)?;
    let (files, yours) = match &a.from {
        Some(t) => (template_files(t, &a.set, &folder)?, true),
        None if !a.words.is_empty() => (
            drafted(home, &name, &folder, &a.words.join(" "), a.go, json).await?,
            true,
        ),
        None if a.go => bail!("--go writes a template or a draft as it is; the bare template needs $EDITOR"),
        None => (vec![NewFile::flow_md(TEMPLATE)], false),
    };
    if a.go {
        let mut rpc = daemon::ensure(home, !json).await?;
        let known = projects(&mut rpc).await?;
        let f = write_new(&mut rpc, home, &name, files, &known).await?;
        return super::said(&f, "approved", json);
    }
    super::in_editor(home, start_new(home, &name, files, yours)?, json).await
}

/// The flow `name` drafted from `words` by `assistants.interpret`: its `flow.md`, the draft's own
/// name passed over for the person's.
async fn drafted(
    home: &Home,
    name: &NewName,
    folder: &Path,
    words: &str,
    go: bool,
    json: bool,
) -> Result<Vec<NewFile>> {
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

#[cfg(test)]
mod tests {
    use super::super::{drop_new, start_edit};
    use super::*;

    #[test]
    fn a_new_flow_is_written_only_where_nothing_is() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("h"));
        let text = "---\nsteps:\n  a:\n    run: echo\n---\n";
        // A name an assistant gave is a flow's name, or nothing is written.
        for name in ["../../escaped", "/tmp/abs", "a/b", ".."] {
            let e = check_name(&home, name).unwrap_err().to_string();
            assert!(e.contains("is not a flow name"), "{name}: {e}");
        }
        // Nor over a flow there already.
        let mine = home.flow_dir("mine");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::write(mine.join("flow.md"), "mine\n").unwrap();
        let e = check_name(&home, "mine").unwrap_err().to_string();
        assert!(e.contains("is a flow already"), "{e}");

        // A flow.md that is a link to nowhere: refused, and what it points at is not made.
        let linked = home.flow_dir("linked");
        std::fs::create_dir_all(&linked).unwrap();
        let target = tmp.path().join("outside.md");
        std::os::unix::fs::symlink(&target, linked.join("flow.md")).unwrap();
        assert!(check_name(&home, "linked").is_err(), "said up front");
        let name = NewName("linked".into());
        let e = start_new(&home, &name, vec![NewFile::flow_md(text)], true)
            .err()
            .expect("refused")
            .to_string();
        assert!(e.contains("is there already"), "{e}");
        assert!(!target.exists(), "never written through");

        // A file that cannot be written stops the rest: nothing of the new flow is left.
        let half = home.flow_dir("half");
        std::fs::create_dir_all(&half).unwrap();
        std::fs::write(half.join("notes.txt"), "mine").unwrap();
        let files = vec![
            NewFile::flow_md(text),
            NewFile {
                path: "missing/probe.sh".into(),
                bytes: b"echo".to_vec(),
                exec: true,
            },
        ];
        assert!(start_new(&home, &NewName("half".into()), files, true).is_err());
        assert!(!half.join("flow.md").exists(), "what was written went again");
        assert_eq!(std::fs::read_to_string(half.join("notes.txt")).unwrap(), "mine");
    }

    #[test]
    fn a_template_left_untouched_goes_and_only_what_was_written() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("h"));
        // A folder holding a script of the person's, and no flow.md yet.
        let dir = home.flow_dir("probe");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("probe.sh"), "echo mine\n").unwrap();
        let name = check_name(&home, "probe").unwrap();
        let editing = start_new(&home, &name, vec![NewFile::flow_md(TEMPLATE)], false).unwrap();
        // Back from an editor that left it as it was: the template goes, the script stays.
        assert!(drop_new(&home, &editing, false).unwrap());
        assert!(!dir.join("flow.md").exists());
        assert_eq!(std::fs::read_to_string(dir.join("probe.sh")).unwrap(), "echo mine\n");

        // So too for `rl flow edit` of a new name.
        let editing = start_edit(&home, "probe").unwrap();
        assert!(drop_new(&home, &editing, false).unwrap());
        assert!(!dir.join("flow.md").exists() && dir.join("probe.sh").exists());

        // Saved changed, it is the person's: kept.
        let editing = start_edit(&home, "probe").unwrap();
        std::fs::write(&editing.file, "---\nsteps: {}\n---\nmine\n").unwrap();
        assert!(!drop_new(&home, &editing, true).unwrap());
        assert!(dir.join("flow.md").exists());

        // A fresh folder left as the template goes whole.
        let name = check_name(&home, "pulse").unwrap();
        let editing = start_new(&home, &name, vec![NewFile::flow_md(TEMPLATE)], false).unwrap();
        assert!(drop_new(&home, &editing, false).unwrap());
        assert!(!home.flow_dir("pulse").exists());
    }
}
