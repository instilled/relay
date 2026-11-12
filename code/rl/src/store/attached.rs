//! Which of the harnesses' own sessions you are attached to from the list
//! right now: one file each under `attached/`, the list's pid in it, there
//! while the attach lasts (`Mark`). What tells a session of mail asks it
//! (`is`), so one you are in is left to its hooks — never stopped under you.
//! A file whose pid is gone (a list that was killed) says nothing.

use rl_core::Home;
use std::path::PathBuf;

/// The file that says you are attached to `harness`'s session `id`.
fn path(home: &Home, harness: &str, id: &str) -> PathBuf {
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    home.attached_dir().join(format!("{}-{}", safe(harness), safe(id)))
}

/// You, attached: the file is there till this is dropped.
pub struct Mark(PathBuf);

impl Drop for Mark {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Marks `harness`'s session `id` as one you are attached to. None where the
/// mark cannot be written: the attach goes on without it.
pub fn mark(home: &Home, harness: &str, id: &str) -> Option<Mark> {
    let p = path(home, harness, id);
    std::fs::create_dir_all(p.parent()?).ok()?;
    rl_core::atomic::write(&p, std::process::id().to_string().as_bytes()).ok()?;
    Some(Mark(p))
}

/// Whether you are attached to `harness`'s session `id` from a list that
/// is still running.
pub fn is(home: &Home, harness: &str, id: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path(home, harness, id)) else {
        return false;
    };
    text.trim()
        .parse::<i32>()
        .is_ok_and(|pid| nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_says_you_are_attached_while_it_lasts_and_a_dead_lists_says_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        assert!(!is(&home, "claude", "ab12"));
        let you = mark(&home, "claude", "ab12").unwrap();
        assert!(is(&home, "claude", "ab12"));
        assert!(!is(&home, "claude", "other"));
        drop(you);
        assert!(!is(&home, "claude", "ab12"), "let go of");
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let pid = gone.id();
        gone.wait().unwrap();
        std::fs::write(path(&home, "claude", "ab12"), pid.to_string()).unwrap();
        assert!(!is(&home, "claude", "ab12"), "a list that is gone");
    }
}
