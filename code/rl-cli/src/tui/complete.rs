//! What the line at the foot of the panel can be completed to.
//!
//! The line is rooted in the folder it will run in — the warm shell's own
//! folder when the line goes to that shell and it has said where it is, else
//! the folder of the row the cursor is on — so a path means that folder's,
//! not the one `rl` was started in. The first word is what to run — an
//! agent, one of the folder's make targets or scripts, a binary on your
//! PATH, a path. A few common tools know their own words (`git`'s
//! subcommands and branches, `make` and `just` targets, a package's scripts,
//! `cargo`'s subcommands); anywhere else, a file. A `/` line's arguments are
//! the words its command takes, and `/flow`'s second one a flow.

use super::{palette, query};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// `git`'s subcommands, the ones a person types most.
const GIT: &[&str] = &[
    "add",
    "bisect",
    "blame",
    "branch",
    "checkout",
    "cherry-pick",
    "clean",
    "clone",
    "commit",
    "config",
    "diff",
    "fetch",
    "grep",
    "init",
    "log",
    "merge",
    "mv",
    "pull",
    "push",
    "rebase",
    "reflog",
    "remote",
    "reset",
    "restore",
    "revert",
    "rm",
    "show",
    "stash",
    "status",
    "switch",
    "tag",
    "worktree",
];

/// The `git` subcommands whose next word is a branch.
const GIT_BRANCHED: &[&str] = &["checkout", "switch", "merge", "rebase", "diff", "log"];

/// `cargo`'s subcommands, the ones a person types most.
const CARGO: &[&str] = &[
    "add", "bench", "build", "check", "clean", "clippy", "doc", "fix", "fmt", "init", "install", "metadata", "new",
    "publish", "remove", "run", "search", "test", "tree", "update",
];

/// `npm`'s subcommands, the ones a person types most; `run` is the one with
/// words of its own after it.
const NPM: &[&str] = &[
    "ci", "exec", "install", "outdated", "publish", "run", "start", "test", "update",
];

/// How long `git` gets to list branches before completion goes on without.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Past this many, a list is no help; the best are kept.
const CAP: usize = 200;

/// What kind of thing a candidate is — decides how it is ordered and shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Agent,
    Target,
    Binary,
    Path,
    Word,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    pub kind: Kind,
}

/// Where the line is being typed.
pub struct Here<'a> {
    /// The folder the line will run in; paths, branches, targets and
    /// scripts complete against it (see `line_folder`).
    pub folder: &'a Path,
    /// The agents the registry knows, never a hardcoded list.
    pub agents: &'a [String],
    /// The directories of $PATH — given, so the tests can supply their own.
    pub path_dirs: &'a [PathBuf],
    /// The names the person's shell knows that PATH does not: aliases and
    /// functions, read from `$SHELL` once (see `shell_names`). A `ll` is here.
    pub aliases: &'a [String],
    /// What `/filter`'s tokens can name: projects, flows, branches; and the
    /// flows `/flow` acts on.
    pub names: Names<'a>,
}

/// The values a search token completes to, as `query::complete` takes them.
#[derive(Default, Clone, Copy)]
pub struct Names<'a> {
    pub projects: &'a [String],
    pub flows: &'a [String],
    pub branches: &'a [String],
    /// What `kind:` takes, when the view says (the events view: event kinds);
    /// empty, the list's own.
    pub kinds: &'a [String],
}

impl<'a> Here<'a> {
    /// A line typed in `folder`, knowing nothing else yet.
    pub fn at(folder: &'a Path) -> Self {
        Here {
            folder,
            agents: &[],
            path_dirs: &[],
            aliases: &[],
            names: Names::default(),
        }
    }
}

fn cand(text: impl Into<String>, kind: Kind) -> Candidate {
    Candidate {
        text: text.into(),
        kind,
    }
}

/// The words of `from` that start with the fragment, as `kind`.
fn starting<'a>(from: impl IntoIterator<Item = &'a str>, frag: &str, kind: Kind) -> Vec<Candidate> {
    from.into_iter()
        .filter(|w| w.starts_with(frag))
        .map(|w| cand(w, kind))
        .collect()
}

/// Where a line's paths are: the warm shell's own folder when the line is
/// headed there and the shell has said where it is (its prompt's OSC 7); the
/// project's folder when it is not — an agent starts in the project, and a
/// `/` command is the panel's — or when the shell has not said.
pub fn line_folder(line: &str, agents: &[String], project: &Path, shell_cwd: Option<&Path>) -> PathBuf {
    let line = line.trim_start();
    let head = line
        .strip_prefix('!')
        .unwrap_or(line)
        .split_whitespace()
        .next()
        .unwrap_or("");
    let to_shell = !line.starts_with('/') && !agents.iter().any(|a| a == head);
    match shell_cwd {
        Some(cwd) if to_shell => cwd.to_path_buf(),
        _ => project.to_path_buf(),
    }
}

/// Candidates for the word the cursor sits in, best first.
pub fn candidates(line: &str, cursor: usize, here: &Here) -> Vec<Candidate> {
    let (start, end) = word_at(line, cursor);
    let frag = &line[start..end];
    let before = &line[..start];
    // A `/` line: the name is completed by the panel itself, its arguments
    // here.
    if line.starts_with('/') {
        return match before.strip_prefix('/') {
            Some(rest) => tidy(command_args(rest, frag, here)),
            None => Vec::new(),
        };
    }
    // A leading bang forces a shell line; it is no part of what it runs.
    let before = before.strip_prefix('!').unwrap_or(before);
    let words: Vec<&str> = before.split_whitespace().collect();
    let mut out: Vec<Candidate> = Vec::new();
    if words.is_empty() && looks_like_path(frag) {
        out = paths(frag, here.folder);
    } else if words.is_empty() {
        // The head of the line: what to run.
        out.extend(starting(here.agents.iter().map(String::as_str), frag, Kind::Agent));
        out.extend(starting(
            targets(here.folder).iter().map(String::as_str),
            frag,
            Kind::Target,
        ));
        out.extend(starting(here.aliases.iter().map(String::as_str), frag, Kind::Binary));
        out.extend(
            binaries(frag, here.path_dirs)
                .into_iter()
                .map(|b| cand(b, Kind::Binary)),
        );
    } else {
        out = arguments(&words, frag, here.folder);
    }
    tidy(out)
}

/// A `/` command's argument: the words it takes there, `/filter`'s search
/// tokens and their values, `/up`'s folders, `/flow`'s flows after its verb
/// (any but `reload`, which is every flow's). `rest` is the line after the
/// slash, up to the word being typed.
fn command_args(rest: &str, frag: &str, here: &Here) -> Vec<Candidate> {
    let mut words = rest.split_whitespace();
    let Some((c, _)) = words.next().and_then(palette::parse) else {
        return Vec::new();
    };
    let said: Vec<&str> = words.collect();
    let at = said.len();
    match c.cmd {
        // A new flow's name is the person's; then `--from`, then a template.
        palette::Cmd::Flow if said.first().copied() == Some("new") => match (at, said.get(2).copied()) {
            (2, _) => starting(["--from"], frag, Kind::Word),
            (3, Some("--from")) => starting(crate::flow::templates::ALL.iter().map(|t| t.name), frag, Kind::Word),
            _ => Vec::new(),
        },
        palette::Cmd::Flow if at == 1 && said[0] != "reload" => {
            starting(here.names.flows.iter().map(String::as_str), frag, Kind::Word)
        }
        palette::Cmd::Filter => {
            let n = here.names;
            query::complete(frag, n.projects, n.flows, n.branches, n.kinds)
                .into_iter()
                .map(|t| cand(t, Kind::Word))
                .collect()
        }
        // `/up` reads a relative path the way the process does.
        palette::Cmd::Up => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| here.folder.to_path_buf());
            paths(frag, &cwd)
        }
        _ => starting(palette::arg_words(c, at).iter().copied(), frag, Kind::Word),
    }
}

/// A word after the first: what a known tool takes there, and otherwise a
/// path. A tool with nothing that fits falls back to paths too, so `make -C
/// sr` still finds `src/`.
fn arguments(words: &[&str], frag: &str, folder: &Path) -> Vec<Candidate> {
    let at = words.len();
    let prev = words.last().copied().unwrap_or("");
    let own: Vec<Candidate> = match (words[0], words.get(1).copied()) {
        ("git", _) if at == 1 => starting(GIT.iter().copied(), frag, Kind::Word),
        ("git", Some(sub)) if GIT_BRANCHED.contains(&sub) => branch_cands(folder, frag, true),
        ("git", Some("branch")) if matches!(prev, "-d" | "-D" | "--delete") => branch_cands(folder, frag, false),
        ("make", _) => starting(make_targets(folder).iter().map(String::as_str), frag, Kind::Target),
        ("just", _) => starting(just_recipes(folder).iter().map(String::as_str), frag, Kind::Target),
        ("npm" | "pnpm" | "yarn" | "bun", Some("run")) if at == 2 => {
            starting(scripts(folder).iter().map(String::as_str), frag, Kind::Target)
        }
        // `yarn dev` and `pnpm dev` run a script without `run`.
        ("yarn" | "pnpm", _) if at == 1 => {
            let mut v = starting(scripts(folder).iter().map(String::as_str), frag, Kind::Target);
            v.extend(starting(["run"], frag, Kind::Word));
            v
        }
        ("npm" | "bun", _) if at == 1 => starting(NPM.iter().copied(), frag, Kind::Word),
        ("cargo", _) if at == 1 => starting(CARGO.iter().copied(), frag, Kind::Word),
        _ => Vec::new(),
    };
    if own.is_empty() { paths(frag, folder) } else { own }
}

/// The branches `git` would take here that start with the fragment: the
/// local ones, then — when `remote` — the remote-tracking ones.
fn branch_cands(folder: &Path, frag: &str, remote: bool) -> Vec<Candidate> {
    let (local, tracking) = branches(folder);
    let mut all = local;
    if remote {
        all.extend(tracking);
    }
    starting(all.iter().map(String::as_str), frag, Kind::Word)
}

/// A first word that can only be a path: it says where it is from.
fn looks_like_path(frag: &str) -> bool {
    frag.starts_with("./") || frag.starts_with("../") || frag.starts_with('/') || frag.starts_with('~')
}

/// The word under the cursor replaced by `text`; gives the new line and where
/// the cursor lands.
pub fn apply(line: &str, cursor: usize, text: &str) -> (String, usize) {
    let (start, end) = word_at(line, cursor);
    let mut out = String::with_capacity(line.len() + text.len());
    out.push_str(&line[..start]);
    out.push_str(text);
    out.push_str(&line[end..]);
    (out, start + text.len())
}

/// The longest prefix every candidate shares, when it is longer than what was
/// typed — what Tab fills in before it starts cycling.
pub fn common_prefix(cands: &[Candidate]) -> Option<String> {
    let first = &cands.first()?.text;
    let mut n = first.len();
    for c in &cands[1..] {
        n = n.min(shared(first, &c.text));
    }
    let p = &first[..n];
    (!p.is_empty()).then(|| p.to_string())
}

/// $PATH, split into directories.
pub fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .filter(|d| !d.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a first word names something this machine can run: an agent, a
/// target in the folder, a binary on PATH, or a path to something runnable
/// (`./build.sh`, `~/bin/deploy`).
pub fn runnable(head: &str, here: &Here) -> bool {
    if head.is_empty() {
        return false;
    }
    here.agents.iter().any(|a| a == head)
        || here.aliases.iter().any(|a| a == head)
        || targets(here.folder).iter().any(|t| t == head)
        || here.path_dirs.iter().any(|d| is_exe(&d.join(head)))
        || (looks_like_path(head) && is_exe(&expand(head, here.folder)))
}

/// How long `$SHELL -ic` gets before it is given up on: long enough for a
/// normal rc, short enough that a stuck one (a network check, an interactive
/// prompt plugin, an rc that misbehaves under a null stdin) cannot freeze the
/// panel.
const SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The names the person's shell knows beyond `$PATH`: its aliases (and shell
/// functions, where it lists them the same way). Asked of `$SHELL`
/// interactively, once, so its rc is read the way a terminal would — the same
/// reason a shell line runs through `$SHELL -ic`, not `sh -c`. Best-effort: an
/// empty list on any trouble, a hang, or a shell that is only `sh`.
pub async fn shell_names() -> Vec<String> {
    let Ok(sh) = std::env::var("SHELL") else {
        return Vec::new();
    };
    if sh.trim().is_empty() || sh.ends_with("/sh") {
        return Vec::new();
    }
    // `alias` prints `name='…'` in zsh, `alias name='…'` in bash; the function
    // lister differs by shell, so ask for names only and let either form pass.
    let script = "alias 2>/dev/null; { print -rl -- ${(k)functions} } 2>/dev/null; declare -F 2>/dev/null";
    let Ok(child) = tokio::process::Command::new(&sh)
        .args(["-ic", script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        // If the timeout below fires, dropping the still-running `child`
        // (inside the timed-out `wait_with_output` future) kills it — nothing
        // is left running in the background.
        .kill_on_drop(true)
        .spawn()
    else {
        return Vec::new();
    };
    // `wait_with_output` drains stdout as it goes, so a chatty rc can't
    // deadlock it against a full pipe the way waiting first, then reading,
    // could.
    let Ok(Ok(out)) = tokio::time::timeout(SHELL_TIMEOUT, child.wait_with_output()).await else {
        return Vec::new();
    };
    parse_shell_names(&String::from_utf8_lossy(&out.stdout))
}

/// Names out of `alias`/`declare -f`/`functions` output: the token before the
/// first `=`, space or tab, once the shell's lead-in words are dropped, kept
/// only when it is a plain identifier one could type as a first word.
fn parse_shell_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in text.lines() {
        let rest = line
            .trim()
            .strip_prefix("alias ")
            .or_else(|| line.trim().strip_prefix("declare -f "))
            .unwrap_or_else(|| line.trim());
        let name = rest.split(['=', ' ', '\t']).next().unwrap_or("").trim();
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || "._-".contains(c)) {
            names.push(name.to_string());
        }
    }
    names.sort();
    names.dedup();
    names
}

/// The bytes the word under the cursor spans: the non-whitespace either side
/// of it. A cursor after a space sits in an empty word, which matches
/// everything. The bang that forces a shell line is not part of its first
/// word.
fn word_at(line: &str, cursor: usize) -> (usize, usize) {
    let mut at = cursor.min(line.len());
    while !line.is_char_boundary(at) {
        at -= 1;
    }
    let start = line[..at]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(usize::from(at > 0 && line.starts_with('!')));
    let end = line[at..]
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| at + i)
        .unwrap_or(line.len());
    (start, end)
}

/// How many bytes two strings share from the front, never cutting a character.
fn shared(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .take_while(|((_, x), y)| x == y)
        .map(|((i, x), _)| i + x.len_utf8())
        .last()
        .unwrap_or(0)
}

/// Sorted by kind then name, one per name, and not more than the cap.
fn tidy(mut out: Vec<Candidate>) -> Vec<Candidate> {
    // Within a kind, by name — except the words Relay itself offers, which
    // keep the order they were written in: `/flow`'s run before its pause.
    out.sort_by(|a, b| {
        a.kind.cmp(&b.kind).then_with(|| {
            if a.kind == Kind::Word {
                std::cmp::Ordering::Equal
            } else {
                a.text.cmp(&b.text)
            }
        })
    });
    let mut seen = HashSet::new();
    out.retain(|c| seen.insert(c.text.clone()));
    out.truncate(CAP);
    out
}

/// Files and directories the fragment could mean. What is before the last `/`
/// says which folder to look in; a fragment of its own comes back on the front
/// so the line reads as it was typed — `~` too, which is looked up as the
/// home folder but stays `~` on the line. A folder ends in `/`, so the next
/// tab goes into it.
fn paths(frag: &str, folder: &Path) -> Vec<Candidate> {
    if frag == "~" {
        return vec![cand("~/", Kind::Path)];
    }
    let (prefix, name) = match frag.rfind('/') {
        Some(i) => (&frag[..=i], &frag[i + 1..]),
        None => ("", frag),
    };
    let Ok(entries) = std::fs::read_dir(expand(prefix, folder)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let file = e.file_name();
        let file = file.to_string_lossy();
        // Hidden files stay hidden until the dot is typed.
        if file.starts_with('.') && !name.starts_with('.') {
            continue;
        }
        if !file.starts_with(name) {
            continue;
        }
        let slash = if e.path().is_dir() { "/" } else { "" };
        out.push(cand(format!("{prefix}{file}{slash}"), Kind::Path));
    }
    out
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// A path as typed, as the file system knows it: `~` is the home folder, and
/// anything relative is from `folder` (an absolute path stays itself).
fn expand(typed: &str, folder: &Path) -> PathBuf {
    match typed.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => home().join(rest.trim_start_matches('/')),
        _ => folder.join(typed),
    }
}

/// What a folder says it can do: its `Makefile` targets and its `package.json`
/// scripts. Anything unreadable or malformed simply has none.
fn targets(folder: &Path) -> Vec<String> {
    let mut out = make_targets(folder);
    out.extend(scripts(folder));
    out
}

/// The folder's `Makefile` targets.
fn make_targets(folder: &Path) -> Vec<String> {
    ["GNUmakefile", "makefile", "Makefile"]
        .iter()
        .find_map(|f| std::fs::read_to_string(folder.join(f)).ok())
        .map(|text| text.lines().filter_map(make_target).map(String::from).collect())
        .unwrap_or_default()
}

/// The folder's `package.json` scripts.
fn scripts(folder: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(folder.join("package.json")) else {
        return Vec::new();
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(json) => json
            .get("scripts")
            .and_then(|s| s.as_object())
            .map(|s| s.keys().cloned().collect())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// A `Makefile` line that names a target: `name:` at the start of the line.
/// A recipe is indented, `.PHONY` is bookkeeping, `VAR:=` is a variable, and
/// `%.o:` is a pattern, not something to ask for by name.
fn make_target(line: &str) -> Option<&str> {
    if line.starts_with([' ', '\t', '#', '.']) {
        return None;
    }
    let (name, rest) = line.split_once(':')?;
    if name.is_empty() || name.contains(char::is_whitespace) || name.contains(['%', '$']) || rest.starts_with('=') {
        return None;
    }
    Some(name)
}

/// The folder's `justfile` recipes, in the order they are written. One
/// whose name starts with `_` is private, and `alias`, `set`, `export`,
/// `import` and `mod` lines, attributes, and `name :=` variables are not
/// recipes.
fn just_recipes(folder: &Path) -> Vec<String> {
    let Some(text) = ["justfile", "Justfile", ".justfile"]
        .iter()
        .find_map(|f| std::fs::read_to_string(folder.join(f)).ok())
    else {
        return Vec::new();
    };
    text.lines().filter_map(just_recipe).map(String::from).collect()
}

/// A `justfile` line that opens a recipe: `name args…:` at the start of the
/// line, `@name:` for a quiet one.
fn just_recipe(line: &str) -> Option<&str> {
    if line.starts_with([' ', '\t', '#', '[', '_']) {
        return None;
    }
    let line = line.strip_prefix('@').unwrap_or(line);
    let end = line.find(|c: char| c.is_whitespace() || c == ':')?;
    let (name, rest) = line.split_at(end);
    let valid = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || "_-".contains(c));
    let keyword = matches!(name, "alias" | "set" | "export" | "import" | "mod");
    let colon = rest.find(':')?;
    if !valid || keyword || rest[colon + 1..].starts_with('=') {
        return None;
    }
    Some(name)
}

/// The branches of the repository `folder` is in: the local ones, and the
/// remote-tracking ones (`origin/main`, never a remote's `HEAD`), each
/// sorted. Asked of `git` itself, so a packed ref counts; given a moment,
/// no more — a slow or missing `git` just means no branches.
fn branches(folder: &Path) -> (Vec<String>, Vec<String>) {
    let Some(text) = git_output(
        folder,
        &["for-each-ref", "--format=%(refname)", "refs/heads", "refs/remotes"],
    ) else {
        return (Vec::new(), Vec::new());
    };
    let (mut local, mut remote) = (Vec::new(), Vec::new());
    for r in text.lines() {
        if let Some(b) = r.strip_prefix("refs/heads/") {
            local.push(b.to_string());
        } else if let Some(b) = r.strip_prefix("refs/remotes/")
            && !b.ends_with("/HEAD")
        {
            remote.push(b.to_string());
        }
    }
    local.sort();
    remote.sort();
    (local, remote)
}

/// What `git` prints in `folder`, if it finishes in time and succeeds. It
/// takes no lock (`GIT_OPTIONAL_LOCKS=0`), so it never gets in the way of a
/// git running beside it; one past its time is killed.
fn git_output(folder: &Path, args: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new("git")
        .args(args)
        .current_dir(folder)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Read on the side, so a long list can't fill the pipe and stall git
    // while this waits on it.
    let mut out = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    let text = rx.recv_timeout(GIT_TIMEOUT);
    if text.is_err() {
        let _ = child.kill();
    }
    let ok = child.wait().is_ok_and(|s| s.success());
    text.ok().filter(|_| ok)
}

/// The binaries on the given directories whose name starts with the fragment.
fn binaries(frag: &str, dirs: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    for d in dirs {
        let Ok(entries) = std::fs::read_dir(d) else {
            continue;
        };
        for e in entries.flatten() {
            let file = e.file_name();
            let file = file.to_string_lossy();
            if file.starts_with(frag) && !file.starts_with('.') && is_exe(&e.path()) {
                out.push(file.into_owned());
            }
        }
    }
    out
}

/// A file this machine can run.
fn is_exe(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder that can build things, and a PATH of our own beside it.
    fn workspace() -> (tempfile::TempDir, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            ".PHONY: build check\nbuild: deps\n\ttrue\ncheck::\n\ttrue\nVAR := 1\n\tnot-a-target:\nno colon here\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts": {"dev": "vite", "check": "tsc"}}"#,
        )
        .unwrap();
        let bin = tempfile::tempdir().unwrap();
        exe(&bin.path().join("cargo"));
        exe(&bin.path().join("check-deps"));
        std::fs::write(bin.path().join("cheese.txt"), "").unwrap();
        (dir, bin)
    }

    fn exe(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn texts(c: &[Candidate]) -> Vec<&str> {
        c.iter().map(|x| x.text.as_str()).collect()
    }

    fn agents() -> Vec<String> {
        vec!["claude".into(), "codex".into()]
    }

    #[test]
    fn the_first_word_offers_agents_before_binaries() {
        let (dir, bin) = workspace();
        let agents = agents();
        let dirs = vec![bin.path().to_path_buf()];
        let here = Here {
            folder: dir.path(),
            agents: &agents,
            path_dirs: &dirs,
            aliases: &[],
            names: Names::default(),
        };
        let c = candidates("c", 1, &here);
        assert_eq!(
            texts(&c),
            ["claude", "codex", "check", "cargo", "check-deps"],
            "agents, then the folder's targets, then PATH"
        );
        assert_eq!(c[0].kind, Kind::Agent);
        assert_eq!(c[2].kind, Kind::Target, "check is a target once, not twice");
        assert_eq!(c[3].kind, Kind::Binary);
        assert!(!texts(&c).contains(&"cheese.txt"), "only what can be run");

        // An empty word matches everything for that position.
        let c = candidates("", 0, &here);
        assert_eq!(
            texts(&c),
            ["claude", "codex", "build", "check", "dev", "cargo", "check-deps"],
            "no .PHONY, no variable, no recipe line"
        );
    }

    #[test]
    fn a_path_completes_against_the_rows_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/machine")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        std::fs::write(dir.path().join(".env"), "").unwrap();
        let here = Here {
            folder: dir.path(),
            agents: &[],
            path_dirs: &[],
            aliases: &[],
            names: Names::default(),
        };
        let c = candidates("cat src/ma", 10, &here);
        assert_eq!(texts(&c), ["src/machine/", "src/main.rs"], "a folder keeps its slash");
        assert!(c.iter().all(|x| x.kind == Kind::Path));
        assert_eq!(texts(&candidates("cat R", 5, &here)), ["README.md"]);
        assert_eq!(
            texts(&candidates("cat ", 4, &here)),
            ["README.md", "src/"],
            "no hidden files until the dot is typed"
        );
        assert_eq!(texts(&candidates("cat .", 5, &here)), [".env"]);
        // `~/` is the home folder, whatever is in it.
        let c = candidates("cat ~/", 6, &here);
        assert!(c.iter().all(|x| x.text.starts_with("~/")));
    }

    #[test]
    fn apply_puts_the_word_back_without_slicing_a_character() {
        let line = "café 🙂x";
        let (out, at) = apply(line, "café".len(), "cafétière");
        assert_eq!(out, "cafétière 🙂x");
        assert_eq!(at, "cafétière".len());
        assert!(out.is_char_boundary(at));

        // The whole word goes, wherever in it the cursor was; the cursor lands
        // after what was put in, counted in bytes, not characters.
        let (out, at) = apply(line, "café 🙂".len(), "ok");
        assert_eq!((out.as_str(), at), ("café ok", 8));
        assert_eq!(apply("make test", 4, "makers").0, "makers test");
        assert_eq!(apply("make test", 0, "go").0, "go test");

        // After a space is an empty word: the text is simply put there.
        let (out, at) = apply("claude ", 7, "fix");
        assert_eq!((out.as_str(), at), ("claude fix", 10));

        // A cursor mid-character is nudged back, never a panic.
        let (out, _) = apply("café", 4, "x");
        assert_eq!(out, "x");

        // What is offered goes back in and reads the same way.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        let here = Here {
            folder: dir.path(),
            agents: &[],
            path_dirs: &[],
            aliases: &[],
            names: Names::default(),
        };
        let line = "cat src/ma";
        let c = candidates(line, line.len(), &here);
        let (out, at) = apply(line, line.len(), &c[0].text);
        assert_eq!((out.as_str(), at), ("cat src/main.rs", 15));
    }

    #[test]
    fn the_shared_prefix_is_what_tab_fills_in() {
        let make = |v: &[&str]| v.iter().map(|t| cand(*t, Kind::Path)).collect::<Vec<_>>();
        assert_eq!(
            common_prefix(&make(&["make-build", "make-check"])),
            Some("make-".into())
        );
        assert_eq!(common_prefix(&make(&["only"])), Some("only".into()));
        assert_eq!(common_prefix(&make(&["apple", "banana"])), None, "nothing in common");
        assert_eq!(common_prefix(&[]), None);
        assert_eq!(
            common_prefix(&make(&["éa", "éb"])),
            Some("é".into()),
            "never mid-character"
        );
        assert_eq!(common_prefix(&make(&["🙂a", "🙂b"])), Some("🙂".into()));
    }

    #[test]
    fn nothing_panics_where_there_is_nothing_to_read() {
        let missing = Path::new("/no/such/folder/anywhere");
        let here = Here {
            folder: missing,
            agents: &[],
            path_dirs: &[missing.to_path_buf()],
            aliases: &[],
            names: Names::default(),
        };
        assert!(candidates("", 0, &here).is_empty());
        assert!(candidates("cat x", 5, &here).is_empty());
        assert!(!runnable("make", &here));

        // A Makefile of noise and a package.json that is not one.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Makefile"), ":\n::\n\t\n   \nx").unwrap();
        std::fs::write(dir.path().join("package.json"), "{\"scripts\": \"nope\"").unwrap();
        let here = Here {
            folder: dir.path(),
            agents: &[],
            path_dirs: &[],
            aliases: &[],
            names: Names::default(),
        };
        assert!(candidates("", 0, &here).is_empty());
    }

    #[test]
    fn runnable_knows_what_this_machine_can_run() {
        let (dir, bin) = workspace();
        let agents = agents();
        let dirs = vec![bin.path().to_path_buf()];
        let here = Here {
            folder: dir.path(),
            agents: &agents,
            path_dirs: &dirs,
            aliases: &[],
            names: Names::default(),
        };
        assert!(runnable("claude", &here), "an agent");
        assert!(runnable("build", &here), "a make target");
        assert!(runnable("dev", &here), "a package script");
        assert!(runnable("cargo", &here), "a binary on PATH");
        assert!(!runnable("cheese.txt", &here), "on PATH, but not runnable");
        assert!(!runnable("nowhere", &here));
        assert!(!runnable("", &here));

        // A shell alias PATH never knows is runnable once the shell is read.
        let aliases = vec!["ll".to_string()];
        let here = Here {
            aliases: &aliases,
            ..here
        };
        assert!(runnable("ll", &here), "a shell alias");
        assert_eq!(candidates("l", 1, &here)[0].text, "ll", "and it completes");
    }

    #[test]
    fn shell_names_reads_aliases_and_functions_from_either_shell() {
        // zsh: `name='…'`; bash: `alias name='…'`; a bare function name.
        let text = "ll='ls -la'\nalias gs='git status'\ngrep\ndeclare -f deploy\n= skip\n/usr/bin/x skip\n";
        assert_eq!(parse_shell_names(text), ["deploy", "grep", "gs", "ll"]);
    }

    #[test]
    fn path_dirs_are_the_ones_in_the_environment() {
        // Whatever the machine's PATH is, every entry is a real directory name.
        assert!(path_dirs().iter().all(|d| !d.as_os_str().is_empty()));
    }

    /// Candidates at the end of the line.
    fn at_end(line: &str, here: &Here) -> Vec<String> {
        candidates(line, line.len(), here).into_iter().map(|c| c.text).collect()
    }

    #[test]
    fn a_slash_commands_arguments_are_its_words() {
        let dir = tempfile::tempdir().unwrap();
        let here = Here::at(dir.path());
        assert_eq!(at_end("/reload ", &here), ["flows", "shell"]);
        assert_eq!(at_end("/rel s", &here), ["shell"], "a name's prefix will do");
        assert!(at_end("/reload shell ", &here).is_empty(), "nothing after shell");
        assert_eq!(at_end("/archive ", &here), ["seen"]);
        assert_eq!(at_end("/pane ", &here), ["full"]);
        assert_eq!(at_end("/resume ", &here), ["all"]);
        assert_eq!(at_end("/resume all ", &here), ["commands"]);
        assert_eq!(at_end("/lens ", &here), ["all", "agents", "flows", "hidden"]);
        assert_eq!(at_end("/lens a", &here), ["all", "agents"]);
        assert_eq!(at_end("/mark b", &here), ["blocked"]);
        assert!(at_end("/stop ", &here).is_empty(), "a command that takes nothing");
        assert!(at_end("/nope ", &here).is_empty(), "no such command");
        // The name itself is the panel's to complete, not a path.
        assert!(at_end("/rel", &here).is_empty());
        // What is taken goes in where the word was.
        let line = "/reload sh";
        assert_eq!(apply(line, line.len(), "shell").0, "/reload shell");

        // /filter: the search tokens, and a token's values.
        let projects = vec!["api".to_string(), "web".to_string(), "global".to_string()];
        let flows = vec!["nightly".to_string()];
        let branches = vec!["main".to_string()];
        let here = Here {
            names: Names {
                projects: &projects,
                flows: &flows,
                branches: &branches,
                kinds: &[],
            },
            ..Here::at(dir.path())
        };
        assert_eq!(at_end("/filter i", &here), ["in:", "is:"]);
        assert_eq!(at_end("/filter build in:", &here), ["in:api", "in:global", "in:web"]);
        assert_eq!(at_end("/filter by:n", &here), ["by:nightly"]);
        assert_eq!(at_end("/filter on:", &here), ["on:main"]);
    }

    #[test]
    fn flow_commands_complete_their_verb_then_a_flow() {
        let dir = tempfile::tempdir().unwrap();
        let flows = vec!["nightly".to_string(), "prod-logs".to_string()];
        let here = Here {
            names: Names {
                flows: &flows,
                ..Names::default()
            },
            ..Here::at(dir.path())
        };
        assert_eq!(
            at_end("/flow ", &here),
            [
                "run", "pause", "resume", "approve", "edit", "new", "test", "draft", "reload"
            ]
        );
        assert_eq!(at_end("/flow re", &here), ["resume", "reload"]);
        assert_eq!(at_end("/flow run ", &here), ["nightly", "prod-logs"]);
        assert_eq!(at_end("/flow edit p", &here), ["prod-logs"]);
        assert_eq!(at_end("/flow test n", &here), ["nightly"]);
        assert!(at_end("/flow run nightly ", &here).is_empty(), "one flow at a time");
        assert!(at_end("/flow reload ", &here).is_empty(), "reload is every flow");
        assert!(at_end("/flow new ", &here).is_empty(), "a new name is the person's");
        assert_eq!(at_end("/flow new x --f", &here), ["--from"]);
        assert_eq!(at_end("/flow new x --from m", &here), ["morning-brief"]);
        assert!(at_end("/flow new x --from prod-logs ", &here).is_empty());
    }

    #[test]
    fn a_first_word_that_looks_like_a_path_completes_as_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
        std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
        exe(&dir.path().join("app/run.sh"));
        std::fs::write(dir.path().join("app/.hidden"), "").unwrap();
        let app = dir.path().join("app");
        let here = Here::at(&app);
        assert_eq!(at_end("./", &here), ["./run.sh", "./src/"], "a folder ends in a slash");
        assert_eq!(at_end("./s", &here), ["./src/"]);
        assert_eq!(at_end("./.", &here), ["./.hidden"], "dotfiles once the dot is typed");
        assert_eq!(at_end("../sc", &here), ["../scripts/"]);
        assert_eq!(at_end("~", &here), ["~/"], "~ stays as typed");
        assert!(at_end("~/", &here).iter().all(|c| c.starts_with("~/")));
        // A line that opens with `/` is a command; with a bang first, a path.
        let abs = format!("!{}/sc", dir.path().display());
        assert_eq!(at_end(&abs, &here), [format!("{}/scripts/", dir.path().display())]);
        // A bang forces a shell line, and stays out of the word it completes.
        assert_eq!(at_end("!./r", &here), ["./run.sh"]);
        assert_eq!(apply("!./r", 4, "./run.sh").0, "!./run.sh");
        // A path to something runnable is something to run.
        assert!(runnable("./run.sh", &here));
        assert!(!runnable("./src/", &here), "a folder is not");
        assert!(!runnable("./nope.sh", &here));
    }

    #[test]
    fn make_and_just_offer_their_targets() {
        let (dir, _bin) = workspace();
        std::fs::write(
            dir.path().join("justfile"),
            "set shell := [\"bash\", \"-c\"]\nalias b := build\nversion := \"1\"\n\n# build it\nbuild:\n    cargo build\n\n@test filter='': build\n    cargo test\n\n[private]\n_hidden:\n    true\n\ndeploy env:\n    ./deploy {{env}}\n",
        )
        .unwrap();
        let here = Here::at(dir.path());
        assert_eq!(at_end("make ", &here), ["build", "check"]);
        assert_eq!(at_end("make -j4 b", &here), ["build"], "anywhere after make");
        assert_eq!(at_end("just ", &here), ["build", "deploy", "test"]);
        assert_eq!(at_end("just d", &here), ["deploy"]);
        // Nothing of make's fits: a path, then.
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        assert_eq!(at_end("make -C su", &here), ["sub/"]);
        // A pattern rule is not a target to ask for by name.
        assert_eq!(make_target("%.o: %.c"), None);
        assert_eq!(make_target("$(OUT): x"), None);
    }

    #[test]
    fn a_package_runner_offers_the_scripts() {
        let (dir, _bin) = workspace();
        let here = Here::at(dir.path());
        for runner in ["npm", "pnpm", "yarn", "bun"] {
            assert_eq!(at_end(&format!("{runner} run "), &here), ["check", "dev"], "{runner}");
        }
        assert_eq!(at_end("npm run d", &here), ["dev"]);
        assert_eq!(at_end("yarn d", &here), ["dev"], "yarn runs a script without run");
        assert_eq!(at_end("pnpm ", &here), ["check", "dev", "run"]);
        assert_eq!(at_end("npm r", &here), ["run"], "npm's own words");
        assert_eq!(at_end("bun ru", &here), ["run"]);
        assert!(
            !at_end("npm run dev ", &here).contains(&"dev".to_string()),
            "one script, then files"
        );
    }

    #[test]
    fn cargo_offers_its_subcommands() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let here = Here::at(dir.path());
        assert_eq!(at_end("cargo cl", &here), ["clean", "clippy"]);
        assert_eq!(at_end("cargo t", &here), ["test", "tree"]);
        assert!(at_end("cargo ", &here).contains(&"build".to_string()));
        assert_eq!(at_end("cargo run s", &here), ["src/"], "after the subcommand, files");
    }

    /// `git` with the settings of this test only, whatever the machine's.
    fn git(dir: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[test]
    fn git_offers_its_subcommands_and_the_branches() {
        let dir = tempfile::tempdir().unwrap();
        let here = Here::at(dir.path());
        assert_eq!(at_end("git ch", &here), ["checkout", "cherry-pick"]);
        assert_eq!(at_end("git sw", &here), ["switch"]);
        // A repository of our own, if there is a git to make one.
        if !git(dir.path(), &["init", "-q", "-b", "main"]) {
            eprintln!("no git: skipping the branches");
            return;
        }
        assert!(git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "one"]));
        assert!(git(dir.path(), &["branch", "feature/login"]));
        assert!(git(dir.path(), &["branch", "fix"]));
        assert!(git(dir.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]));
        assert!(git(
            dir.path(),
            &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]
        ));
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        assert_eq!(
            at_end("git checkout ", &here),
            ["feature/login", "fix", "main", "origin/main"],
            "local, then remote-tracking; never a remote's HEAD"
        );
        assert_eq!(at_end("git switch f", &here), ["feature/login", "fix"]);
        assert_eq!(at_end("git merge o", &here), ["origin/main"]);
        assert_eq!(at_end("git rebase -i m", &here), ["main"]);
        assert_eq!(at_end("git log fe", &here), ["feature/login"]);
        assert_eq!(at_end("git branch -d f", &here), ["feature/login", "fix"]);
        assert_eq!(
            at_end("git branch -D ", &here),
            ["feature/login", "fix", "main"],
            "only local"
        );
        assert_eq!(at_end("git diff sr", &here), ["src/"], "no branch fits: a path");
        assert_eq!(at_end("git add sr", &here), ["src/"], "and elsewhere, paths");
        // Outside any repository: no branches, and no trouble.
        let bare = tempfile::tempdir().unwrap();
        assert!(branches(bare.path()).0.is_empty());
    }

    #[test]
    fn a_shell_line_completes_where_the_shell_is() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join("web/src")).unwrap();
        std::fs::write(project.path().join("web/Makefile"), "serve:\n\ttrue\n").unwrap();
        std::fs::write(project.path().join("README.md"), "").unwrap();
        let cwd = project.path().join("web");
        let agents = agents();
        let p = project.path();
        // A shell line goes to the warm shell, which has `cd`'d into web/.
        assert_eq!(line_folder("ls sr", &agents, p, Some(&cwd)), cwd);
        assert_eq!(line_folder("!./x", &agents, p, Some(&cwd)), cwd);
        // An agent starts in the project; a `/` line is the panel's; a shell
        // that has not said where it is, is taken to be at the project.
        assert_eq!(line_folder("claude fix R", &agents, p, Some(&cwd)), p);
        assert_eq!(line_folder("/up ", &agents, p, Some(&cwd)), p);
        assert_eq!(line_folder("ls sr", &agents, p, None), p);

        let folder = line_folder("ls sr", &agents, p, Some(&cwd));
        let here = Here::at(&folder);
        assert_eq!(at_end("ls sr", &here), ["src/"]);
        assert_eq!(
            at_end("make ", &here),
            ["serve"],
            "its targets are the shell's folder's"
        );
        assert!(at_end("ls R", &here).is_empty(), "not the project's files");
    }
}
