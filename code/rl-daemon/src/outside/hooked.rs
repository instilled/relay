//! The sessions agents' hooks tell of: Codex's and Gemini's, each of which
//! calls `rl hook <agent> <Event>` from the user-level hooks `rl setup`
//! adds (`rl_detect::user_hooks`). Nothing is asked: a session is kept as
//! its last event left it, and goes when it ends.
//!
//! All are open in a terminal (`interactive`, nothing to attach to). One
//! that ends without saying so — killed, or a hook that did not run — goes
//! once its process is gone, where the hook found it, checked on each of
//! `serve`'s ticks; or, where it did not, after `STALE` with no word (and
//! even with a process, after `QUIET`: the process found may not be it).
//! A session started in a process that held another (`/clear`, `/new`) is
//! that process's only one: the other goes.
//!
//! Codex runs its hooks in the background, several at once, so they can
//! arrive out of order. An ended session is remembered for `ENDED`, and an
//! event of its that arrives in that time — one sent before the end — does
//! not bring it back; only a start does (`codex resume` keeps the id). A
//! tool done heard after its turn's end does not make it busy again.

use rl_detect::user_hooks::{self, Told};
use rl_proto::methods::{Outside, OutsideHookParams};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a session with no known process stands with no word from it.
pub const STALE: Duration = Duration::from_secs(12 * 3600);
/// How long one with a known process does: long, as that process lives.
pub const QUIET: Duration = Duration::from_secs(48 * 3600);
/// How long an ended session is remembered as ended.
pub const ENDED: Duration = Duration::from_secs(60);

struct Heard {
    session: Outside,
    /// When its hook was last heard.
    at: Instant,
    /// Its conversation's file, as the last hook that named one did.
    transcript: Option<PathBuf>,
}

/// One session as last heard: when, and its conversation's file.
#[derive(Debug, Clone)]
pub struct Last {
    pub session: Outside,
    pub heard: Instant,
    pub transcript: Option<PathBuf>,
}

#[derive(Default)]
struct Kept {
    heard: HashMap<String, Heard>,
    /// Ended sessions, by key, and when.
    ended: HashMap<String, Instant>,
}

#[derive(Default)]
pub(crate) struct Hooked {
    kept: Mutex<Kept>,
}

/// What a hook said, for `Outsiders::told`.
pub(crate) enum Said {
    /// The session as it stands now, for it to settle and keep; and the
    /// sessions it replaces in its process, gone.
    Stands { session: Box<Outside>, gone: Vec<String> },
    /// It is over.
    Ended(String),
    /// Late word of a session that ended: nothing.
    Late,
}

impl Hooked {
    /// The session as `p` leaves it, or that it ended: the one known,
    /// changed by the event, or a new one. Not kept yet: `keep` keeps it,
    /// once settled. A start heard of a session already known says
    /// nothing new — codex runs its hooks in the background, so a start
    /// may come after the prompt that followed it.
    pub fn hear(&self, p: OutsideHookParams) -> Said {
        let key = format!("o:{}:{}", p.agent, p.session_id);
        let told = user_hooks::told(&p.agent, &p.event, p.detail.as_deref());
        let start = p.event == "SessionStart";
        let mut k = self.kept.lock().unwrap();
        if told == Told::Ended {
            k.heard.remove(&key);
            k.ended.insert(key.clone(), Instant::now());
            return Said::Ended(key);
        }
        if k.ended.contains_key(&key) {
            if !start {
                return Said::Late;
            }
            k.ended.remove(&key);
        }
        // A start in a process that held another session: that one is over.
        let mut gone = Vec::new();
        if start && let Some(pid) = p.pid {
            gone = k
                .heard
                .iter()
                .filter(|(other, h)| **other != key && h.session.agent == p.agent && h.session.pid == Some(pid))
                .map(|(other, _)| other.clone())
                .collect();
            let now = Instant::now();
            for g in &gone {
                k.heard.remove(g);
                k.ended.insert(g.clone(), now);
            }
        }
        let known = k.heard.get(&key).map(|h| h.session.clone());
        drop(k);
        let mut s = known.clone().unwrap_or_else(|| Outside {
            agent: p.agent.clone(),
            session_id: p.session_id.clone(),
            id: None,
            name: p
                .cwd
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.session_id.clone()),
            cwd: p.cwd.clone(),
            kind: "interactive".into(),
            status: Outside::IDLE.into(),
            state: None,
            pid: None,
            started_at: start.then(jiff::Timestamp::now),
            waiting_for: None,
            run: None,
            new: false,
            workspace: None,
            state_at: None,
            was: None,
        });
        // A tool done ends a wait or keeps a turn going; late, it never
        // wakes one that has gone idle.
        let wakes =
            known.as_ref().is_some_and(|k| k.status == Outside::IDLE) && user_hooks::carries_on(&p.agent, &p.event);
        if let Told::Stands { status, waiting_for } = told
            && !(known.is_some() && start)
            && !wakes
        {
            s.status = status.into();
            s.waiting_for = waiting_for.map(Into::into);
        }
        if p.pid.is_some() {
            s.pid = p.pid;
        }
        Said::Stands {
            session: Box::new(s),
            gone,
        }
    }

    /// Keeps a session as it now stands, heard just now, with its
    /// conversation's file where this word named one (else the one known)
    /// — unless it ended while it was being settled. Whether it was kept.
    pub fn keep(&self, s: Outside, transcript: Option<PathBuf>) -> bool {
        let key = s.key();
        let mut k = self.kept.lock().unwrap();
        if k.ended.contains_key(&key) {
            return false;
        }
        let transcript = transcript.or_else(|| k.heard.get(&key).and_then(|h| h.transcript.clone()));
        k.heard.insert(
            key,
            Heard {
                session: s,
                at: Instant::now(),
                transcript,
            },
        );
        true
    }

    /// One session as last heard, by its key.
    pub fn last(&self, key: &str) -> Option<Last> {
        self.kept.lock().unwrap().heard.get(key).map(|h| Last {
            session: h.session.clone(),
            heard: h.at,
            transcript: h.transcript.clone(),
        })
    }

    /// Every session as last heard.
    pub fn latest(&self) -> Vec<Outside> {
        let mut list: Vec<Outside> = self
            .kept
            .lock()
            .unwrap()
            .heard
            .values()
            .map(|h| h.session.clone())
            .collect();
        list.sort_by_key(Outside::key);
        list
    }

    /// Lets go of the sessions that ended without a word: a process gone
    /// (`alive` says whether a pid still is), nothing heard for `QUIET`,
    /// or no process known and nothing heard for `STALE`. Their keys. And
    /// forgets the ends older than `ENDED`.
    pub fn prune(&self, now: Instant, alive: impl Fn(u32) -> bool) -> Vec<String> {
        let mut gone = Vec::new();
        let mut k = self.kept.lock().unwrap();
        k.heard.retain(|key, h| {
            let quiet = now.saturating_duration_since(h.at);
            let stands = match h.session.pid {
                Some(pid) => quiet < QUIET && alive(pid),
                None => quiet < STALE,
            };
            if !stands {
                gone.push(key.clone());
            }
            stands
        });
        k.ended.retain(|_, at| now.saturating_duration_since(*at) < ENDED);
        gone
    }

    #[cfg(test)]
    pub fn age(&self, key: &str, by: Duration) {
        let mut k = self.kept.lock().unwrap();
        if let Some(h) = k.heard.get_mut(key) {
            h.at -= by;
        }
        if let Some(at) = k.ended.get_mut(key) {
            *at -= by;
        }
    }
}

/// Whether a process lives: signal 0 reaches it, or would but for
/// permission.
pub fn alive(pid: u32) -> bool {
    use nix::errno::Errno;
    let Ok(pid) = i32::try_from(pid) else { return false };
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
        Ok(()) => true,
        Err(e) => e == Errno::EPERM,
    }
}
