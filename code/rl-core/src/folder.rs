//! A flow's folder, as its hash reads it: every file in it but `runs/`. The
//! daemon reads it to load a flow and again before each step it starts; `rl
//! flow edit` reads it to say yes to no more than what was edited.

use std::io;
use std::path::{Path, PathBuf};

/// What a folder holds.
#[derive(Debug, Default)]
pub struct Files {
    /// Each file by its path in the folder, with its bytes. A link to a file
    /// counts as the file.
    pub files: Vec<(PathBuf, Vec<u8>)>,
    /// Links to folders, by their paths in the folder. They are not followed:
    /// what is under one would run unhashed, so a flow with one does not load.
    pub linked: Vec<PathBuf>,
}

/// Every file under `dir` but `runs/`. A pipe, socket or device is passed
/// over, so reading never waits on one.
pub fn read(dir: &Path) -> io::Result<Files> {
    let mut out = Files::default();
    let mut todo = vec![PathBuf::new()];
    while let Some(rel) = todo.pop() {
        for entry in std::fs::read_dir(dir.join(&rel))? {
            let entry = entry?;
            let path = rel.join(entry.file_name());
            if path == Path::new("runs") {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                todo.push(path);
                continue;
            }
            let target = || std::fs::metadata(entry.path());
            if kind.is_symlink() && target().is_ok_and(|m| m.is_dir()) {
                out.linked.push(path);
            } else if kind.is_file() || (kind.is_symlink() && target().is_ok_and(|m| m.is_file())) {
                out.files.push((path, std::fs::read(entry.path())?));
            }
        }
    }
    out.files.sort_by(|a, b| a.0.cmp(&b.0));
    out.linked.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(f: &Files) -> Vec<PathBuf> {
        f.files.iter().map(|(p, _)| p.clone()).collect()
    }

    #[test]
    fn a_folder_is_every_file_in_it_but_its_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("runs/f1")).unwrap();
        std::fs::write(dir.join("flow.md"), "x").unwrap();
        std::fs::write(dir.join("lib/a.sh"), "a").unwrap();
        std::fs::write(dir.join("runs/f1/probe.out"), "o").unwrap();
        let got = read(dir).unwrap();
        assert_eq!(paths(&got), [PathBuf::from("flow.md"), PathBuf::from("lib/a.sh")]);
        assert!(got.linked.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_pipe_is_passed_over_and_a_link_to_a_folder_said() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("flow.md"), "x").unwrap();
        std::fs::write(dir.join("lib/a.sh"), "a").unwrap();
        nix::unistd::mkfifo(&dir.join("pipe"), nix::sys::stat::Mode::S_IRWXU).unwrap();
        std::os::unix::fs::symlink(dir, dir.join("lib/up")).unwrap();
        std::os::unix::fs::symlink(dir.join("lib/a.sh"), dir.join("b.sh")).unwrap();
        let got = read(dir).unwrap();
        assert_eq!(paths(&got), ["b.sh", "flow.md", "lib/a.sh"].map(PathBuf::from));
        assert_eq!(got.linked, [PathBuf::from("lib/up")]);
    }
}
