//! A project's `.relay/`, as far as taking it away goes. When a project is
//! removed its rule files can go with it, but only ever aside, into the
//! Relay home's `removed/`, never deleted: the folder may be committed to
//! git, and the files are the user's own writing. The daemon moves them; the
//! panel reads the same verdict to decide whether to ask.

use crate::Home;
use anyhow::{Context, Result};
use jiff::Timestamp;
use std::path::{Path, PathBuf};

/// The files Relay reads from a project's `.relay/`. Anything else in there
/// is the project's own business and stays where it is.
pub const RULE_FILES: [&str; 2] = ["schedules.yaml", "reactions.yaml"];

/// What a `.relay/` is, for moving it aside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RulesDir {
    /// No `.relay/`, or one holding neither rule file: nothing to move.
    Nothing,
    /// A real folder, and these rule files in it.
    Movable(Vec<PathBuf>),
    /// A symlink: it leads somewhere Relay did not make, so it is left be.
    Symlink,
    /// The Relay home itself, as when `~` was registered: moving its files
    /// would take the user's own global rules.
    Home,
}

/// Looks at `dir` (a project's `.relay/`) without touching it.
pub fn rules_dir(dir: &Path, home: &Home) -> RulesDir {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return RulesDir::Nothing;
    };
    if meta.file_type().is_symlink() {
        return RulesDir::Symlink;
    }
    if !meta.is_dir() {
        return RulesDir::Nothing;
    }
    let same = |a: &Path, b: &Path| match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    if same(dir, home.path()) {
        return RulesDir::Home;
    }
    let files: Vec<PathBuf> = RULE_FILES
        .iter()
        .map(|f| dir.join(f))
        .filter(|p| std::fs::symlink_metadata(p).is_ok())
        .collect();
    if files.is_empty() {
        RulesDir::Nothing
    } else {
        RulesDir::Movable(files)
    }
}

/// Moves the rule files out of `dir` into a fresh `<home>/removed/<name>-<when>/`
/// and returns that folder. `dir` itself goes only once nothing else is left
/// in it; a file of the project's own keeps it there. Refuses what
/// [`rules_dir`] says is not movable.
pub fn move_aside(dir: &Path, home: &Home, name: &str, at: Timestamp) -> Result<PathBuf> {
    let files = match rules_dir(dir, home) {
        RulesDir::Movable(files) => files,
        RulesDir::Nothing => anyhow::bail!("{} holds no rules to move", dir.display()),
        RulesDir::Symlink => anyhow::bail!("{} is a symlink; left where it is", dir.display()),
        RulesDir::Home => anyhow::bail!("{} is the Relay home; left where it is", dir.display()),
    };
    let dest = fresh(
        &home.removed_dir(),
        &format!("{}-{}", safe(name), at.strftime("%Y%m%d-%H%M%S")),
    );
    std::fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;
    for f in &files {
        let to = dest.join(f.file_name().expect("a rule file has a name"));
        move_file(f, &to).with_context(|| format!("moving {} to {}", f.display(), to.display()))?;
    }
    // Only an empty folder goes; anything the project keeps there holds it.
    let _ = std::fs::remove_dir(dir);
    Ok(dest)
}

/// `base/stem`, or `base/stem-2`, `-3`… when that is taken, so a second
/// removal within the same second never lands on the first.
fn fresh(base: &Path, stem: &str) -> PathBuf {
    let mut p = base.join(stem);
    let mut n = 2;
    while std::fs::symlink_metadata(&p).is_ok() {
        p = base.join(format!("{stem}-{n}"));
        n += 1;
    }
    p
}

/// A folder name from a project's name: path separators and the like become
/// `-`, so the name can never climb out of `removed/`.
fn safe(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_start_matches('.').to_string();
    if s.is_empty() { "project".into() } else { s }
}

/// A rename, or a copy and then the original's removal when the two are on
/// different filesystems — a move either way, never a copy left behind twice.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> Timestamp {
        "2026-09-23T10:15:00Z".parse().unwrap()
    }

    #[test]
    fn a_relay_folder_with_only_rules_moves_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("home"));
        let dir = tmp.path().join("proj/.relay");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("schedules.yaml"), "schedules: {}\n").unwrap();
        assert_eq!(
            rules_dir(&dir, &home),
            RulesDir::Movable(vec![dir.join("schedules.yaml")])
        );

        let dest = move_aside(&dir, &home, "proj", at()).unwrap();
        assert_eq!(dest, home.removed_dir().join("proj-20260923-101500"));
        assert_eq!(
            std::fs::read_to_string(dest.join("schedules.yaml")).unwrap(),
            "schedules: {}\n"
        );
        assert!(!dir.exists(), "an emptied .relay goes");

        // The same second again lands beside it, not on it.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("reactions.yaml"), "reactions: {}\n").unwrap();
        let again = move_aside(&dir, &home, "proj", at()).unwrap();
        assert_eq!(again, home.removed_dir().join("proj-20260923-101500-2"));
    }

    #[test]
    fn a_file_of_the_projects_own_stays_and_keeps_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("home"));
        let dir = tmp.path().join("proj/.relay");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("schedules.yaml"), "").unwrap();
        std::fs::write(dir.join("reactions.yaml"), "").unwrap();
        std::fs::write(dir.join("notes.md"), "mine").unwrap();
        let dest = move_aside(&dir, &home, "proj", at()).unwrap();
        assert!(dest.join("schedules.yaml").is_file() && dest.join("reactions.yaml").is_file());
        assert_eq!(std::fs::read_to_string(dir.join("notes.md")).unwrap(), "mine");
        assert!(!dir.join("schedules.yaml").exists());
    }

    #[test]
    fn a_symlink_the_home_and_an_empty_folder_are_not_moved() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join(".relay"));
        std::fs::create_dir_all(home.path()).unwrap();
        std::fs::write(home.schedules_file(), "").unwrap();
        // `~` registered: its `.relay` is the home.
        assert_eq!(rules_dir(&tmp.path().join(".relay"), &home), RulesDir::Home);
        assert!(move_aside(&tmp.path().join(".relay"), &home, "me", at()).is_err());
        assert!(home.schedules_file().is_file());

        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(tmp.path().join("elsewhere")).unwrap();
        std::fs::write(tmp.path().join("elsewhere/schedules.yaml"), "").unwrap();
        std::fs::create_dir_all(&proj).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("elsewhere"), proj.join(".relay")).unwrap();
        assert_eq!(rules_dir(&proj.join(".relay"), &home), RulesDir::Symlink);
        assert!(move_aside(&proj.join(".relay"), &home, "proj", at()).is_err());
        assert!(tmp.path().join("elsewhere/schedules.yaml").is_file());

        let bare = tmp.path().join("bare/.relay");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::write(bare.join("config.yaml"), "").unwrap();
        assert_eq!(rules_dir(&bare, &home), RulesDir::Nothing);
        assert_eq!(rules_dir(&tmp.path().join("none/.relay"), &home), RulesDir::Nothing);
    }

    #[test]
    fn a_name_cannot_climb_out_of_removed() {
        assert_eq!(safe("../../etc"), "-..-etc");
        assert_eq!(safe("my proj/x"), "my-proj-x");
        assert_eq!(safe(""), "project");
    }
}
