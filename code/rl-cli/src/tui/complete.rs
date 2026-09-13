//! What the line at the foot of the panel can be completed to.
//!
//! The line is rooted in the folder of the row the cursor is on, so a path
//! means that workspace's, not the folder `rl` was started in. The first word
//! is what to run — an agent, one of the folder's make targets or scripts, a
//! binary on your PATH — or the word that starts a rule. After `every` comes
//! a duration, after `on` an event; anywhere else, a file.

use relay_proto::types::event_kind;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The durations offered after `every`.
const DURATIONS: &[&str] = &["30s", "5m", "15m", "30m", "1h", "6h", "12h", "24h"];

/// The words that begin a rule rather than a run.
const RULE_WORDS: &[&str] = &["every", "on", "cron"];

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
    /// The workspace of the row the cursor is on; paths complete against it.
    pub folder: &'a Path,
    /// The agents the registry knows, never a hardcoded list.
    pub agents: &'a [String],
    /// The directories of $PATH — given, so the tests can supply their own.
    pub path_dirs: &'a [PathBuf],
}

fn cand(text: impl Into<String>, kind: Kind) -> Candidate {
    Candidate {
        text: text.into(),
        kind,
    }
}

/// Candidates for the word the cursor sits in, best first.
pub fn candidates(line: &str, cursor: usize, here: &Here) -> Vec<Candidate> {
    let (start, end) = word_at(line, cursor);
    let frag = &line[start..end];
    let before = &line[..start];
    let mut out: Vec<Candidate> = Vec::new();
    if before.trim().is_empty() {
        // The head of the line: what to run, or the word that opens a rule.
        out.extend(
            here.agents
                .iter()
                .filter(|a| a.starts_with(frag))
                .map(|a| cand(a.clone(), Kind::Agent)),
        );
        out.extend(
            targets(here.folder)
                .into_iter()
                .filter(|t| t.starts_with(frag))
                .map(|t| cand(t, Kind::Target)),
        );
        out.extend(
            binaries(frag, here.path_dirs)
                .into_iter()
                .map(|b| cand(b, Kind::Binary)),
        );
        out.extend(
            RULE_WORDS
                .iter()
                .filter(|w| w.starts_with(frag))
                .map(|w| cand(*w, Kind::Word)),
        );
    } else {
        match before.split_whitespace().next_back() {
            Some("every") => out.extend(
                DURATIONS
                    .iter()
                    .filter(|d| d.starts_with(frag))
                    .map(|d| cand(*d, Kind::Word)),
            ),
            Some("on") => out.extend(
                event_kind::ALL
                    .iter()
                    .filter(|k| k.starts_with(frag))
                    .map(|k| cand(*k, Kind::Word)),
            ),
            _ => out = paths(frag, here.folder),
        }
    }
    tidy(out)
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
/// target in the folder, or a binary on PATH.
pub fn runnable(head: &str, here: &Here) -> bool {
    if head.is_empty() {
        return false;
    }
    here.agents.iter().any(|a| a == head)
        || targets(here.folder).iter().any(|t| t == head)
        || here.path_dirs.iter().any(|d| is_exe(&d.join(head)))
}

/// The bytes the word under the cursor spans: the non-whitespace either side
/// of it. A cursor after a space sits in an empty word, which matches
/// everything.
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
        .unwrap_or(0);
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
    // keep the order they were written in: 30m before 1h, not 12h before 15m.
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
/// so the line reads as it was typed.
fn paths(frag: &str, folder: &Path) -> Vec<Candidate> {
    let (prefix, name) = match frag.rfind('/') {
        Some(i) => (&frag[..=i], &frag[i + 1..]),
        None => ("", frag),
    };
    let dir = match prefix.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => folder.join(prefix),
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
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

/// What a folder says it can do: its `Makefile` targets and its `package.json`
/// scripts. Anything unreadable or malformed simply has none.
fn targets(folder: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string(folder.join("Makefile")) {
        out.extend(text.lines().filter_map(make_target).map(String::from));
    }
    if let Ok(text) = std::fs::read_to_string(folder.join("package.json"))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(scripts) = json.get("scripts").and_then(|s| s.as_object())
    {
        out.extend(scripts.keys().cloned());
    }
    out
}

/// A `Makefile` line that names a target: `name:` at the start of the line.
/// A recipe is indented, `.PHONY` is bookkeeping, and `VAR:=` is a variable.
fn make_target(line: &str) -> Option<&str> {
    if line.starts_with([' ', '\t', '#', '.']) {
        return None;
    }
    let (name, rest) = line.split_once(':')?;
    if name.is_empty() || name.contains(char::is_whitespace) || rest.starts_with('=') {
        return None;
    }
    Some(name)
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
        };
        let c = candidates("c", 1, &here);
        assert_eq!(
            texts(&c),
            ["claude", "codex", "check", "cargo", "check-deps", "cron"],
            "agents, then the folder's targets, then PATH, then the rule words"
        );
        assert_eq!(c[0].kind, Kind::Agent);
        assert_eq!(c[2].kind, Kind::Target, "check is a target once, not twice");
        assert_eq!(c[3].kind, Kind::Binary);
        assert_eq!(c[5].kind, Kind::Word);
        assert!(!texts(&c).contains(&"cheese.txt"), "only what can be run");

        // An empty word matches everything for that position.
        let c = candidates("", 0, &here);
        assert_eq!(
            texts(&c),
            [
                "claude",
                "codex",
                "build",
                "check",
                "dev",
                "cargo",
                "check-deps",
                "every",
                "on",
                "cron"
            ],
            "no .PHONY, no variable, no recipe line; Relay's own words last, as written"
        );
    }

    #[test]
    fn every_and_on_say_what_comes_next() {
        let dir = tempfile::tempdir().unwrap();
        let here = Here {
            folder: dir.path(),
            agents: &[],
            path_dirs: &[],
        };
        let c = candidates("every ", 6, &here);
        assert_eq!(c.len(), DURATIONS.len());
        assert!(texts(&c).contains(&"30s"));
        assert!(c.iter().all(|x| x.kind == Kind::Word));
        assert_eq!(
            texts(&candidates("every 1", 7, &here)),
            ["15m", "1h", "12h"],
            "short to long"
        );

        let c = candidates("on ", 3, &here);
        assert_eq!(c.len(), event_kind::ALL.len());
        assert_eq!(texts(&candidates("on run.f", 8, &here)), ["run.finished", "run.failed"]);
        // The cursor sits in the word it is completing, wherever the line goes.
        assert_eq!(
            texts(&candidates("on run.f now", 8, &here)),
            ["run.finished", "run.failed"]
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
        };
        assert_eq!(texts(&candidates("", 0, &here)), ["every", "on", "cron"]);
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
        };
        assert_eq!(texts(&candidates("", 0, &here)), ["every", "on", "cron"]);
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
        };
        assert!(runnable("claude", &here), "an agent");
        assert!(runnable("build", &here), "a make target");
        assert!(runnable("dev", &here), "a package script");
        assert!(runnable("cargo", &here), "a binary on PATH");
        assert!(!runnable("cheese.txt", &here), "on PATH, but not runnable");
        assert!(!runnable("nowhere", &here));
        assert!(!runnable("", &here));
    }

    #[test]
    fn path_dirs_are_the_ones_in_the_environment() {
        // Whatever the machine's PATH is, every entry is a real directory name.
        assert!(path_dirs().iter().all(|d| !d.as_os_str().is_empty()));
    }
}
