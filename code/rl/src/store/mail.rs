//! Mail between agents: `mail/<address>/<ts>-<from>.json`, one message per
//! file. Sent by dropping a file, read by taking the files: sender and
//! receiver needn't overlap.

use jiff::Timestamp;
use rl_core::Home;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub from: String,
    pub to: String,
    pub text: String,
    pub at: Timestamp,
}

/// An address as one path component: anything but letters, digits, `.`, `_`
/// and `-` becomes `_`, and a leading `.` too (so it is never hidden nor `..`).
pub fn component(address: &str) -> String {
    let s: String = address
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    match s.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None if s.is_empty() => "_".into(),
        None => s,
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Drops `m` into the recipient's folder, atomically. Names sort oldest first.
pub fn send(home: &Home, m: &Message) -> io::Result<PathBuf> {
    let dir = home.mail_dir().join(component(&m.to));
    std::fs::create_dir_all(&dir)?;
    let file = format!(
        "{:020}-{:04}-{}.json",
        m.at.as_nanosecond(),
        COUNTER.fetch_add(1, Ordering::Relaxed) % 10_000,
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

/// Reads and removes the address's mail.
pub fn take(home: &Home, address: &str) -> io::Result<Taken> {
    let dir = home.mail_dir().join(component(address));
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
        match std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Message>(&b).ok())
        {
            Some(m) => {
                std::fs::remove_file(&path)?;
                taken.messages.push(m);
            }
            None => taken.unreadable.push(path),
        }
    }
    Ok(taken)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        send(&home, &msg("b", "a", "second", 20)).unwrap();
        send(&home, &msg("c", "a", "first", 10)).unwrap();
        send(&home, &msg("c", "z", "other", 10)).unwrap();
        let got = take(&home, "a").unwrap();
        let texts: Vec<_> = got.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["first", "second"]);
        assert!(take(&home, "a").unwrap().messages.is_empty());
        assert_eq!(take(&home, "z").unwrap().messages.len(), 1);
    }

    #[test]
    fn an_address_cannot_leave_the_mail_folder_and_a_bad_file_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        let p = send(&home, &msg("../x", "../../y", "hi", 1)).unwrap();
        assert!(p.starts_with(home.mail_dir()));
        assert_eq!(component(".."), "_.");
        let dir = home.mail_dir().join("a");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("1-x.json"), "{nope").unwrap();
        let got = take(&home, "a").unwrap();
        assert!(got.messages.is_empty());
        assert_eq!(got.unreadable.len(), 1);
        assert!(got.unreadable[0].exists());
    }
}
