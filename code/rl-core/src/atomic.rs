//! One way to write a file so a reader never sees half of it: write
//! `.<file name>.<pid>.<n>.tmp` beside it (unique per write, so writers of the
//! same path never share a tmp), fsync, rename over the target.

use serde::Serialize;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
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
        let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "no tmp left: {left:?}");
    }

    #[test]
    fn concurrent_writers_of_one_path_never_show_a_reader_half_a_file() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.json");
        write_json(&p, &serde_json::json!({"w": 0, "pad": "x".repeat(4096)})).unwrap();
        let done = AtomicBool::new(false);
        std::thread::scope(|s| {
            let reader = s.spawn(|| {
                let mut reads = 0;
                while !done.load(Ordering::Relaxed) {
                    let bytes = std::fs::read(&p).unwrap();
                    serde_json::from_slice::<serde_json::Value>(&bytes).expect("a whole file");
                    reads += 1;
                }
                reads
            });
            let writers: Vec<_> = (0..8)
                .map(|w| {
                    let p = &p;
                    s.spawn(move || {
                        for i in 0..100 {
                            write_json(p, &serde_json::json!({"w": w, "i": i, "pad": "y".repeat(4096)})).unwrap();
                        }
                    })
                })
                .collect();
            let wrote: Vec<_> = writers.into_iter().map(|w| w.join()).collect();
            done.store(true, Ordering::Relaxed);
            let reads = reader.join();
            assert!(wrote.iter().all(Result::is_ok), "a writer failed");
            assert!(reads.unwrap() > 0);
        });
        let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "no tmp left: {left:?}");
    }

    #[test]
    fn json_is_pretty_with_a_trailing_newline() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.json");
        write_json(&p, &serde_json::json!({"k": 1})).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\n  \"k\": 1\n}\n");
    }
}
