//! A flow folder's version: what a yes is given to, so changing a script asks
//! again as changing `flow.md` does.

use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// sha256, in hex, over each file's path (relative to the folder) and its
/// bytes, taken in path order. The caller leaves out `runs/`.
pub fn folder(files: &[(PathBuf, Vec<u8>)]) -> String {
    let mut sorted: Vec<&(PathBuf, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = Sha256::new();
    for (path, bytes) in sorted {
        // Each length said first, so no file's end can pass for the next one's start.
        let name = path.as_os_str().as_encoded_bytes();
        h.update((name.len() as u64).to_be_bytes());
        h.update(name);
        h.update((bytes.len() as u64).to_be_bytes());
        h.update(bytes);
    }
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[(&str, &str)]) -> Vec<(PathBuf, Vec<u8>)> {
        list.iter()
            .map(|(p, t)| (PathBuf::from(p), t.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn the_order_files_are_read_in_does_not_matter() {
        let a = folder(&files(&[("flow.md", "x"), ("beat.sh", "date")]));
        let b = folder(&files(&[("beat.sh", "date"), ("flow.md", "x")]));
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn a_script_changed_renamed_or_added_changes_it() {
        let base = folder(&files(&[("flow.md", "x"), ("beat.sh", "date")]));
        assert_ne!(base, folder(&files(&[("flow.md", "x"), ("beat.sh", "date -u")])));
        assert_ne!(base, folder(&files(&[("flow.md", "x"), ("pulse.sh", "date")])));
        assert_ne!(
            base,
            folder(&files(&[("flow.md", "x"), ("beat.sh", "date"), ("lib/a.sh", "")]))
        );
    }

    #[test]
    fn bytes_cannot_move_between_a_name_and_a_file() {
        assert_ne!(folder(&files(&[("a", "bc")])), folder(&files(&[("ab", "c")])));
        assert_ne!(
            folder(&files(&[("a", "x"), ("b", "")])),
            folder(&files(&[("a", ""), ("b", "x")]))
        );
    }
}
