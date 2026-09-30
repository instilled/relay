//! Writing one rule into a `schedules.yaml` or `reactions.yaml` without
//! touching the rest of it. The file is the user's: comments, order and
//! spacing around other rules stay as they were. A rule is a block under the
//! top key (`schedules:` or `reactions:`), found by its name; putting one in
//! replaces the block of that name or adds one at the end of the mapping.

use crate::client::Client;
use anyhow::{Context, Result, bail};
use rl_proto::methods::{ReactionReload, RuleApprove, RuleRef, ScheduleReload, WorkspaceUp, WorkspaceUpParams};
use rl_proto::rule::RuleKind;
use rl_proto::schedule::GLOBAL_SCOPE;
use std::path::{Path, PathBuf};

/// A rule to write down: which file, and its block.
#[derive(Debug)]
pub struct RuleDraft {
    pub kind: RuleKind,
    /// The folder whose `.relay/` it goes in; none for the global rules.
    pub folder: Option<PathBuf>,
    pub name: String,
    pub body: Vec<String>,
}

impl RuleDraft {
    pub fn scope(&self) -> String {
        match &self.folder {
            Some(f) => f.to_string_lossy().into_owned(),
            None => GLOBAL_SCOPE.into(),
        }
    }
}

/// The rule a draft replaces, when it is an edit: a rename or a move takes
/// the old block out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Was {
    pub kind: RuleKind,
    pub scope: String,
    pub name: String,
}

/// Writes the draft into its file, then makes it so: a folder new to Relay
/// is registered (and its rules approved, this being the user's yes);
/// otherwise the kind is reloaded and this one rule approved. Returns the
/// file. `registered` says whether the folder is known to the daemon.
pub async fn install(
    home: &rl_core::Home,
    rpc: &mut Client,
    d: &RuleDraft,
    was: Option<&Was>,
    registered: bool,
) -> Result<PathBuf> {
    let file = file(home, d.kind, d.folder.as_deref());
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let new = upsert(&text, top(d.kind), &d.name, &d.body)?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&file, new).with_context(|| format!("writing {}", file.display()))?;
    let scope = d.scope();
    if let Some(w) = was
        && (w.kind, w.scope.as_str(), w.name.as_str()) != (d.kind, scope.as_str(), d.name.as_str())
    {
        let folder = (w.scope != GLOBAL_SCOPE).then(|| PathBuf::from(&w.scope));
        let old = self::file(home, w.kind, folder.as_deref());
        if let Ok(t) = std::fs::read_to_string(&old)
            && let Some(t) = remove(&t, top(w.kind), &w.name)
        {
            std::fs::write(&old, t)?;
        }
    }
    // From here on, the file already carries the rule: `ScheduleReload` /
    // `ReactionReload` (and `WorkspaceUp`'s scan of the folder) re-read it
    // from disk, so the write above can't be deferred until after these
    // calls succeed. If one of them fails, the write still stands, so a bare
    // error would be misleading: say so, so a retry isn't confusing.
    let partial = |e: anyhow::Error| {
        e.context(format!(
            "{} was saved to disk, but the daemon may not be caught up yet — retry is safe",
            file.display()
        ))
    };
    if let (Some(path), false) = (&d.folder, registered) {
        rpc.call::<WorkspaceUp>(WorkspaceUpParams {
            path: path.clone(),
            approve: true,
        })
        .await
        .map_err(partial)?;
    } else {
        match d.kind {
            RuleKind::Schedule => {
                rpc.call::<ScheduleReload>(()).await.map_err(partial)?;
            }
            RuleKind::Reaction => {
                rpc.call::<ReactionReload>(()).await.map_err(partial)?;
            }
        }
        rpc.call::<RuleApprove>(RuleRef {
            name: d.name.clone(),
            scope: Some(scope),
            kind: Some(d.kind),
        })
        .await
        .map_err(partial)?;
    }
    Ok(file)
}

/// The top key a kind's rules live under.
pub fn top(kind: RuleKind) -> &'static str {
    match kind {
        RuleKind::Schedule => "schedules",
        RuleKind::Reaction => "reactions",
    }
}

/// The file a kind's rules are in: yours in the home, a folder's in `.relay/`.
pub fn file(home: &rl_core::Home, kind: RuleKind, folder: Option<&Path>) -> std::path::PathBuf {
    let name = format!("{}.yaml", top(kind));
    match folder {
        Some(f) => f.join(rl_proto::schedule::WORKSPACE_DIR).join(name),
        None => home.path().join(name),
    }
}

/// The rule under the cursor, as `/edit` needs it: its file, its kind and
/// its name, to find its line.
pub struct Here<'a> {
    pub source: &'a Path,
    pub kind: RuleKind,
    pub name: &'a str,
}

/// Which file `/edit` opens in `$EDITOR`, and at which line, for the words
/// after it and what the cursor stands on. `schedules` or `reactions` (or
/// the singular) is that kind's file for the folder the cursor is in, the
/// Relay home's where nothing names a folder; `global` after it is the
/// home's from anywhere. Bare, it is the rule under the cursor's own file at
/// its line, else the folder's file that did not load at the line it broke
/// on, else the folder's schedules. A file named by words that did not load
/// opens at that line too. `broken` is every file that did not load, with
/// its line. Words it does not take are `Err`, saying which it does.
pub fn edit_target(
    home: &rl_core::Home,
    words: &str,
    rule: Option<Here>,
    folder: Option<&Path>,
    broken: &[(&Path, usize)],
) -> std::result::Result<(std::path::PathBuf, usize), String> {
    let at = |f: PathBuf| {
        let line = broken.iter().find(|(b, _)| *b == f).map_or(1, |(_, l)| *l);
        (f, line)
    };
    let words: Vec<&str> = words.split_whitespace().collect();
    let kind = |w: &str| match w {
        "schedule" | "schedules" => Some(RuleKind::Schedule),
        "reaction" | "reactions" => Some(RuleKind::Reaction),
        _ => None,
    };
    let asked = match words.as_slice() {
        [] => None,
        [w] => kind(w).map(|k| (k, folder)),
        [w, "global"] => kind(w).map(|k| (k, None)),
        _ => None,
    };
    match (words.is_empty(), asked, rule) {
        (true, _, Some(r)) => {
            let line = std::fs::read_to_string(r.source)
                .ok()
                .and_then(|t| line_of(&t, top(r.kind), r.name))
                .unwrap_or(1);
            Ok((r.source.to_path_buf(), line))
        }
        (true, _, None) => {
            let mine = [RuleKind::Schedule, RuleKind::Reaction].map(|k| file(home, k, folder));
            let first = broken.iter().find(|(b, _)| mine.iter().any(|m| m == b));
            Ok(match first {
                Some((b, line)) => (b.to_path_buf(), *line),
                None => at(file(home, RuleKind::Schedule, folder)),
            })
        }
        (false, Some((k, folder)), _) => Ok(at(file(home, k, folder))),
        (false, None, _) => Err(format!(
            "edit {}? schedules or reactions, and global for the Relay home's",
            words.join(" ")
        )),
    }
}

/// A rule name: letters, digits, `.`, `_` and `-`.
pub fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        bail!("a name is letters, digits, `.`, `_` and `-`");
    }
    Ok(())
}

/// A rule's body as lines, from its spec: what differs from the default,
/// in the order the fields are declared, the way `serde` writes YAML.
pub fn body<T: serde::Serialize>(spec: &T, default: &T) -> Result<Vec<String>> {
    let mut v = serde_norway::to_value(spec)?;
    let d = serde_norway::to_value(default)?;
    if let (serde_norway::Value::Mapping(m), serde_norway::Value::Mapping(dm)) = (&mut v, &d) {
        m.retain(|k, val| dm.get(k) != Some(val));
    }
    let text = serde_norway::to_string(&v)?;
    Ok(text.lines().map(str::to_string).collect())
}

/// Puts `name` with `body` (lines, unindented) into `text` under `top`:
/// replacing the block of that name, or adding one at the end of the mapping.
/// Everything else in the file stays as it was. The result is checked to
/// parse and to carry the rule.
pub fn upsert(text: &str, top: &str, name: &str, body: &[String]) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();
    let map = mapping(&lines, top);
    let indent = map
        .as_ref()
        .and_then(|m| m.indent.clone())
        .unwrap_or_else(|| "  ".to_string());
    let block = |indent: &str| -> Vec<String> {
        let mut b = vec![format!("{indent}{name}:")];
        b.extend(body.iter().map(|l| format!("{indent}{indent}{l}")));
        b
    };
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    match map {
        None => {
            // No mapping yet: one at the end of the file.
            if out.iter().any(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(format!("{top}:"));
            out.extend(block("  "));
        }
        Some(m) if m.empty => {
            out.splice(
                m.start..m.start + 1,
                std::iter::once(format!("{top}:")).chain(block("  ")),
            );
        }
        Some(m) => match m.block(name) {
            Some((from, to)) => {
                out.splice(from..to, block(&indent));
            }
            None => {
                // After the last line of the mapping that says something.
                let at = (m.start + 1..m.end)
                    .rev()
                    .find(|&i| !lines[i].trim().is_empty() && !lines[i].trim_start().starts_with('#'))
                    .map(|i| i + 1)
                    .unwrap_or(m.start + 1);
                out.splice(at..at, block(&indent));
            }
        },
    }
    let mut result = out.join("\n");
    result.push('\n');
    let v: serde_norway::Value = serde_norway::from_str(&result).context("the file would not parse")?;
    if v.get(top).and_then(|m| m.get(name)).is_none() {
        bail!("`{name}` did not land under `{top}:`");
    }
    Ok(result)
}

/// Takes `name` out from under `top`; nothing else changes. Ok(None) when it
/// was not there.
pub fn remove(text: &str, top: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let (from, to) = mapping(&lines, top)?.block(name)?;
    let mut out: Vec<&str> = lines[..from].to_vec();
    out.extend_from_slice(&lines[to..]);
    let mut s = out.join("\n");
    s.push('\n');
    Some(s)
}

/// The line (1-based) `name` is written on, for an editor.
pub fn line_of(text: &str, top: &str, name: &str) -> Option<usize> {
    let lines: Vec<&str> = text.lines().collect();
    mapping(&lines, top)?.block(name).map(|(from, _)| from + 1)
}

/// The lines of the `top:` mapping, from its key line to the next thing at
/// column zero.
struct Mapping<'a> {
    lines: &'a [&'a str],
    /// The `top:` line.
    start: usize,
    /// One past the last line of the mapping.
    end: usize,
    /// `top: {}`.
    empty: bool,
    /// The indent its keys use, when it has any.
    indent: Option<String>,
}

fn mapping<'a>(lines: &'a [&'a str], top: &str) -> Option<Mapping<'a>> {
    let start = lines.iter().position(|l| {
        let l = l.trim_end();
        l == format!("{top}:") || l.starts_with(&format!("{top}: ")) || l.starts_with(&format!("{top}:#"))
    })?;
    let rest = lines[start].trim_end()[top.len() + 1..].trim();
    let empty = rest.starts_with("{}");
    let end = (start + 1..lines.len())
        .find(|&i| {
            let l = lines[i];
            !l.is_empty() && !l.starts_with([' ', '\t', '#'])
        })
        .unwrap_or(lines.len());
    let indent = (start + 1..end)
        .map(|i| lines[i])
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| l[..l.len() - l.trim_start().len()].to_string());
    Some(Mapping {
        lines,
        start,
        end,
        empty,
        indent,
    })
}

impl Mapping<'_> {
    /// The lines `[from, to)` of the block named `name`.
    fn block(&self, name: &str) -> Option<(usize, usize)> {
        let indent = self.indent.as_deref()?;
        let key = format!("{indent}{name}:");
        let from = (self.start + 1..self.end).find(|&i| {
            let l = self.lines[i].trim_end();
            l == key || l.starts_with(&format!("{key} ")) || l.starts_with(&format!("{key}#"))
        })?;
        let to = (from + 1..self.end)
            .find(|&i| {
                let l = self.lines[i];
                let lead = l.len() - l.trim_start().len();
                !l.trim().is_empty() && !l.trim_start().starts_with('#') && lead <= indent.len()
            })
            .unwrap_or(self.end);
        // Blank lines and comments right before the next key belong to it.
        let to = (from + 1..to)
            .rev()
            .find(|&i| !self.lines[i].trim().is_empty() && !self.lines[i].trim_start().starts_with('#'))
            .map(|i| i + 1)
            .unwrap_or(from + 1);
        Some((from, to))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
# my rules
schedules:
  # the nightly one
  nightly:
    every: 4h
    run: make test

  backup:
    cron: \"0 2 * * *\"
    run: ./backup.sh
";

    #[test]
    fn a_rule_goes_in_after_the_others_and_comments_stay() {
        let body = vec!["every: 30m".to_string(), "run: make lint".to_string()];
        let out = upsert(FILE, "schedules", "lint", &body).unwrap();
        assert_eq!(
            out,
            "\
# my rules
schedules:
  # the nightly one
  nightly:
    every: 4h
    run: make test

  backup:
    cron: \"0 2 * * *\"
    run: ./backup.sh
  lint:
    every: 30m
    run: make lint
"
        );
        assert_eq!(line_of(&out, "schedules", "lint"), Some(11));
        assert_eq!(line_of(&out, "schedules", "nightly"), Some(4));
    }

    #[test]
    fn a_rule_of_the_same_name_is_replaced_in_place() {
        let body = vec![
            "every: 6h".to_string(),
            "run: make test".to_string(),
            "title: nightly".to_string(),
        ];
        let out = upsert(FILE, "schedules", "nightly", &body).unwrap();
        assert_eq!(
            out,
            "\
# my rules
schedules:
  # the nightly one
  nightly:
    every: 6h
    run: make test
    title: nightly

  backup:
    cron: \"0 2 * * *\"
    run: ./backup.sh
"
        );
        let gone = remove(&out, "schedules", "nightly").unwrap();
        assert!(!gone.contains("nightly:") && gone.contains("backup:"), "{gone}");
        assert!(remove(&out, "schedules", "nope").is_none());
    }

    #[test]
    fn an_empty_or_missing_file_gets_the_mapping() {
        let body = vec!["on: run.failed".to_string(), "run: say no".to_string()];
        assert_eq!(
            upsert("", "reactions", "notify", &body).unwrap(),
            "reactions:\n  notify:\n    on: run.failed\n    run: say no\n"
        );
        assert_eq!(
            upsert("# nothing yet\nreactions: {}\n", "reactions", "notify", &body).unwrap(),
            "# nothing yet\nreactions:\n  notify:\n    on: run.failed\n    run: say no\n"
        );
        // Another top key already there: the mapping goes after it.
        let out = upsert("other: 1\n", "reactions", "notify", &body).unwrap();
        assert_eq!(
            out,
            "other: 1\n\nreactions:\n  notify:\n    on: run.failed\n    run: say no\n"
        );
    }

    #[test]
    fn the_files_own_indent_is_kept() {
        let text = "schedules:\n    wide:\n        every: 1h\n        run: a\n";
        let body = vec!["every: 2h".to_string(), "run: b".to_string()];
        let out = upsert(text, "schedules", "two", &body).unwrap();
        assert_eq!(
            out,
            "schedules:\n    wide:\n        every: 1h\n        run: a\n    two:\n        every: 2h\n        run: b\n"
        );
    }

    #[test]
    fn a_body_says_only_what_differs_and_reads_back() {
        use rl_proto::action::Task;
        use rl_proto::schedule::ScheduleSpec;
        let spec = ScheduleSpec {
            cron: Some("0 2 * * *".into()),
            run: Some(Task::Agent {
                agent: "claude".into(),
                prompt: Some("fix the build: it's red".into()),
                prompt_file: None,
                args: vec![],
            }),
            tags: vec!["nightly".into()],
            title: Some("night".into()),
            ..Default::default()
        };
        let body = body(&spec, &ScheduleSpec::default()).unwrap();
        assert_eq!(
            body,
            [
                "cron: 0 2 * * *",
                "run:",
                "  agent: claude",
                "  prompt: 'fix the build: it''s red'",
                "tags:",
                "- nightly",
                "title: night"
            ]
        );
        let out = upsert("", "schedules", "night", &body).unwrap();
        let v: serde_norway::Value = serde_norway::from_str(&out).unwrap();
        let back: ScheduleSpec = serde_norway::from_value(v["schedules"]["night"].clone()).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn edit_opens_the_file_the_words_and_the_cursor_name() {
        let home = rl_core::Home::at("/h");
        let w = Path::new("/w");
        let open = |words: &str, folder: Option<&Path>| edit_target(&home, words, None, folder, &[]);
        let at = |p: &str| Ok((PathBuf::from(p), 1));
        // Bare, or schedules: the folder's schedules; nothing naming a folder
        // (the `everywhere` header, an empty list): the home's.
        assert_eq!(open("", Some(w)), at("/w/.relay/schedules.yaml"));
        assert_eq!(open("", None), at("/h/schedules.yaml"));
        assert_eq!(open("schedules", Some(w)), at("/w/.relay/schedules.yaml"));
        assert_eq!(
            open("schedule", Some(w)),
            at("/w/.relay/schedules.yaml"),
            "the singular still works"
        );
        assert_eq!(open("reactions", Some(w)), at("/w/.relay/reactions.yaml"));
        assert_eq!(open("reaction", None), at("/h/reactions.yaml"));
        // global: the home's, whatever the cursor is on.
        assert_eq!(open("schedules global", Some(w)), at("/h/schedules.yaml"));
        assert_eq!(open("reactions  global", Some(w)), at("/h/reactions.yaml"));
        // Gone: the form, and `file`; anything else says what it takes.
        for bad in ["file", "global", "schedules nope", "reactions global more"] {
            let e = open(bad, Some(w)).unwrap_err();
            assert!(e.contains("schedules or reactions"), "{bad}: {e}");
        }

        // Bare on a rule: that rule's own file, at its line.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("schedules.yaml");
        std::fs::write(&src, FILE).unwrap();
        let rule = |name| {
            Some(Here {
                source: &src,
                kind: RuleKind::Schedule,
                name,
            })
        };
        assert_eq!(
            edit_target(&home, "", rule("backup"), Some(w), &[]),
            Ok((src.clone(), 8))
        );
        assert_eq!(
            edit_target(&home, "", rule("gone"), Some(w), &[]),
            Ok((src.clone(), 1)),
            "not found: the top"
        );
        // Words still say which file, rule or not.
        assert_eq!(
            edit_target(&home, "reactions", rule("backup"), Some(w), &[]),
            at("/w/.relay/reactions.yaml")
        );
    }

    #[test]
    fn edit_opens_a_file_that_did_not_load_at_the_line() {
        let home = rl_core::Home::at("/h");
        let w = Path::new("/w");
        let (mine, yours) = (
            PathBuf::from("/w/.relay/reactions.yaml"),
            PathBuf::from("/h/schedules.yaml"),
        );
        let broken: [(&Path, usize); 2] = [(&mine, 7), (&yours, 4)];
        let open = |words: &str, folder: Option<&Path>| edit_target(&home, words, None, folder, &broken);
        // Bare on a header: its folder's broken file, at the line, before its
        // schedules; `everywhere`'s is the home's.
        assert_eq!(open("", Some(w)), Ok((mine.clone(), 7)));
        assert_eq!(open("", None), Ok((yours.clone(), 4)));
        // Named, at the line when it is the broken one, else the top.
        assert_eq!(open("schedules global", Some(w)), Ok((yours.clone(), 4)));
        assert_eq!(
            open("schedules", Some(w)),
            Ok((PathBuf::from("/w/.relay/schedules.yaml"), 1))
        );
        assert_eq!(open("reactions", Some(w)), Ok((mine.clone(), 7)));
        // Another folder's broken file is not this one's.
        assert_eq!(
            open("", Some(Path::new("/v"))),
            Ok((PathBuf::from("/v/.relay/schedules.yaml"), 1))
        );
    }
}
