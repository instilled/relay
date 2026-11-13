//! Mail between agents: `mail/<key>/<ts>-<pid>-<n>-<from>.json`, one message
//! per file. Sent by dropping a file, read by taking the files: sender and
//! receiver needn't overlap. A message read is moved to the folder's
//! `delivered/` only once the reply carrying it is out (`Claimed`), and kept
//! there for `KEPT` (`recent`). A key is what a session keeps for as long as
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
    sweep_dir(&dir, Timestamp::now());
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

/// Where a folder keeps the keys its session had before it was carried on
/// as a new one (`moved`), a key a line.
const WAS: &str = ".was";
/// Where a folder keeps the key its session was carried on under.
const MOVED: &str = ".moved";

/// The session `from` carries on as `to` (Claude's `--resume … --bg`
/// begins a new session, with a new conversation id): `to` reads what is
/// left under `from` and under every key `from` had (`with_was`), and what
/// is sent to `from` from now on goes to `to` (`now`).
pub fn moved(home: &Home, from: &str, to: &str) -> io::Result<()> {
    if from == to {
        return Ok(());
    }
    let mut was: Vec<String> = Vec::new();
    let all = std::iter::once(from.to_string())
        .chain(self::was(home, from))
        .chain(self::was(home, to));
    for k in all {
        if k != to && !was.contains(&k) {
            was.push(k);
        }
    }
    let dir = home.mail_dir().join(component(to));
    std::fs::create_dir_all(&dir)?;
    rl_core::atomic::write(&dir.join(WAS), (was.join("\n") + "\n").as_bytes())?;
    let old = home.mail_dir().join(component(from));
    std::fs::create_dir_all(&old)?;
    rl_core::atomic::write(&old.join(MOVED), format!("{to}\n").as_bytes())
}

/// The keys the session `key` had before it was carried on as itself.
pub fn was(home: &Home, key: &str) -> Vec<String> {
    std::fs::read_to_string(home.mail_dir().join(component(key)).join(WAS))
        .map(|t| t.lines().filter(|l| !l.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

/// `keys` and every key their sessions had before (`was`), each once.
pub fn with_was(home: &Home, keys: Vec<String>) -> Vec<String> {
    let mut all = Vec::new();
    for k in keys {
        let before = was(home, &k);
        for k in std::iter::once(k).chain(before) {
            if !all.contains(&k) {
                all.push(k);
            }
        }
    }
    all
}

/// The key the session `key` carries on under now: `key`, or where it was
/// carried on as another, that one's (followed a few times at most).
pub fn now(home: &Home, key: &str) -> String {
    let mut key = key.to_string();
    for _ in 0..16 {
        let next = std::fs::read_to_string(home.mail_dir().join(component(&key)).join(MOVED));
        match next.as_deref().map(str::trim) {
            Ok(n) if !n.is_empty() && n != key => key = n.to_string(),
            _ => break,
        }
    }
    key
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

/// Where a folder keeps the mail it delivered, under each message's own
/// name, for `KEPT` (`recent`). A folder, so no `.json` of a pending
/// message: `waiting` (the hooks) and `claim` never look in it.
pub const DELIVERED: &str = "delivered";

/// How long delivered mail is kept, from when it was delivered.
pub const KEPT: jiff::SignedDuration = jiff::SignedDuration::from_hours(7 * 24);

/// What `take` found: the messages oldest first, and the files it could not
/// read (left where they are).
#[derive(Debug, Default)]
pub struct Taken {
    pub messages: Vec<Message>,
    pub unreadable: Vec<PathBuf>,
}

/// Mail claimed by this process and not yet delivered: its messages, oldest
/// first, while their files are under names only this process knows. It is
/// `deliver`ed once the reply carrying it is out, else `put_back`. Dropped
/// without either (the process died), its claims are released by the next
/// `take` or `waiting` that finds this process gone (`release_stale`).
#[derive(Debug, Default)]
#[must_use = "deliver it once its reply is out, else put it back"]
pub struct Claimed {
    pub messages: Vec<Message>,
    /// The files that did not read, left pending.
    pub unreadable: Vec<PathBuf>,
    /// Each claimed file, and its pending name.
    files: Vec<(PathBuf, PathBuf)>,
}

impl Claimed {
    /// Adds what `other` claimed: the messages stay oldest first.
    pub fn merge(&mut self, other: Claimed) {
        self.messages.extend(other.messages);
        self.messages.sort_by_key(|m| m.at);
        self.unreadable.extend(other.unreadable);
        self.files.extend(other.files);
    }

    /// The reply is out: each file goes to its folder's `delivered/`, under
    /// its own name, kept from `now` (its modified time) for `KEPT`.
    pub fn deliver(self, now: Timestamp) {
        let when = std::time::SystemTime::from(now);
        for (claimed, pending) in self.files {
            let (Some(dir), Some(name)) = (pending.parent(), pending.file_name()) else {
                continue;
            };
            let to = dir.join(DELIVERED).join(name);
            let moved = std::fs::create_dir_all(to.parent().unwrap_or(dir))
                .and_then(|()| std::fs::File::options().write(true).open(&claimed))
                .and_then(|f| f.set_modified(when))
                .and_then(|()| std::fs::rename(&claimed, &to));
            if let Err(e) = moved {
                // Delivered all the same: it is not to come again.
                eprintln!("rl mcp: could not keep delivered mail {}: {e}", claimed.display());
                let _ = std::fs::remove_file(&claimed);
            }
        }
    }

    /// The reply did not go out: each file is pending again, for the next
    /// inbox.
    pub fn put_back(self) {
        for (claimed, pending) in self.files {
            if let Err(e) = std::fs::rename(&claimed, &pending) {
                eprintln!("rl mcp: could not put back mail {}: {e}", claimed.display());
            }
        }
    }
}

static CLAIMS: AtomicU64 = AtomicU64::new(0);

/// Claims the mail of `key`, renaming each file to a name only this call
/// knows: of two inboxes at once, each message goes to exactly one. One
/// already claimed is passed over; one that does not read is put back and
/// left. Delivered mail past `KEPT` is swept first.
pub fn claim(home: &Home, key: &str) -> io::Result<Claimed> {
    let dir = home.mail_dir().join(component(key));
    release_stale(&dir);
    sweep_dir(&dir, Timestamp::now());
    let mut files = mail_files(&dir)?;
    files.sort();
    let mut claimed = Claimed::default();
    for path in files {
        let Some(c) = claim_file(&path) else { continue };
        match std::fs::read(&c)
            .ok()
            .and_then(|b| serde_json::from_slice::<Message>(&b).ok())
        {
            Some(m) => {
                claimed.messages.push(m);
                claimed.files.push((c, path));
            }
            None => {
                let back = std::fs::rename(&c, &path).map_or(c, |()| path);
                claimed.unreadable.push(back);
            }
        }
    }
    Ok(claimed)
}

/// Claims the mail of `key` and delivers it at once.
pub fn take(home: &Home, key: &str) -> io::Result<Taken> {
    let mut c = claim(home, key)?;
    let taken = Taken {
        messages: std::mem::take(&mut c.messages),
        unreadable: std::mem::take(&mut c.unreadable),
    };
    c.deliver(Timestamp::now());
    Ok(taken)
}

/// The mail of `key` delivered within `KEPT` of `now`, newest last; nothing
/// is moved.
pub fn recent(home: &Home, key: &str, now: Timestamp) -> Vec<Message> {
    let dir = home.mail_dir().join(component(key)).join(DELIVERED);
    let mut got: Vec<Message> = mail_files(&dir)
        .unwrap_or_default()
        .iter()
        .filter(|p| delivered_at(p).is_some_and(|at| now.duration_since(at) <= KEPT))
        .filter_map(|p| serde_json::from_slice(&std::fs::read(p).ok()?).ok())
        .collect();
    got.sort_by_key(|m: &Message| m.at);
    got
}

/// Takes away the mail of `key` delivered more than `KEPT` before `now`.
pub fn sweep(home: &Home, key: &str, now: Timestamp) {
    sweep_dir(&home.mail_dir().join(component(key)), now);
}

fn sweep_dir(dir: &Path, now: Timestamp) {
    for p in mail_files(&dir.join(DELIVERED)).unwrap_or_default() {
        if delivered_at(&p).is_some_and(|at| now.duration_since(at) > KEPT) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// When a delivered file was delivered: its modified time.
fn delivered_at(p: &Path) -> Option<Timestamp> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    Timestamp::try_from(t).ok()
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
fn claim_file(path: &Path) -> Option<PathBuf> {
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

    #[test]
    fn a_session_carried_on_twice_reads_every_key_it_had_and_mail_to_any_goes_to_the_last() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().join("relay"));
        assert_eq!(now(&home, "conv-1"), "conv-1");
        moved(&home, "conv-1", "conv-2").unwrap();
        moved(&home, "conv-2", "conv-3").unwrap();
        assert_eq!(now(&home, "conv-1"), "conv-3");
        assert_eq!(now(&home, "conv-2"), "conv-3");
        assert_eq!(was(&home, "conv-3"), ["conv-2", "conv-1"]);
        assert_eq!(
            with_was(&home, vec!["conv-3".into(), "name".into()]),
            ["conv-3", "conv-2", "conv-1", "name"]
        );
        moved(&home, "x", "x").unwrap();
        assert_eq!(now(&home, "x"), "x", "never onto itself");
    }

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
        assert!(pending(&home, "a").is_empty());
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
        assert!(pending(&home, "k").is_empty());
        assert_eq!(recent(&home, "k", Timestamp::now()).len(), N, "each delivered once");
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

    /// The pending message files in the folder of `key`, by name.
    fn pending(home: &Home, key: &str) -> Vec<String> {
        let mut v: Vec<String> = mail_files(&home.mail_dir().join(component(key)))
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn texts(ms: &[Message]) -> Vec<&str> {
        ms.iter().map(|m| m.text.as_str()).collect()
    }

    #[test]
    fn claimed_mail_is_moved_to_delivered_only_when_delivered() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        let sent = send(&home, "a", &msg("b", "a", "hi", 5)).unwrap();
        let name = sent.file_name().unwrap().to_owned();
        let c = claim(&home, "a").unwrap();
        assert_eq!(texts(&c.messages), ["hi"]);
        assert!(pending(&home, "a").is_empty(), "claimed: no other inbox gets it");
        let delivered = home.mail_dir().join("a").join(DELIVERED);
        assert!(!delivered.join(&name).exists(), "not delivered before the reply is out");
        c.deliver(Timestamp::now());
        assert!(delivered.join(&name).exists(), "kept under its own name");
        assert!(claim(&home, "a").unwrap().messages.is_empty());
        assert_eq!(texts(&recent(&home, "a", Timestamp::now())), ["hi"]);
    }

    #[test]
    fn claimed_mail_put_back_is_delivered_by_the_next_take() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        let sent = send(&home, "a", &msg("b", "a", "again", 5)).unwrap();
        let c = claim(&home, "a").unwrap();
        c.put_back();
        assert!(sent.exists(), "under its pending name");
        assert!(recent(&home, "a", Timestamp::now()).is_empty());
        assert_eq!(texts(&take(&home, "a").unwrap().messages), ["again"]);
    }

    #[test]
    fn recent_is_the_delivered_mail_of_the_last_seven_days_newest_last_and_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        let day = jiff::SignedDuration::from_hours(24);
        // Two days ago: the sweeps of the claims, at the real now, keep all.
        let t0 = Timestamp::now() - day * 2;
        send(&home, "a", &msg("b", "a", "old", 1)).unwrap();
        claim(&home, "a").unwrap().deliver(t0);
        send(&home, "a", &msg("b", "a", "late", 3)).unwrap();
        send(&home, "a", &msg("b", "a", "early", 2)).unwrap();
        claim(&home, "a").unwrap().deliver(t0 + day * 3);
        send(&home, "a", &msg("b", "a", "waiting", 4)).unwrap();
        let now = t0 + day * 8;
        assert_eq!(texts(&recent(&home, "a", now)), ["early", "late"], "old is past 7 days");
        assert_eq!(texts(&waiting(&home, "a")), ["waiting"], "left pending");
        assert_eq!(recent(&home, "a", now).len(), 2, "recent moves nothing");
        // The sweep takes what is past 7 days away; the rest stays.
        sweep(&home, "a", now);
        let dir = home.mail_dir().join("a").join(DELIVERED);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        sweep(&home, "a", t0 + day * 11);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(texts(&waiting(&home, "a")), ["waiting"], "pending mail is never swept");
    }

    #[test]
    fn delivered_mail_is_not_waiting() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path().to_path_buf());
        send(&home, "a", &msg("b", "a", "read", 5)).unwrap();
        take(&home, "a").unwrap();
        assert!(waiting(&home, "a").is_empty(), "the hooks count only pending mail");
        send(&home, "a", &msg("b", "a", "new", 6)).unwrap();
        assert_eq!(texts(&waiting(&home, "a")), ["new"]);
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
