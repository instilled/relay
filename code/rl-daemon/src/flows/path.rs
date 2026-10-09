//! What a step finds first on its `PATH`: the flow's own folder, then `run/bin/`, a folder `rld`
//! owns holding only a link to the `rl` beside it — so `rl flow state` in a step is the `rl` that
//! matches the `rld` running it, and nothing else of `rld`'s folder goes ahead of the user's own.

use rl_core::Home;
use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use tracing::warn;

/// The `rl` beside this `rld`, when there is one.
pub(crate) fn beside_rld() -> Option<PathBuf> {
    let rl = std::env::current_exe().ok()?.with_file_name("rl");
    rl.is_file().then_some(rl)
}

/// Makes `run/bin/rl` a link to `rl`, over whatever link was there; with no `rl`, takes it away.
/// The folder, when it holds the link: what goes on a step's `PATH`.
pub(crate) fn link_rl(home: &Home, rl: Option<&Path>) -> Option<PathBuf> {
    let bin = home.bin_dir();
    let linked = || -> std::io::Result<bool> {
        let link = bin.join("rl");
        let Some(rl) = rl else {
            return match std::fs::remove_file(&link) {
                Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
                _ => Ok(false),
            };
        };
        std::fs::create_dir_all(&bin)?;
        // Made beside it and renamed over it, so a step starting meanwhile finds the old or the new.
        let new = bin.join(".rl.new");
        let _ = std::fs::remove_file(&new);
        std::os::unix::fs::symlink(rl, &new)?;
        std::fs::rename(&new, &link)?;
        Ok(true)
    };
    match linked() {
        Ok(true) => Some(bin),
        Ok(false) => None,
        Err(e) => {
            warn!(dir = %bin.display(), error = %e, "flows: rl not linked for the steps' PATH");
            None
        }
    }
}

/// A step's `PATH`: the flow's folder, `bin` when it holds the link, then the `PATH` given.
pub(super) fn step_path(flow_dir: &Path, bin: Option<&Path>, path: &OsStr) -> String {
    let mut parts = vec![flow_dir.to_string_lossy()];
    parts.extend(bin.map(Path::to_string_lossy));
    parts.push(path.to_string_lossy());
    parts.join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rl_is_linked_afresh_at_each_start_and_unlinked_when_there_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("home"));
        let link = home.bin_dir().join("rl");
        let (old, new) = (tmp.path().join("old-rl"), tmp.path().join("new-rl"));

        assert_eq!(link_rl(&home, Some(&old)), Some(home.bin_dir()));
        assert_eq!(std::fs::read_link(&link).unwrap(), old);
        assert_eq!(
            link_rl(&home, Some(&new)),
            Some(home.bin_dir()),
            "a link left is replaced"
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), new);
        let left: Vec<_> = std::fs::read_dir(home.bin_dir()).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "only the link: {left:?}");

        assert_eq!(link_rl(&home, None), None);
        assert!(std::fs::symlink_metadata(&link).is_err(), "no rl beside rld, no link");
        assert_eq!(link_rl(&home, None), None, "and none to take away");
    }

    #[test]
    fn a_steps_path_is_its_flow_then_rl_then_the_rest() {
        let (flow, bin) = (Path::new("/h/flows/f"), Path::new("/h/run/bin"));
        let path = OsStr::new("/usr/bin:/bin");
        assert_eq!(step_path(flow, Some(bin), path), "/h/flows/f:/h/run/bin:/usr/bin:/bin");
        assert_eq!(step_path(flow, None, path), "/h/flows/f:/usr/bin:/bin");
    }
}
