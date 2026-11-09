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

/// The messages waiting under `key`, oldest first, left where they are:
/// what a hook tells of. A file that does not read is passed over.
pub fn waiting(home: &Home, key: &str) -> Vec<Message> {
    let dir = home.mail_dir().join(component(key));
    release_stale(&dir);
    let mut files = mail_files(&dir).unwrap_or_default();
    files.sort();
    files
        .iter()
        .filter_map(|p| serde_json::from_slice(&std::fs::read(p).ok()?).ok())
        .collect()
}

/// Where the newest message a session was told of is kept: hidden in its
/// mail folder, where `take` never looks.
const TOLD: &str = ".told";

/// When the newest message a session (its first key, `key`) was last told
/// of was sent; None where it was told of none.
pub fn told(home: &Home, key: &str) -> Option<Timestamp> {
    let path = home.mail_dir().join(component(key)).join(TOLD);
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Keeps that the session `key` was told of the mail up to `at`.
pub fn set_told(home: &Home, key: &str, at: Timestamp) -> io::Result<()> {
    let dir = home.mail_dir().join(component(key));
    std::fs::create_dir_all(&dir)?;
    rl_core::atomic::write(&dir.join(TOLD), format!("{at}\n").as_bytes())
}

/// The message files in `dir`: none where it is not there.
fn mail_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => Ok(entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "json")
                    && p.file_name().is_some_and(|n| !n.to_string_lossy().starts_with('.'))
            })
            .collect()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
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
    release_stale(&dir);
    let mut files = mail_files(&dir)?;
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

/// Puts back, under its own name, every message in `dir` claimed by a
/// process that is no longer alive (it died between claiming and removing),
/// so it is delivered again. One claimed by a live process is its to finish;
/// one that is gone by the time it is renamed was won by another taker.
fn release_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((original, pid)) = claimed_by(&name) else {
            continue;
        };
        if alive(pid) {
            continue;
        }
        match std::fs::rename(entry.path(), dir.join(original)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("rl: could not release stale mail {}: {e}", entry.path().display()),
        }
    }
}

/// `.<original>.taken-<pid>-<n>` as (original, pid).
fn claimed_by(name: &str) -> Option<(&str, i32)> {
    let rest = name.strip_prefix('.')?;
    let (original, claim) = rest.rsplit_once(".taken-")?;
    let (pid, n) = claim.split_once('-')?;
    n.parse::<u64>().ok()?;
    let pid = pid.parse::<i32>().ok().filter(|p| *p > 0)?;
    (!original.is_empty()).then_some((original, pid))
}

/// Whether process `pid` lives; one we may not signal still does.
fn alive(pid: i32) -> bool {
    use nix::errno::Errno;
    !matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Err(Errno::ESRCH)
    )
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
    fn waiting_mail_is_read_and_left_and_what_was_told_is_kept_apart() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        assert!(waiting(&home, "a").is_empty());
        assert_eq!(told(&home, "a"), None);
        send(&home, "a", &msg("b", "a", "second", 20)).unwrap();
        send(&home, "a", &msg("c", "a", "first", 10)).unwrap();
        std::fs::write(home.mail_dir().join("a").join("0-x.json"), "{nope").unwrap();
        let w: Vec<_> = waiting(&home, "a").into_iter().map(|m| m.text).collect();
        assert_eq!(w, ["first", "second"], "the bad one passed over");
        let at = Timestamp::from_second(20).unwrap();
        set_told(&home, "a", at).unwrap();
        assert_eq!(told(&home, "a"), Some(at));
        assert_eq!(waiting(&home, "a").len(), 2, "left where it was");
        let got = take(&home, "a").unwrap();
        assert_eq!(got.messages.len(), 2, "what was told is no message");
        assert_eq!(told(&home, "a"), Some(at));
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

    #[test]
    fn mail_a_dead_taker_left_claimed_is_delivered_again() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        let sent = send(&home, "a", &msg("b", "a", "stuck", 5)).unwrap();
        let dir = sent.parent().unwrap().to_path_buf();
        let name = sent.file_name().unwrap().to_string_lossy().into_owned();
        // A pid past any the kernel hands out: nothing lives there.
        let stale = dir.join(format!(".{name}.taken-{}-0", i32::MAX));
        std::fs::rename(&sent, &stale).unwrap();
        assert_eq!(waiting(&home, "a").len(), 1, "waiting sweeps too");
        std::fs::rename(&sent, &stale).unwrap();
        let got = take(&home, "a").unwrap();
        let texts: Vec<_> = got.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["stuck"]);
        assert!(take(&home, "a").unwrap().messages.is_empty());
    }

    #[test]
    fn mail_a_live_taker_holds_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        let sent = send(&home, "a", &msg("b", "a", "mine", 5)).unwrap();
        let dir = sent.parent().unwrap().to_path_buf();
        let name = sent.file_name().unwrap().to_string_lossy().into_owned();
        let held = dir.join(format!(".{name}.taken-{}-0", std::process::id()));
        std::fs::rename(&sent, &held).unwrap();
        assert!(take(&home, "a").unwrap().messages.is_empty());
        assert!(waiting(&home, "a").is_empty());
        assert!(held.exists(), "still the taker's");
        assert!(!sent.exists());
    }
}
