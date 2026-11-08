//! Mail between agents: `mail/<key>/<ts>-<pid>-<n>-<from>.json`, one message
//! per file. Sent by dropping a file, read by taking the files: sender and
//! receiver needn't overlap. A key is what a session keeps for as long as
//! it lives (its name, or its conversation id), never an address that can
//! change when names clash.

use jiff::Timestamp;
use rl_core::Home;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub from: String,
    pub to: String,
    pub text: String,
    pub at: Timestamp,
}

/// A key as one path component, one to one: lowercase letters, digits and
/// `-` stay, every other byte is `%` and two hex digits. So no two keys
/// share a folder (`API` and `api`, `a b` and `a_b` included), even on a
/// file system that folds case, and none is hidden, `..` or holds a `/`.
pub fn component(key: &str) -> String {
    if key.is_empty() {
        return "%".into();
    }
    let mut out = String::with_capacity(key.len());
    for b in key.bytes() {
        if b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02x}"));
        }
    }
    out
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Drops `m` into the folder of `key`, atomically. Names sort oldest first,
/// and no two senders' names are the same.
pub fn send(home: &Home, key: &str, m: &Message) -> io::Result<PathBuf> {
    let dir = home.mail_dir().join(component(key));
    std::fs::create_dir_all(&dir)?;
    let file = format!(
        "{:020}-{}-{}-{}.json",
        m.at.as_nanosecond(),
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        component(&m.from)
    );
    let path = dir.join(file);
    rl_core::atomic::write_json(&path, m)?;
    Ok(path)
}

/// What `take` found: the messages oldest first, and the files it could not
/// read (left where they are).
#[derive(Debug, Default)]
pub struct Taken {
    pub messages: Vec<Message>,
    pub unreadable: Vec<PathBuf>,
}

static CLAIMS: AtomicU64 = AtomicU64::new(0);

/// Reads and removes the mail of `key`. Each file is claimed first, by
/// renaming it to a name only this call knows: of two inboxes taking at
/// once, each message goes to exactly one. One already claimed is passed
/// over; one that does not read is put back and left.
pub fn take(home: &Home, key: &str) -> io::Result<Taken> {
    let dir = home.mail_dir().join(component(key));
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "json")
                    && p.file_name().is_some_and(|n| !n.to_string_lossy().starts_with('.'))
            })
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Taken::default()),
        Err(e) => return Err(e),
    };
    files.sort();
    let mut taken = Taken::default();
    for path in files {
        let Some(claimed) = claim(&path) else { continue };
        match std::fs::read(&claimed)
            .ok()
            .and_then(|b| serde_json::from_slice::<Message>(&b).ok())
        {
            Some(m) => {
                if let Err(e) = std::fs::remove_file(&claimed) {
                    eprintln!("rl mcp: could not remove taken mail {}: {e}", claimed.display());
                }
                taken.messages.push(m);
            }
            None => {
                let back = std::fs::rename(&claimed, &path).map_or(claimed, |()| path);
                taken.unreadable.push(back);
            }
        }
    }
    Ok(taken)
}

/// `path` renamed to `.<name>.taken-<pid>-<n>` beside it, where no other
/// take looks; None when another took it first (or it cannot be renamed).
fn claim(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy();
    let claimed = path.with_file_name(format!(
        ".{name}.taken-{}-{}",
        std::process::id(),
        CLAIMS.fetch_add(1, Ordering::Relaxed)
    ));
    match std::fs::rename(path, &claimed) {
        Ok(()) => Some(claimed),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!("rl mcp: could not take mail {}: {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn msg(from: &str, to: &str, text: &str, s: i64) -> Message {
        Message {
            from: from.into(),
            to: to.into(),
            text: text.into(),
            at: Timestamp::from_second(s).unwrap(),
        }
    }

    #[test]
    fn mail_is_taken_oldest_first_and_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        send(&home, "a", &msg("b", "a", "second", 20)).unwrap();
        send(&home, "a", &msg("c", "a", "first", 10)).unwrap();
        send(&home, "z", &msg("c", "z", "other", 10)).unwrap();
        let got = take(&home, "a").unwrap();
        let texts: Vec<_> = got.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["first", "second"]);
        assert!(take(&home, "a").unwrap().messages.is_empty());
        assert_eq!(take(&home, "z").unwrap().messages.len(), 1);
        assert_eq!(std::fs::read_dir(home.mail_dir().join("a")).unwrap().count(), 0);
    }

    #[test]
    fn a_key_cannot_leave_the_mail_folder_and_a_bad_file_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        let p = send(&home, "../../y", &msg("../x", "../../y", "hi", 1)).unwrap();
        assert!(
            p.parent().unwrap().parent() == Some(home.mail_dir().as_path()),
            "{}",
            p.display()
        );
        assert_eq!(component(".."), "%2e%2e");
        assert_eq!(component("a/b"), "a%2fb");
        let dir = home.mail_dir().join("a");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("1-x.json"), "{nope").unwrap();
        let got = take(&home, "a").unwrap();
        assert!(got.messages.is_empty());
        assert_eq!(got.unreadable, [dir.join("1-x.json")]);
        assert!(got.unreadable[0].exists(), "put back where it was");
    }

    #[test]
    fn keys_that_differ_keep_folders_that_differ() {
        let pairs = [
            ("a b", "a_b"),
            ("API", "api"),
            ("a.b", "a_b"),
            ("a%2fb", "a/b"),
            ("", "%"),
        ];
        for (x, y) in pairs {
            assert_ne!(component(x), component(y), "{x:?} and {y:?}");
        }
        assert_eq!(component("api-claude-2"), "api-claude-2");
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        send(&home, "API", &msg("x", "API", "upper", 1)).unwrap();
        send(&home, "api", &msg("x", "api", "lower", 2)).unwrap();
        assert_eq!(take(&home, "API").unwrap().messages[0].text, "upper");
        assert_eq!(take(&home, "api").unwrap().messages[0].text, "lower");
    }

    #[test]
    fn two_inboxes_at_once_get_each_message_exactly_once() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        const N: usize = 400;
        for i in 0..N {
            send(&home, "k", &msg("x", "k", &i.to_string(), i as i64 + 1)).unwrap();
        }
        let start = std::sync::Barrier::new(2);
        let got: Vec<Vec<String>> = std::thread::scope(|s| {
            let readers: Vec<_> = (0..2)
                .map(|_| {
                    s.spawn(|| {
                        start.wait();
                        let mut texts = Vec::new();
                        loop {
                            let t = take(&home, "k").unwrap();
                            assert!(t.unreadable.is_empty(), "{:?}", t.unreadable);
                            if t.messages.is_empty() {
                                return texts;
                            }
                            texts.extend(t.messages.into_iter().map(|m| m.text));
                        }
                    })
                })
                .collect();
            readers.into_iter().map(|r| r.join().unwrap()).collect()
        });
        let all: Vec<&String> = got.iter().flatten().collect();
        assert_eq!(all.len(), N, "no message twice, none lost");
        let unique: BTreeSet<&String> = all.iter().copied().collect();
        assert_eq!(unique.len(), N);
        assert_eq!(std::fs::read_dir(home.mail_dir().join("k")).unwrap().count(), 0);
    }
}
