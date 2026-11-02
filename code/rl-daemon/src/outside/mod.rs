//! Agents' own sessions, for the list: the sessions an agent CLI runs that
//! are not Relay's runs, kept in memory so a digest only reads what was last
//! heard and never waits on an agent.
//!
//! Every agent's are told, by the hooks `rl setup` adds to their
//! user-level files, and kept in one store (`hooked`): each agent whose
//! profile has hooks (`sessions.hooks`). An agent that lists its own
//! sessions (`sessions.list`: Claude's, `claude agents --json`) has them
//! filled in by asking it too (`lister`), only with a reason and only while
//! something is showing the list — a panel's digest (`watching`) within
//! the last minute, the
//! way badges are: a hook cannot say whether a session is in the agent's
//! background, nor tell of one that began before it was there.
//! What is heard is settled here before it is kept: each session's project
//! folder, resolved once per session as a run's would be
//! (`workspace::filed_under`), and when it came to stand as it does
//! (`state_at`), which brings a hidden one back once it needs you. Neither
//! is worked out on a digest's path.
//!
//! The sessions that are Relay's own runs are left out of the list, as a
//! digest reads it (`not_relays`): a run is tagged `<agent>-session:<id>`
//! when the agent's `SessionStart` hook reaches it, and a session whose pid
//! is a live run's agent is Relay's as well. Every agent the daemon starts
//! itself — an assistant's `claude -p`, a flow's agent step, a resumed or
//! moved session — is such a run, started by the supervisor with the hooks.
//! Two still list: one left open in its terminal at work there again, and
//! one of Claude's background sessions a run that has ended was sent to
//! (`went_from`).

pub mod claude;
pub mod hooked;
pub mod lister;

pub use lister::Polling;

use anyhow::{Result, bail};
use jiff::Timestamp;
use rl_detect::Registry;
use rl_ledger::Ledger;
use rl_proto::methods::{DigestItem, Outside, WentFrom};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// How long after a digest the list counts as showing.
const SHOWING: Duration = Duration::from_secs(60);

/// The tag on a run that carries one of `agent`'s sessions on, before the
/// session's id: `claude-session:`, `codex-session:`, `gemini-session:`.
pub fn tag(agent: &str) -> String {
    format!("{agent}-session:")
}

/// The tag on a run that carries on one of `agent`'s sessions which stays
/// open in its terminal, before the session's id: `claude-left-open:`. That
/// one is still the user's, and shows again while it works or waits there.
pub fn left_open(agent: &str) -> String {
    format!("{agent}-left-open:")
}

/// The tag that says which process held it in that terminal, where known,
/// before `<session id>:<pid>`: `claude-left-open-pid:`. Only that process
/// is the terminal's: another listed under the session is the run's own.
pub fn left_open_pid(agent: &str) -> String {
    format!("{agent}-left-open-pid:")
}

/// What has been settled of a session, by its key: kept from one hearing to
/// the next, and forgotten once it is gone (`forget_one`).
struct Settled {
    /// None while it could not be filed: the panel guesses, and the next
    /// hearing tries again.
    workspace: Option<PathBuf>,
    /// Whether it wanted you when last heard (`Outside::wants_you`).
    wanted: bool,
    /// Since when it has (or has not) wanted you.
    at: Timestamp,
}

pub struct Outsiders {
    /// What each agent's events say, and which agents list their sessions.
    registry: Arc<Registry>,
    /// The agents asked for their sessions, and when; none are while
    /// asking is off.
    listers: Vec<lister::Lister>,
    /// How often `serve` wakes.
    tick: Duration,
    /// The sessions hooks tell of, and asks fill in.
    hooked: hooked::Hooked,
    settled: Mutex<HashMap<String, Settled>>,
    /// The sessions being carried on now, by key (`moving`).
    moving: Mutex<HashSet<String>>,
    /// When a digest last asked for the list.
    wanted: Mutex<Option<Instant>>,
    wake: Notify,
}

impl Outsiders {
    /// `polling`: when the agents of the `registry` that list their own
    /// sessions are asked; none, and none ever is, while the store itself
    /// still serves: every agent's sessions are told by its hooks.
    pub fn new(registry: Arc<Registry>, polling: Option<Polling>) -> Self {
        Self {
            tick: polling.as_ref().map_or(lister::TICK, |p| p.tick),
            listers: polling.map_or_else(Vec::new, |p| lister::Lister::all(&registry, &p)),
            registry,
            hooked: hooked::Hooked::default(),
            settled: Mutex::default(),
            moving: Mutex::default(),
            wanted: Mutex::default(),
            wake: Notify::new(),
        }
    }

    /// Every agent's sessions as last heard, Relay's own among them: the
    /// digest leaves those out (`not_relays`). Where `watching` (the panel's
    /// digest), counts as the list showing: the first time in a while,
    /// each agent that lists its sessions is asked now, in the background,
    /// and a later digest carries what it heard. A one-off digest only
    /// reads.
    pub fn latest(&self, watching: bool) -> Vec<Outside> {
        if !watching {
            return self.hooked.latest();
        }
        let mut w = self.wanted.lock().unwrap();
        let idle = w.is_none_or(|w| w.elapsed() > SHOWING);
        *w = Some(Instant::now());
        if idle && !self.listers.is_empty() {
            for l in &self.listers {
                l.asker.want();
            }
            self.wake.notify_one();
        }
        drop(w);
        self.hooked.latest()
    }

    /// Holds the session `key` while it is carried on, so a second ask to
    /// carry it on (another panel, or enter and `/import` at once) is turned
    /// away rather than starting a second run on one conversation. None
    /// while it is held already; let go when the hold is dropped.
    pub fn moving(&self, key: &str) -> Option<Moving<'_>> {
        self.moving.lock().unwrap().insert(key.to_string()).then(|| Moving {
            store: self,
            key: key.to_string(),
        })
    }

    /// One session a hook told of, as last heard: by its agent and id.
    pub fn told_of(&self, agent: &str, session_id: &str) -> Option<hooked::Last> {
        self.hooked.last(&format!("o:{agent}:{session_id}"))
    }

    /// What an agent's hook told of one of its sessions, outside any run of
    /// that agent's: kept, settled as any session is, or let go once it
    /// ended. A word from under a run that is the agent's own (Claude's
    /// user-level hook, in a live Claude run of Relay's) is that run's,
    /// which its own hooks have said: let go. Of one of an agent that lists its
    /// sessions that the last ask did not list, an ask is called for, to
    /// fill it in. Answers whether the session is kept with no process
    /// known, which the hook then finds.
    pub async fn told(&self, p: rl_proto::methods::OutsideHookParams, ledger: &Ledger) -> Result<bool> {
        let registry = self.registry.clone();
        let Some(hooks) = registry.user_hooks(&p.agent) else {
            bail!("`{}` sessions are not told by hooks", p.agent);
        };
        if p.session_id.is_empty() || !p.cwd.is_absolute() {
            bail!("a session needs its id and a whole folder");
        }
        if let Some(id) = p.run.clone() {
            let run = ledger.call_read(move |c| rl_ledger::runs::get(c, &id)).await?;
            // Only while that run lives: a process it left behind (Claude's
            // background supervisor, should it keep the environment it was
            // started in) carries its `RELAY_RUN` on, and the sessions it
            // hosts later are not the run's. A late word of the ended run's
            // own session is harmless: its end only ends it, and any other
            // is hidden by the run's tag (`not_relays`).
            if run.is_some_and(|r| !r.state.is_terminal() && !rl_detect::user_hooks::foreign(&r, &p.agent, &registry)) {
                return Ok(false);
            }
        }
        let transcript = p.transcript_path.clone().filter(|t| t.is_absolute());
        let mut wants_pid = false;
        match self.hooked.hear(p, hooks) {
            hooked::Said::Ended(key) => self.forget_one(&key),
            hooked::Said::Late => {}
            hooked::Said::Stands {
                mut session,
                gone,
                stood,
            } => {
                for key in &gone {
                    self.forget_one(key);
                }
                self.settle_one(&mut session, ledger).await;
                let key = session.key();
                let agent = session.agent.clone();
                // Ended while it was settled: the end stands.
                if !self.hooked.keep(*session, transcript, stood) {
                    self.forget_one(&key);
                } else {
                    wants_pid = self.hooked.last(&key).is_some_and(|l| l.session.pid.is_none());
                    if let Some(l) = self.listers.iter().find(|l| l.agent == agent)
                        && l.asker.told_of(&key)
                    {
                        self.wake.notify_one();
                    }
                }
            }
        }
        Ok(wants_pid)
    }

    /// Lets go of the told sessions that ended without saying so.
    fn prune_told(&self) {
        for key in self.hooked.prune(Instant::now(), hooked::alive, |p| p.is_dir()) {
            debug!(session = %key, "outside: a told session is gone");
            self.forget_one(&key);
        }
    }

    fn forget_one(&self, key: &str) {
        self.settled.lock().unwrap().remove(key);
        for l in &self.listers {
            l.asker.forget(key);
        }
    }

    fn showing(&self) -> bool {
        self.wanted.lock().unwrap().is_some_and(|w| w.elapsed() <= SHOWING)
    }

    /// Until shutdown, every `tick` or sooner: lets go of the sessions
    /// that ended without a word, and, while the list is showing, asks each
    /// agent that lists its sessions when something calls for it
    /// (`lister::Asker`). Runs whether or not any is asked.
    pub async fn serve(self: Arc<Self>, ledger: Ledger, shutdown: CancellationToken) {
        let mut wait = self.tick;
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(wait) => {}
            }
            self.prune_told();
            wait = self.tick;
            if !self.showing() {
                continue;
            }
            for l in &self.listers {
                let background = self
                    .hooked
                    .any(|s| s.agent == l.agent && s.kind == lister::BACKGROUND_KIND);
                let due = l.asker.due_in(Instant::now(), background);
                if due.is_zero() {
                    self.ask(l, &ledger).await;
                } else {
                    wait = wait.min(due);
                }
            }
        }
    }

    /// Asks one agent for its sessions once, and takes in what it says
    /// (`hooked::merge`). Each session listed is filed first, so none is
    /// kept without its folder; since when it stands as it does is settled
    /// once merged, as it is a hook's word, where one was heard since, that
    /// says how.
    async fn ask(&self, l: &lister::Lister, ledger: &Ledger) {
        let asker = &l.asker;
        let began = Instant::now();
        asker.began(began);
        match l.hear(&self.registry, asker.polling().wait).await {
            Ok(mut list) => {
                asker.listed(list.iter().map(Outside::key).collect());
                for s in &mut list {
                    self.file_one(s, ledger).await;
                }
                let merged = self.hooked.merge(&l.agent, list, began, hooked::alive);
                for key in &merged.gone {
                    debug!(session = %key, "outside: no longer listed");
                    self.forget_one(key);
                }
                for mut s in merged.listed {
                    self.stand_one(&mut s);
                    self.hooked.settle(&s);
                }
            }
            Err(e) => asker.not_heard(e),
        }
    }

    /// One session as just heard, given its project folder and since when
    /// it has (or has not) wanted you (`file_one`, `stand_one`).
    pub async fn settle_one(&self, s: &mut Outside, ledger: &Ledger) {
        self.file_one(s, ledger).await;
        self.stand_one(s);
    }

    /// What was settled of a session, or, first seen, what is known: it has
    /// stood so since it started, as far as is known — a restart of `rld`
    /// does not make an old wait new.
    fn entry<'a>(settled: &'a mut HashMap<String, Settled>, s: &Outside) -> &'a mut Settled {
        let now = Timestamp::now();
        settled.entry(s.key()).or_insert_with(|| Settled {
            workspace: None,
            wanted: s.wants_you(),
            at: s.started_at.map_or(now, |t| t.min(now)),
        })
    }

    /// Since when a session has (or has not) wanted you, as it now stands.
    fn stand_one(&self, s: &mut Outside) {
        let wanted = s.wants_you();
        let mut settled = self.settled.lock().unwrap();
        let entry = Self::entry(&mut settled, s);
        if entry.wanted != wanted {
            entry.wanted = wanted;
            entry.at = Timestamp::now();
        }
        s.workspace = entry.workspace.clone();
        s.state_at = Some(entry.at);
    }

    /// A session's project folder: resolved once; one that cannot be is
    /// left for the panel to guess, and tried again next time.
    async fn file_one(&self, s: &mut Outside, ledger: &Ledger) {
        let key = s.key();
        let filed = self
            .settled
            .lock()
            .unwrap()
            .get(&key)
            .is_some_and(|e| e.workspace.is_some());
        let found = if filed {
            None
        } else {
            match filed_under(ledger, &s.cwd).await {
                Ok(w) => Some(w),
                Err(e) => {
                    debug!(error = %format!("{e:#}"), cwd = %s.cwd.display(), "outside: no folder settled");
                    None
                }
            }
        };
        let mut settled = self.settled.lock().unwrap();
        let entry = Self::entry(&mut settled, s);
        if entry.workspace.is_none() {
            entry.workspace = found;
        }
        s.workspace = entry.workspace.clone();
        s.state_at = Some(entry.at);
    }
}

/// A session held while it is carried on (`Outsiders::moving`).
pub struct Moving<'a> {
    store: &'a Outsiders,
    key: String,
}

impl Drop for Moving<'_> {
    fn drop(&mut self) {
        self.store.moving.lock().unwrap().remove(&self.key);
    }
}

/// The project folder a session in `cwd` stands under, as a run started
/// there is filed: `cwd` made canonical, then as `workspace::filed_under`
/// has it. A folder gone from disk is taken as given.
async fn filed_under(ledger: &Ledger, cwd: &std::path::Path) -> Result<PathBuf> {
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let place = crate::git::placement(&cwd).await;
    crate::workspace::filed_under(ledger, &cwd, place.toplevel).await
}

/// The run that carries each of these sessions, by the session's key: the
/// one tagged `<agent>-session:<id>`, the newest where several are.
pub fn carried(c: &rusqlite::Connection, sessions: &[Outside]) -> Result<HashMap<String, String>> {
    tagged(c, sessions, tag)
}

/// Those of these sessions carried on by a run while left open in their
/// terminal (`left_open`), by key, each with the process that held it
/// there where the run says (`left_open_pid`).
pub fn left_open_of(c: &rusqlite::Connection, sessions: &[Outside]) -> Result<HashMap<String, Option<u32>>> {
    let mut out = HashMap::new();
    for (key, run) in tagged(c, sessions, left_open)? {
        let Some(s) = sessions.iter().find(|s| s.key() == key) else {
            continue;
        };
        let held = format!("{}{}:", left_open_pid(&s.agent), s.session_id);
        let pid = rl_ledger::runs::get(c, &run)?
            .and_then(|r| r.tags.iter().find_map(|t| t.strip_prefix(&held)?.parse().ok()));
        out.insert(key, pid);
    }
    Ok(out)
}

/// The run tagged `<prefix(agent)><id>` for each of these sessions, by key.
fn tagged(
    c: &rusqlite::Connection,
    sessions: &[Outside],
    prefix: fn(&str) -> String,
) -> Result<HashMap<String, String>> {
    let tagged: HashMap<String, String> = sessions
        .iter()
        .map(|s| (format!("{}{}", prefix(&s.agent), s.session_id), s.key()))
        .collect();
    let tags: Vec<String> = tagged.keys().cloned().collect();
    Ok(rl_ledger::runs::carrying(c, &tags)?
        .into_iter()
        .filter_map(|(t, run)| Some((tagged.get(&t)?.clone(), run)))
        .collect())
}

/// The ended runs these background sessions went on from, by the session's
/// key: a Relay run of Claude's sent to Claude's background (`/background`)
/// ends here, and its conversation goes on in a process Claude's supervisor
/// hosts, which `RELAY_RUN` does not reach — so no hook tags it, and it
/// lists under the session id the run is tagged with (`carried`).
///
/// This rests on `/background` keeping the session id, which Claude does
/// not document (to be checked by hand): were it a new one, nothing here
/// ever matches and such a session simply lists as any other of Claude's.
/// Only a run that has ended counts — a live one still has the session —
/// and only a session started after the run did: one moved in from Claude's
/// background (`import`) started before its run, and stays Relay's.
pub fn went_from(
    c: &rusqlite::Connection,
    sessions: &[Outside],
    carried: &HashMap<String, String>,
) -> Result<HashMap<String, WentFrom>> {
    let mut out = HashMap::new();
    for s in sessions.iter().filter(|s| s.agent == claude::AGENT && s.id.is_some()) {
        let key = s.key();
        let Some(run) = carried.get(&key) else { continue };
        let Some(r) = rl_ledger::runs::get(c, run)? else {
            continue;
        };
        if r.state.is_terminal() && s.started_at.is_some_and(|t| t > r.started_at) {
            out.insert(
                key,
                WentFrom {
                    run: r.id,
                    title: r.title,
                },
            );
        }
    }
    Ok(out)
}

/// An ended run's summary, once a background session went on from it.
pub const WENT: &str = "went to Claude's background";

/// What an ended run sent to Claude's background says on its row, for the
/// sessions that went on from it (`Outside::was`): where it went.
pub fn point_on(items: &mut [DigestItem], outside: &[Outside]) {
    for was in outside.iter().filter_map(|s| s.was.as_ref()) {
        if let Some(i) = items.iter_mut().find(|i| i.run.id == was.run) {
            i.summary = WENT.into();
            i.recap = None;
        }
    }
}

/// The sessions that are not Relay's own: none whose pid is a live run's
/// agent, none a run carries (`carried`, by key) — but for one carried on
/// while left open in its terminal (`left`, by key, with the process that
/// held it there where known) that is at work or waiting there again, in
/// that process: still the user's, and two processes on one conversation
/// is worth seeing. It goes with the run that carries it. Nor one that went
/// on from an ended run (`was`, by key): it says which. One row a session:
/// where its agent lists it twice (resumed twice by hand), the one whose
/// process lives and started last.
pub fn not_relays(
    list: Vec<Outside>,
    carried: &HashMap<String, String>,
    left: &HashMap<String, Option<u32>>,
    was: &HashMap<String, WentFrom>,
    pids: &HashSet<u32>,
) -> Vec<Outside> {
    let kept = list
        .into_iter()
        .filter(|s| s.pid.is_none_or(|p| !pids.contains(&p)))
        .filter_map(|mut s| {
            let key = s.key();
            let Some(run) = carried.get(&key) else {
                return Some(s);
            };
            if let Some(w) = was.get(&key) {
                s.was = Some(w.clone());
                return Some(s);
            }
            let its_terminal = left.get(&key).is_some_and(|held| held.is_none_or(|p| s.pid == Some(p)));
            (its_terminal && s.in_a_terminal() && s.at_work()).then(|| {
                s.run = Some(run.clone());
                s
            })
        });
    let mut out: Vec<Outside> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for s in kept {
        match at.get(&s.key()) {
            Some(&i) => {
                let rank = |s: &Outside| (s.pid.is_some(), s.started_at);
                if rank(&s) > rank(&out[i]) {
                    out[i] = s;
                }
            }
            None => {
                at.insert(s.key(), out.len());
                out.push(s);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store of the built-in profiles' sessions, asking no agent.
    fn store() -> Outsiders {
        Outsiders::new(Arc::new(Registry::builtin()), None)
    }

    fn session(agent: &str, sid: &str, pid: Option<u32>) -> Outside {
        Outside {
            agent: agent.into(),
            pid,
            ..claude::parse(&format!(r#"[{{"cwd":"/w","kind":"interactive","sessionId":"{sid}"}}]"#))
                .unwrap()
                .remove(0)
        }
    }

    #[test]
    fn relays_own_sessions_are_left_out() {
        let list = vec![
            session("claude", "tagged", Some(1)),
            session("claude", "ours", Some(7)),
            session("claude", "theirs", Some(8)),
            session("claude", "dead", None),
            session("codex", "tagged", None),
        ];
        let carried = HashMap::from([("o:claude:tagged".to_string(), "r1".to_string())]);
        let pids = HashSet::from([7]);
        let kept = not_relays(list, &carried, &HashMap::new(), &HashMap::new(), &pids);
        assert_eq!(
            kept.iter().map(Outside::key).collect::<Vec<_>>(),
            ["o:claude:theirs", "o:claude:dead", "o:codex:tagged"],
            "a session is carried by its own agent's tag, not another's"
        );
    }

    #[test]
    fn one_left_open_in_its_terminal_shows_again_while_at_work_there() {
        let at = |sid: &str, status: &str, pid: Option<u32>| Outside {
            status: status.into(),
            ..session("codex", sid, pid)
        };
        let list = vec![
            at("busy", Outside::BUSY, Some(8)),
            at("asks", Outside::WAITING, None),
            at("idle", Outside::IDLE, Some(9)),
            // Relay's own process on the same conversation: its pid says so.
            at("ours", Outside::BUSY, Some(7)),
            // Carried, but not left open (a hook's tag, say): Relay's.
            at("hooked", Outside::BUSY, None),
        ];
        let carried: HashMap<String, String> = ["busy", "asks", "idle", "ours", "hooked"]
            .iter()
            .map(|s| (format!("o:codex:{s}"), format!("r-{s}")))
            .collect();
        let left: HashMap<String, Option<u32>> = ["busy", "asks", "idle", "ours"]
            .iter()
            .map(|s| (format!("o:codex:{s}"), None))
            .collect();
        let kept = not_relays(list, &carried, &left, &HashMap::new(), &HashSet::from([7]));
        assert_eq!(
            kept.iter()
                .map(|s| (s.key(), s.run.clone().unwrap_or_default()))
                .collect::<Vec<_>>(),
            [
                ("o:codex:busy".to_string(), "r-busy".to_string()),
                ("o:codex:asks".to_string(), "r-asks".to_string()),
            ]
        );
    }

    #[test]
    fn only_the_terminals_own_process_shows_as_left_open() {
        let at = |pid: u32| Outside {
            status: Outside::BUSY.into(),
            ..session("claude", "both", Some(pid))
        };
        // Claude lists the terminal's process and the run's (a re-exec, say,
        // whose pid is not the one Relay started) under one session.
        let list = vec![at(9), at(8)];
        let carried = HashMap::from([("o:claude:both".to_string(), "r1".to_string())]);
        let left = HashMap::from([("o:claude:both".to_string(), Some(8))]);
        let kept = not_relays(list.clone(), &carried, &left, &HashMap::new(), &HashSet::new());
        assert_eq!(
            kept.iter().map(|s| (s.pid, s.run.clone())).collect::<Vec<_>>(),
            [(Some(8), Some("r1".to_string()))]
        );
        // Where the run does not say which process it was, either shows.
        let left = HashMap::from([("o:claude:both".to_string(), None)]);
        let kept = not_relays(list, &carried, &left, &HashMap::new(), &HashSet::new());
        assert_eq!(kept.len(), 1, "one row a session: {kept:?}");
    }

    #[test]
    fn a_session_listed_twice_is_one_row_its_live_newest_process() {
        let t = |s: &str| -> Timestamp { s.parse().unwrap() };
        let at = |pid: Option<u32>, started: &str| Outside {
            started_at: Some(t(started)),
            ..session("claude", "twice", pid)
        };
        let list = vec![
            at(Some(1), "2026-09-10T10:00:00Z"),
            session("claude", "other", Some(2)),
            at(Some(3), "2026-09-10T11:00:00Z"),
            at(None, "2026-09-10T12:00:00Z"),
        ];
        let kept = not_relays(list, &HashMap::new(), &HashMap::new(), &HashMap::new(), &HashSet::new());
        assert_eq!(
            kept.iter().map(|s| (s.key(), s.pid)).collect::<Vec<_>>(),
            [
                ("o:claude:twice".to_string(), Some(3)),
                ("o:claude:other".to_string(), Some(2))
            ]
        );
    }

    #[tokio::test]
    async fn the_left_open_process_is_read_back_from_the_run() {
        use rl_proto::types::{RunKind, RunState};
        let ledger = Ledger::open_in_memory().unwrap();
        let sessions = vec![session("claude", "held", Some(8)), session("codex", "told", None)];
        let mut runs = vec![];
        for (id, agent, sid, pid) in [("r1", "claude", "held", Some(8)), ("r2", "codex", "told", None)] {
            let mut r = rl_proto::types::Run::new(id, RunKind::Agent, "x", vec![], "/w", "/w", Timestamp::now());
            r.state = RunState::Working;
            r.tags = vec![format!("{}{sid}", tag(agent)), format!("{}{sid}", left_open(agent))];
            if let Some(p) = pid {
                r.tags.push(format!("{}{sid}:{p}", left_open_pid(agent)));
            }
            runs.push(r);
        }
        let left = ledger
            .call(move |c| {
                for r in &runs {
                    rl_ledger::runs::insert(c, r)?;
                }
                left_open_of(c, &sessions)
            })
            .await
            .unwrap();
        assert_eq!(
            left,
            HashMap::from([
                ("o:claude:held".to_string(), Some(8)),
                ("o:codex:told".to_string(), None)
            ])
        );
    }

    #[test]
    fn a_session_is_carried_on_by_one_ask_at_a_time() {
        let store = store();
        let held = store.moving("o:codex:s").expect("free");
        assert!(store.moving("o:codex:s").is_none(), "held already");
        assert!(store.moving("o:codex:t").is_some(), "another is free");
        drop(held);
        assert!(store.moving("o:codex:s").is_some(), "let go");
    }

    #[tokio::test]
    async fn a_hook_names_its_conversation_and_the_last_one_named_is_kept() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        let tell = |event: &str, transcript: Option<&str>| rl_proto::methods::OutsideHookParams {
            agent: "codex".into(),
            session_id: "t1".into(),
            cwd: "/".into(),
            event: event.into(),
            detail: None,
            pid: None,
            transcript_path: transcript.map(Into::into),
            run: None,
        };
        store
            .told(tell("SessionStart", Some("/t/one.jsonl")), &ledger)
            .await
            .unwrap();
        store.told(tell("Stop", None), &ledger).await.unwrap();
        let last = store.told_of("codex", "t1").unwrap();
        assert_eq!(last.transcript.as_deref(), Some(std::path::Path::new("/t/one.jsonl")));
        assert!(last.heard.elapsed() < Duration::from_secs(5));
        store.told(tell("Stop", Some("rel.jsonl")), &ledger).await.unwrap();
        let last = store.told_of("codex", "t1").unwrap();
        assert_eq!(
            last.transcript.as_deref(),
            Some(std::path::Path::new("/t/one.jsonl")),
            "a file not named whole is not taken"
        );
        assert!(store.told_of("codex", "nope").is_none());
    }

    #[tokio::test]
    async fn one_session_settles_without_its_siblings_and_says_since_when_it_wants_you() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        let tmp = tempfile::tempdir().unwrap();
        let started: Timestamp = "2026-09-10T11:00:00Z".parse().unwrap();
        let at = |agent: &str, sid: &str, state: Option<&str>, status: &str| Outside {
            cwd: tmp.path().to_path_buf(),
            state: state.map(Into::into),
            status: status.into(),
            started_at: Some(started),
            ..session(agent, sid, None)
        };
        let mut a = at("codex", "a", None, "busy");
        let mut b = at("codex", "b", None, "busy");
        let mut c = at("claude", "c", Some("working"), "busy");
        for s in [&mut a, &mut b, &mut c] {
            store.settle_one(s, &ledger).await;
        }
        // First seen: since it started, as far as is known.
        assert_eq!(a.state_at, Some(started));
        assert_eq!(a.workspace, Some(tmp.path().canonicalize().unwrap()));

        // One told again on its own leaves its siblings as they were.
        let mut a2 = at("codex", "a", None, "busy");
        store.settle_one(&mut a2, &ledger).await;
        let mut b2 = at("codex", "b", None, "busy");
        store.settle_one(&mut b2, &ledger).await;
        assert_eq!((a2.state_at, b2.state_at), (Some(started), Some(started)));

        // Within wanting you or not, nothing is news; across it, now is.
        let mut c2 = at("claude", "c", Some("done"), "idle");
        store.settle_one(&mut c2, &ledger).await;
        assert_eq!(c2.state_at, Some(started), "working to done: still not wanting you");
        let mut c3 = at("claude", "c", Some("blocked"), "waiting");
        store.settle_one(&mut c3, &ledger).await;
        assert!(c3.state_at.is_some_and(|t| t > started), "{:?}", c3.state_at);
        let blocked = c3.state_at;
        let mut c4 = at("claude", "c", Some("failed"), "");
        store.settle_one(&mut c4, &ledger).await;
        assert_eq!(c4.state_at, blocked, "blocked to failed: still wanting you");
    }

    #[tokio::test]
    async fn told_sessions_stand_as_their_last_event_left_them_and_go_when_they_end() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        let tmp = tempfile::tempdir().unwrap();
        let tell = |agent: &str, sid: &str, event: &str, detail: Option<&str>, pid: Option<u32>| {
            rl_proto::methods::OutsideHookParams {
                agent: agent.into(),
                session_id: sid.into(),
                cwd: tmp.path().join("app"),
                event: event.into(),
                detail: detail.map(Into::into),
                pid,
                transcript_path: None,
                run: None,
            }
        };
        let one = |key: &str| store.latest(true).into_iter().find(|s| s.key() == key);

        store
            .told(tell("codex", "c1", "SessionStart", Some("startup"), Some(41)), &ledger)
            .await
            .unwrap();
        let s = one("o:codex:c1").unwrap();
        assert_eq!(
            (
                s.kind.as_str(),
                s.status.as_str(),
                s.id.as_deref(),
                s.pid,
                s.name.as_str()
            ),
            ("interactive", "idle", None, Some(41), "app")
        );
        assert!(s.started_at.is_some() && s.state_at.is_some() && s.workspace.is_some());
        assert!(!s.wants_you());

        store
            .told(tell("codex", "c1", "PermissionRequest", Some("shell"), None), &ledger)
            .await
            .unwrap();
        let s = one("o:codex:c1").unwrap();
        assert!(s.wants_you());
        assert_eq!((s.waiting_for.as_deref(), s.pid), (Some("permission prompt"), Some(41)));

        // A start heard after the prompt that followed it says nothing new.
        store
            .told(tell("gemini", "g1", "BeforeAgent", None, None), &ledger)
            .await
            .unwrap();
        store
            .told(tell("gemini", "g1", "SessionStart", None, None), &ledger)
            .await
            .unwrap();
        assert_eq!(one("o:gemini:g1").unwrap().status, "busy");
        store
            .told(
                tell("gemini", "g1", "Notification", Some("ToolPermission"), None),
                &ledger,
            )
            .await
            .unwrap();
        assert!(one("o:gemini:g1").unwrap().wants_you());
        store
            .told(tell("gemini", "g1", "AfterAgent", None, None), &ledger)
            .await
            .unwrap();
        let g = one("o:gemini:g1").unwrap();
        assert_eq!((g.status.as_str(), g.waiting_for), ("idle", None));

        store
            .told(tell("codex", "c1", "SessionEnd", Some("exit"), None), &ledger)
            .await
            .unwrap();
        assert!(one("o:codex:c1").is_none());
        assert!(!store.settled.lock().unwrap().contains_key("o:codex:c1"));

        // What is not a told session is refused, and nothing is kept of it.
        for bad in [
            tell("cursor", "x", "Stop", None, None),
            tell("codex", "", "Stop", None, None),
            rl_proto::methods::OutsideHookParams {
                cwd: "rel".into(),
                ..tell("codex", "x", "Stop", None, None)
            },
        ] {
            assert!(store.told(bad, &ledger).await.is_err());
        }
        assert_eq!(store.latest(true).len(), 1);
    }

    #[tokio::test]
    async fn a_told_session_goes_with_its_process_or_after_long_quiet() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        let tell = |sid: &str, pid: Option<u32>| rl_proto::methods::OutsideHookParams {
            agent: "codex".into(),
            session_id: sid.into(),
            cwd: "/".into(),
            event: "Stop".into(),
            detail: None,
            pid,
            transcript_path: None,
            run: None,
        };
        // A process that has been and gone.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        store.told(tell("dead", Some(dead)), &ledger).await.unwrap();
        store.told(tell("me", Some(std::process::id())), &ledger).await.unwrap();
        store.told(tell("quiet", None), &ledger).await.unwrap();
        store.told(tell("recent", None), &ledger).await.unwrap();
        store
            .hooked
            .age("o:codex:quiet", hooked::STALE + Duration::from_secs(1));
        store.prune_told();
        let keys: Vec<String> = store.latest(true).iter().map(Outside::key).collect();
        assert_eq!(keys, ["o:codex:me", "o:codex:recent"]);
        assert!(!store.settled.lock().unwrap().contains_key("o:codex:dead"));
        // A live process, but no word for very long: the pid may not be it.
        store.hooked.age("o:codex:me", hooked::QUIET + Duration::from_secs(1));
        store.prune_told();
        let keys: Vec<String> = store.latest(true).iter().map(Outside::key).collect();
        assert_eq!(keys, ["o:codex:recent"]);
    }

    #[tokio::test]
    async fn a_told_session_in_a_folder_gone_goes() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone");
        std::fs::create_dir(&gone).unwrap();
        let tell = |sid: &str, cwd: std::path::PathBuf| rl_proto::methods::OutsideHookParams {
            agent: "codex".into(),
            session_id: sid.into(),
            cwd,
            event: "Stop".into(),
            detail: None,
            pid: Some(std::process::id()),
            transcript_path: None,
            run: None,
        };
        store.told(tell("gone", gone.clone()), &ledger).await.unwrap();
        store.told(tell("here", "/".into()), &ledger).await.unwrap();
        std::fs::remove_dir(&gone).unwrap();
        store.prune_told();
        let keys: Vec<String> = store.latest(true).iter().map(Outside::key).collect();
        assert_eq!(keys, ["o:codex:here"]);
    }

    fn told_as(
        agent: &str,
        sid: &str,
        event: &str,
        detail: Option<&str>,
        pid: Option<u32>,
    ) -> rl_proto::methods::OutsideHookParams {
        rl_proto::methods::OutsideHookParams {
            agent: agent.into(),
            session_id: sid.into(),
            cwd: "/".into(),
            event: event.into(),
            detail: detail.map(Into::into),
            pid,
            transcript_path: None,
            run: None,
        }
    }

    #[tokio::test]
    async fn approving_and_the_tool_running_makes_a_session_busy_again() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        for (agent, wait, wait_detail, ran) in [
            ("codex", "PermissionRequest", Some("shell"), "PostToolUse"),
            ("gemini", "Notification", Some("ToolPermission"), "AfterTool"),
        ] {
            store
                .told(told_as(agent, "s", wait, wait_detail, None), &ledger)
                .await
                .unwrap();
            let key = format!("o:{agent}:s");
            let one = || store.latest(true).into_iter().find(|s| s.key() == key).unwrap();
            assert!(one().wants_you());
            store.told(told_as(agent, "s", ran, None, None), &ledger).await.unwrap();
            let s = one();
            assert!(!s.wants_you(), "{agent}: {s:?}");
            assert_eq!((s.status.as_str(), s.waiting_for), ("busy", None));
        }
        store
            .told(told_as("codex", "s", "Interrupt", None, None), &ledger)
            .await
            .unwrap();
        let s = store.latest(true).into_iter().find(|s| s.key() == "o:codex:s").unwrap();
        assert_eq!(s.status, "idle");
    }

    #[tokio::test]
    async fn an_ended_session_is_not_brought_back_by_word_sent_before_its_end() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        store
            .told(told_as("codex", "s", "UserPromptSubmit", None, None), &ledger)
            .await
            .unwrap();
        // Heard, then the end lands while it is still being settled.
        let codex = store.registry.user_hooks("codex").unwrap();
        let hooked::Said::Stands { session, .. } = store.hooked.hear(told_as("codex", "s", "Stop", None, None), codex)
        else {
            panic!("a session stands");
        };
        store
            .told(told_as("codex", "s", "SessionEnd", None, None), &ledger)
            .await
            .unwrap();
        assert!(!store.hooked.keep(*session, None, true), "the end stands");
        // A late word after the end: nothing.
        store
            .told(told_as("codex", "s", "PermissionRequest", None, None), &ledger)
            .await
            .unwrap();
        assert!(store.latest(true).is_empty());
        assert!(!store.settled.lock().unwrap().contains_key("o:codex:s"));
        // A start (`codex resume`, the same id) brings it back.
        store
            .told(told_as("codex", "s", "SessionStart", Some("resume"), None), &ledger)
            .await
            .unwrap();
        assert_eq!(store.latest(true).len(), 1);
        // The end is forgotten after a while.
        store
            .told(told_as("codex", "t", "SessionEnd", None, None), &ledger)
            .await
            .unwrap();
        store.hooked.age("o:codex:t", hooked::ENDED + Duration::from_secs(1));
        store.prune_told();
        store
            .told(told_as("codex", "t", "Stop", None, None), &ledger)
            .await
            .unwrap();
        assert_eq!(store.latest(true).len(), 2);
    }

    #[tokio::test]
    async fn a_start_in_a_process_ends_the_session_it_held_before() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        let me = Some(std::process::id());
        store
            .told(told_as("gemini", "old", "PermissionRequest", None, me), &ledger)
            .await
            .unwrap();
        store
            .told(
                told_as("gemini", "old", "Notification", Some("ToolPermission"), me),
                &ledger,
            )
            .await
            .unwrap();
        store
            .told(told_as("codex", "same-pid-other-agent", "Stop", None, me), &ledger)
            .await
            .unwrap();
        store
            .told(told_as("gemini", "elsewhere", "Stop", None, Some(1)), &ledger)
            .await
            .unwrap();
        // `/clear`: a new session id in the same process.
        store
            .told(told_as("gemini", "new", "SessionStart", Some("clear"), me), &ledger)
            .await
            .unwrap();
        let keys: Vec<String> = store.latest(true).iter().map(Outside::key).collect();
        assert_eq!(
            keys,
            ["o:codex:same-pid-other-agent", "o:gemini:elsewhere", "o:gemini:new"]
        );
        assert!(!store.settled.lock().unwrap().contains_key("o:gemini:old"));
    }

    #[tokio::test]
    async fn a_tool_done_heard_after_the_turn_ended_does_not_wake_it() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        let status = || store.latest(true)[0].status.clone();
        store
            .told(told_as("codex", "s", "UserPromptSubmit", None, None), &ledger)
            .await
            .unwrap();
        store
            .told(told_as("codex", "s", "Stop", None, None), &ledger)
            .await
            .unwrap();
        // Codex's background hooks: the last tool's word lands after the end.
        store
            .told(told_as("codex", "s", "PostToolUse", None, None), &ledger)
            .await
            .unwrap();
        assert_eq!(status(), "idle");
        // A turn going on is kept going.
        store
            .told(told_as("codex", "s", "UserPromptSubmit", None, None), &ledger)
            .await
            .unwrap();
        store
            .told(told_as("codex", "s", "PostToolUse", None, None), &ledger)
            .await
            .unwrap();
        assert_eq!(status(), "busy");
    }

    #[tokio::test]
    async fn a_run_sent_to_claudes_background_points_to_where_it_went() {
        use rl_proto::types::{RunKind, RunState};
        let ledger = Ledger::open_in_memory().unwrap();
        let t = |s: &str| -> Timestamp { s.parse().unwrap() };
        let bg = |sid: &str, started: &str| Outside {
            id: Some(format!("bg-{sid}")),
            kind: "background".into(),
            state: Some("working".into()),
            started_at: Some(t(started)),
            ..session("claude", sid, Some(99))
        };
        let runs: Vec<rl_proto::types::Run> = [
            ("ended", RunState::Finished, "fix the build"),
            ("live", RunState::Working, "still here"),
            ("moved", RunState::Finished, "moved in"),
        ]
        .into_iter()
        .map(|(sid, state, title)| {
            let mut r = rl_proto::types::Run::new(
                format!("r-{sid}"),
                RunKind::Agent,
                title,
                vec![],
                "/w",
                "/w",
                t("2026-09-10T10:00:00Z"),
            );
            r.state = state;
            r.tags = vec![format!("{}{sid}", tag("claude"))];
            r
        })
        .collect();
        let list = vec![
            // Sent to the background after its run began: it ended here.
            bg("ended", "2026-09-10T10:30:00Z"),
            // The run is live: it still has the session.
            bg("live", "2026-09-10T10:30:00Z"),
            // Moved in from the background: it was there before its run.
            bg("moved", "2026-09-10T09:00:00Z"),
        ];
        let sessions = list.clone();
        let (carried, was) = ledger
            .call(move |c| {
                for r in &runs {
                    rl_ledger::runs::insert(c, r)?;
                }
                let carried = carried(c, &sessions)?;
                let was = went_from(c, &sessions, &carried)?;
                Ok((carried, was))
            })
            .await
            .unwrap();
        assert_eq!(carried.len(), 3);
        let kept = not_relays(list, &carried, &HashMap::new(), &was, &HashSet::new());
        assert_eq!(
            kept.iter().map(|s| (s.key(), s.was.clone())).collect::<Vec<_>>(),
            [(
                "o:claude:ended".to_string(),
                Some(WentFrom {
                    run: "r-ended".into(),
                    title: "fix the build".into()
                })
            )],
            "one row of Claude's, for the ended run only"
        );

        let item = |id: &str| DigestItem {
            run: rl_proto::types::Run::new(id, RunKind::Agent, "x", vec![], "/w", "/w", t("2026-09-10T10:00:00Z")),
            summary: "exit 0 · 30m".into(),
            at: t("2026-09-10T10:30:00Z"),
            unseen: false,
            joined_by: None,
            emitted: vec![],
            cwd: None,
            recap: Some("fixed it".into()),
        };
        let mut items = vec![item("r-ended"), item("r-live")];
        point_on(&mut items, &kept);
        assert_eq!((items[0].summary.as_str(), items[0].recap.as_deref()), (WENT, None));
        assert_eq!(items[1].summary, "exit 0 · 30m");
        assert_eq!(WENT, "went to Claude's background");
    }

    #[test]
    fn each_agent_has_its_own_tag() {
        assert_eq!(tag("claude"), crate::import::TAG);
        assert_eq!(tag("codex"), "codex-session:");
        assert_eq!(tag("gemini"), "gemini-session:");
    }

    /// One of Claude's as `claude agents` lists it.
    fn listed(sid: &str, id: Option<&str>, kind: &str, status: &str, pid: Option<u32>) -> Outside {
        Outside {
            id: id.map(Into::into),
            kind: kind.into(),
            status: status.into(),
            name: format!("named {sid}"),
            started_at: Some("2026-10-13T09:00:00Z".parse().unwrap()),
            ..session("claude", sid, pid)
        }
    }

    #[tokio::test]
    async fn an_ask_fills_in_a_told_session_and_a_hook_heard_since_keeps_its_state() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        let me = std::process::id();
        store
            .told(told_as("claude", "s", "UserPromptSubmit", None, None), &ledger)
            .await
            .unwrap();
        // Asked a while after the hook: all of it is taken.
        let began = Instant::now();
        let merged = store.hooked.merge(
            "claude",
            vec![listed("s", Some("bg1"), "background", "idle", Some(me))],
            began,
            |_| true,
        );
        assert!(merged.gone.is_empty());
        let s = store.latest(true).remove(0);
        assert_eq!(
            (
                s.id.as_deref(),
                s.kind.as_str(),
                s.name.as_str(),
                s.pid,
                s.status.as_str()
            ),
            (Some("bg1"), "background", "named s", Some(me), "idle")
        );
        assert!(s.started_at.is_some());

        // A hook heard after the next ask began: its word stands, the rest
        // is filled in still.
        let began = Instant::now();
        store
            .told(
                told_as("claude", "s", "Notification", Some("permission_prompt"), None),
                &ledger,
            )
            .await
            .unwrap();
        store.hooked.merge(
            "claude",
            vec![listed("s", Some("bg2"), "background", "busy", Some(me))],
            began,
            |_| true,
        );
        let s = store.latest(true).remove(0);
        assert_eq!(
            (s.id.as_deref(), s.status.as_str(), s.waiting_for.as_deref()),
            (Some("bg2"), "waiting", Some("permission prompt"))
        );
        assert!(s.wants_you());
    }

    #[tokio::test]
    async fn a_word_that_says_nothing_neither_changes_a_session_nor_keeps_an_asks_state_out() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        store
            .told(told_as("claude", "s", "UserPromptSubmit", None, None), &ledger)
            .await
            .unwrap();
        let began = Instant::now();
        store
            .told(
                told_as("claude", "s", "Notification", Some("auth_success"), None),
                &ledger,
            )
            .await
            .unwrap();
        assert_eq!(store.latest(true)[0].status, "busy");
        store.hooked.merge(
            "claude",
            vec![listed("s", Some("b"), "background", "waiting", None)],
            began,
            |_| true,
        );
        assert_eq!(store.latest(true)[0].status, "waiting", "the ask's word is the newer");

        // A turn's end leaves a background session's `failed` as it was.
        store.hooked.merge(
            "claude",
            vec![Outside {
                state: Some("failed".into()),
                ..listed("s", Some("b"), "background", "idle", None)
            }],
            Instant::now(),
            |_| true,
        );
        store
            .told(told_as("claude", "s", "Stop", None, None), &ledger)
            .await
            .unwrap();
        let s = store.latest(true).remove(0);
        assert_eq!((s.state.as_deref(), s.wants_you()), (Some("failed"), true));
        // A prompt is newer than it.
        store
            .told(told_as("claude", "s", "UserPromptSubmit", None, None), &ledger)
            .await
            .unwrap();
        assert_eq!(store.latest(true)[0].state, None);
    }

    #[tokio::test]
    async fn one_only_an_ask_listed_stands_while_listed_and_an_end_ends_it_for_both() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        let keys = || store.latest(true).iter().map(Outside::key).collect::<Vec<_>>();
        store
            .told(told_as("codex", "c", "Stop", None, None), &ledger)
            .await
            .unwrap();
        store
            .told(told_as("claude", "told", "Stop", None, None), &ledger)
            .await
            .unwrap();
        // Its process gone: listed still, but with no process to say so.
        let merged = store.hooked.merge(
            "claude",
            vec![
                listed("asked", Some("b1"), "background", "", Some(4_000_001)),
                listed("other", None, "interactive", "idle", None),
            ],
            Instant::now(),
            |_| false,
        );
        assert_eq!(merged.listed.len(), 2);
        assert_eq!(store.latest(true)[0].pid, None, "{:?}", store.latest(true));
        assert_eq!(
            keys(),
            ["o:claude:asked", "o:claude:other", "o:claude:told", "o:codex:c"]
        );
        // Not listed again: one told of by nothing goes; told ones stay.
        let merged = store.hooked.merge(
            "claude",
            vec![listed("other", None, "interactive", "idle", None)],
            Instant::now(),
            |_| true,
        );
        assert_eq!(merged.gone, ["o:claude:asked"]);
        assert_eq!(keys(), ["o:claude:other", "o:claude:told", "o:codex:c"]);
        // Its end, from its hook: gone, and a late listing does not bring
        // it back.
        store
            .told(told_as("claude", "other", "SessionEnd", Some("exit"), None), &ledger)
            .await
            .unwrap();
        store.hooked.merge(
            "claude",
            vec![listed("other", None, "interactive", "idle", None)],
            Instant::now(),
            |_| true,
        );
        assert_eq!(keys(), ["o:claude:told", "o:codex:c"]);
    }

    #[tokio::test]
    async fn claudes_user_level_word_under_a_run_of_claudes_is_the_runs_and_let_go() {
        use rl_proto::types::{Run, RunKind};
        let ledger = Ledger::open_in_memory().unwrap();
        let store = store();
        let mut ours = Run::new("r-claude", RunKind::Agent, "x", vec![], "/w", "/w", Timestamp::now());
        ours.cli = Some("claude".into());
        // One that has ended, whose `RELAY_RUN` a process it left behind
        // (Claude's background supervisor) still carries.
        let mut ended = Run::new("r-ended", RunKind::Agent, "x", vec![], "/w", "/w", Timestamp::now());
        ended.cli = Some("claude".into());
        ended.state = rl_proto::types::RunState::Finished;
        let shell = Run::new("r-shell", RunKind::Shell, "x", vec![], "/w", "/w", Timestamp::now());
        ledger
            .call(move |c| {
                rl_ledger::runs::insert(c, &ours)?;
                rl_ledger::runs::insert(c, &ended)?;
                rl_ledger::runs::insert(c, &shell)
            })
            .await
            .unwrap();
        let under = |sid: &str, run: &str| rl_proto::methods::OutsideHookParams {
            run: Some(run.into()),
            ..told_as("claude", sid, "Stop", None, None)
        };
        let wants = store.told(under("ours", "r-claude"), &ledger).await.unwrap();
        assert!(!wants, "the run's own: let go, so no ps");
        let wants = store.told(under("in-shell", "r-shell"), &ledger).await.unwrap();
        assert!(wants, "kept with no process known");
        store.told(under("unknown-run", "r-gone"), &ledger).await.unwrap();
        let wants = store.told(under("after-its-run", "r-ended"), &ledger).await.unwrap();
        assert!(wants, "an ended run's: kept, one of Claude's own");
        let keys: Vec<String> = store.latest(true).iter().map(Outside::key).collect();
        assert_eq!(
            keys,
            ["o:claude:after-its-run", "o:claude:in-shell", "o:claude:unknown-run"]
        );
    }

    #[tokio::test]
    async fn a_hook_of_a_claude_session_not_listed_calls_for_one_ask() {
        let ledger = Ledger::open_in_memory().unwrap();
        let store = Outsiders::new(Arc::new(Registry::builtin()), Some(Polling::default()));
        let asker = &store.listers[0].asker;
        let t0 = Instant::now();
        asker.began(t0);
        asker.listed(HashSet::from(["o:claude:known".to_string()]));
        for sid in ["known", "new", "new"] {
            store
                .told(told_as("claude", sid, "Stop", None, None), &ledger)
                .await
                .unwrap();
        }
        store
            .told(told_as("codex", "x", "Stop", None, None), &ledger)
            .await
            .unwrap();
        assert_eq!(asker.due_in(t0 + lister::BETWEEN, false), Duration::ZERO);
        asker.began(t0 + lister::BETWEEN);
        assert_eq!(
            asker.due_in(t0 + lister::BETWEEN * 2, false),
            lister::UNHOOKED - lister::BETWEEN,
            "once: then only the fallback, the hook not being there"
        );
    }
}
