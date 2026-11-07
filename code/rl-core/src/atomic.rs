//! One way to write a file so a reader never sees half of it: write
//! `<file name>.tmp` beside it, fsync, rename over the target.

use serde::Serialize;
use std::io::{self, Write};
use std::path::Path;

pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

/// Pretty JSON with a trailing newline.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_replaces_the_content_and_leaves_no_tmp() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.json");
        write(&p, b"one").unwrap();
        write(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert!(!tmp.path().join("a.json.tmp").exists());
    }

    #[test]
    fn json_is_pretty_with_a_trailing_newline() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.json");
        write_json(&p, &serde_json::json!({"k": 1})).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\n  \"k\": 1\n}\n");
    }
}
