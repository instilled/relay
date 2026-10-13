//! Asking an agent for its own sessions: told by its hooks as every
//! agent's are (`hooked`), and filled in, for an agent whose profile says
//! how it lists them (`sessions.list`: Claude's `claude agents --json`), by
//! asking it now and then, in the background (`Outsiders::serve`) — never
//! on a digest's path. What it says is read by one of the readers here,
//! named by the profile (`reader`).
//!
//! A hook cannot say all of it: whether a session is in the agent's
//! background, its attach `id`, its `name`, its `pid` and when it started;
//! nor can it tell of a session that began before the hook was there, or
//! before `rld` was. The listing says those, but costs a start of the agent
//! (`claude agents`: a Node start, some 0.13s and 160 MB) each time, so it
//! is asked only with a reason (`Asker`), and only while something is
//! showing the list:
//! - the first time the list is wanted in a while;
//! - when a hook tells of a session the last ask did not list (once for
//!   each, and no more than once every `BETWEEN`);
//! - every `BACKGROUND` while one of the agent's background sessions is
//!   listed: whether Claude runs user-level hooks in those is not
//!   documented, and this is the safety net;
//! - every `UNHOOKED` while the agent's user-level hook is not in its file
//!   (read once every `RECHECK` at most), as nothing else would tell.
//!
//! A failed ask leaves the list as it was. Nothing is asked while the
//! profile's binary is not on this machine.

use anyhow::{Context, Result, anyhow, bail};
use rl_detect::Registry;
use rl_detect::sessions::UserHooks;
use rl_detect::user_hooks;
use rl_proto::methods::Outside;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// The `kind` a listing gives a session in its agent's background.
pub const BACKGROUND_KIND: &str = "background";
/// How often `serve` wakes, to let go of sessions that ended without a
/// word and to see whether an ask is due.
pub const TICK: Duration = Duration::from_secs(5);
/// The least time from one ask to the next that a hook called for.
pub const BETWEEN: Duration = Duration::from_secs(3);
/// How often an agent is asked while one of its background sessions is
/// listed.
pub const BACKGROUND: Duration = Duration::from_secs(60);
/// How often it is asked while its user-level hook is not there.
pub const UNHOOKED: Duration = Duration::from_secs(30);
/// How often its hooks file is read for the hook, at most.
pub const RECHECK: Duration = Duration::from_secs(60);
/// How long one ask may take.
pub const WAIT: Duration = Duration::from_secs(10);
/// `RELAY_CLAUDE_SESSIONS=off` keeps `rld` from asking any agent; hooks
/// still tell. The repo's `.cargo/config.toml` sets it for everything cargo
/// runs — the tests, the `rl` and `rld` they start, and `cargo run` too,
/// where `RELAY_CLAUDE_SESSIONS=on cargo run --bin rld` asks anyway (cargo
/// leaves a variable already set alone). Any other value, or none, asks.
const OFF: &str = "RELAY_CLAUDE_SESSIONS";

/// How a listing reads: the agent's output, as sessions.
pub type Reader = fn(&str) -> Result<Vec<Outside>>;

/// The reader a profile's `sessions.list.reader` names, of those
/// `rl_detect::sessions::READERS` lists.
pub fn reader(name: &str) -> Option<Reader> {
    match name {
        "claude-agents" => Some(super::claude::parse),
        _ => None,
    }
}

/// When agents are asked, how long an ask may take, and the environment
/// their hooks files are placed from.
#[derive(Debug, Clone)]
pub struct Polling {
    pub tick: Duration,
    pub between: Duration,
    pub background: Duration,
    pub unhooked: Duration,
    pub recheck: Duration,
    pub wait: Duration,
    /// Reads a variable of the environment each agent's hooks file is
    /// placed from (`UserHooks::file`): `rld`'s own as it started. An `rl
    /// setup` run where that says otherwise writes a file `rld` does not
    /// read, and `rld` asks every `unhooked` as if there were no hook. By
    /// default none is set, and no file is placed: a profile that names a
    /// whole path still has it.
    pub env: fn(&str) -> Option<OsString>,
}

impl Default for Polling {
    fn default() -> Self {
        Self {
            tick: TICK,
            between: BETWEEN,
            background: BACKGROUND,
            unhooked: UNHOOKED,
            recheck: RECHECK,
            wait: WAIT,
            env: |_| None,
        }
    }
}

impl Polling {
    /// The default, with the hooks files where this environment places
    /// them — unless `RELAY_CLAUDE_SESSIONS=off`.
    pub fn from_env() -> Option<Self> {
        match std::env::var(OFF) {
            Ok(v) if v == "off" => None,
            _ => Some(Self {
                env: |k| std::env::var_os(k),
                ..Self::default()
            }),
        }
    }
}

/// One agent that lists its own sessions, and when it is asked.
pub(crate) struct Lister {
    pub agent: String,
    args: Vec<String>,
    read: Reader,
    pub asker: Asker,
}

impl Lister {
    /// Each agent of the `registry` whose profile says how it lists its
    /// sessions, with a reader `rld` has, asked as `polling` says.
    pub fn all(registry: &Registry, polling: &Polling) -> Vec<Self> {
        registry
            .listing()
            .filter_map(|p| {
                let s = p.sessions.as_ref()?;
                let list = s.list.as_ref()?;
                Some(Self {
                    agent: p.name.clone(),
                    args: list.args.clone(),
                    read: reader(&list.reader)?,
                    asker: Asker::new(polling.clone(), s.hooks.clone()),
                })
            })
            .collect()
    }

    /// Asks the agent for its sessions, for no longer than `wait`.
    pub async fn hear(&self, registry: &Registry, wait: Duration) -> Result<Vec<Outside>, NotHeard> {
        let bin = agent_bin(registry, &self.agent).map_err(|why| NotHeard { missing: true, why })?;
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        let text = ask(&bin, &args, wait)
            .await
            .map_err(|why| NotHeard { missing: false, why })?;
        let mut list = (self.read)(&text).map_err(|why| NotHeard { missing: false, why })?;
        for s in &mut list {
            s.agent = self.agent.clone();
        }
        Ok(list)
    }
}

/// Why an agent was not heard.
pub(crate) struct NotHeard {
    /// It is not here: how many machines are, not worth a warning.
    pub missing: bool,
    pub why: anyhow::Error,
}

#[derive(Default)]
struct Asks {
    /// When the last ask began.
    began: Option<Instant>,
    /// The sessions the last good ask listed, by key.
    listed: HashSet<String>,
    /// An ask is called for: the list wanted afresh, or a hook told of a
    /// session not listed.
    wanted: bool,
    /// The sessions not listed that an ask was called for already.
    called: HashSet<String>,
    /// The last ask failed: the next comes later than a hook's would.
    failed: bool,
    /// Whether the hooks file had Relay's hook, and when that was read.
    hooked: Option<(bool, Instant)>,
    /// Why the agent was not heard last time, in words that stay put
    /// (`error_key`): logged only when those change.
    error: Option<String>,
}

/// When to ask an agent for its sessions, and what the last ask said.
pub(crate) struct Asker {
    polling: Polling,
    /// The agent's own hooks, whose being there spares asking; and the file
    /// they are in, as `polling` places it.
    hooks: Option<(UserHooks, PathBuf)>,
    asks: Mutex<Asks>,
}

impl Asker {
    pub fn new(polling: Polling, hooks: Option<UserHooks>) -> Self {
        let hooks = hooks.and_then(|h| {
            let file = h.file(polling.env)?;
            Some((h, file))
        });
        Self {
            polling,
            hooks,
            asks: Mutex::default(),
        }
    }

    pub fn polling(&self) -> &Polling {
        &self.polling
    }

    /// The list is wanted afresh: an ask is called for.
    pub fn want(&self) {
        self.asks.lock().unwrap().wanted = true;
    }

    /// A hook told of the session `key`: whether that calls for an ask —
    /// the last one did not list it, and none was called for it yet.
    pub fn told_of(&self, key: &str) -> bool {
        let mut a = self.asks.lock().unwrap();
        if a.listed.contains(key) || !a.called.insert(key.to_string()) {
            return false;
        }
        a.wanted = true;
        true
    }

    /// The session `key` is gone: a hook that tells of it again calls for
    /// an ask again.
    pub fn forget(&self, key: &str) {
        self.asks.lock().unwrap().called.remove(key);
    }

    /// How long until an ask is due, from `now`, while the list is showing:
    /// zero when it is due now, `Duration::MAX` when nothing calls for one.
    /// `background`: whether one of the agent's background sessions is
    /// listed. Whether its hook is there is read from its file, no more
    /// than once every `recheck`.
    pub fn due_in(&self, now: Instant, background: bool) -> Duration {
        let p = &self.polling;
        let mut a = self.asks.lock().unwrap();
        let hooked = match a.hooked {
            Some((hooked, at)) if now.saturating_duration_since(at) < p.recheck => hooked,
            _ => {
                let hooked = self.read_hooked();
                a.hooked = Some((hooked, now));
                hooked
            }
        };
        let since = a.began.map(|b| now.saturating_duration_since(b));
        let after = |every: Duration| since.map_or(Duration::ZERO, |s| every.saturating_sub(s));
        [
            a.wanted
                .then(|| after(if a.failed { p.between.max(p.unhooked) } else { p.between })),
            background.then(|| after(p.background)),
            (!hooked).then(|| after(p.unhooked)),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(Duration::MAX)
    }

    /// Whether the agent's hooks file has Relay's hook in it, as it reads.
    /// A file that does not read has none that runs.
    fn read_hooked(&self) -> bool {
        self.hooks
            .as_ref()
            .is_some_and(|(h, f)| user_hooks::installed(f, h).unwrap_or(false))
    }

    /// An ask begins now: what called for it is answered.
    pub fn began(&self, at: Instant) {
        let mut a = self.asks.lock().unwrap();
        a.began = Some(at);
        a.wanted = false;
    }

    /// What the ask heard: its sessions, by key. A hook that called for an
    /// ask while this one was on its way is answered by it, where it
    /// listed that session.
    pub fn listed(&self, listed: HashSet<String>) {
        let mut a = self.asks.lock().unwrap();
        if a.error.take().is_some() {
            info!(sessions = listed.len(), "agent sessions: listed again");
        }
        a.failed = false;
        a.called.retain(|k| !listed.contains(k));
        if a.called.is_empty() {
            a.wanted = false;
        }
        a.listed = listed;
    }

    /// Says once why nothing was heard; the list stands as it was. What
    /// called for the ask still does, so it is tried again — after
    /// `unhooked` at the least, so a broken agent is not asked every few
    /// seconds — and a hook may call for one again. Not where the agent is
    /// not here at all.
    pub fn not_heard(&self, NotHeard { missing, why }: NotHeard) {
        let mut a = self.asks.lock().unwrap();
        if !missing {
            a.wanted = true;
            a.failed = true;
            a.called.clear();
        }
        let why = format!("{why:#}");
        let key = error_key(&why).to_string();
        if a.error.as_deref() != Some(key.as_str()) {
            if missing {
                debug!(error = %why, "agent sessions not listed");
            } else {
                warn!(error = %why, "agent sessions not listed; the list stands as it was");
            }
            a.error = Some(key);
        }
    }
}

/// What kind of failure, without what changes from one to the next (a
/// timestamp in the agent's stderr, say): the words before the first `: `.
/// `ask`'s and the readers' errors all say what failed first.
fn error_key(why: &str) -> &str {
    why.split(": ").next().unwrap_or(why)
}

/// The `agent` profile's binary, where it is on this machine.
pub(crate) fn agent_bin(registry: &Registry, agent: &str) -> Result<PathBuf> {
    let p = registry.profile(agent).ok_or_else(|| anyhow!("no {agent} profile"))?;
    rl_detect::registry::find_bin(&p.bins).ok_or_else(|| anyhow!("`{agent}` is not on this machine"))
}

/// Runs the agent's `bin` with `args`, for no longer than `wait`; its
/// output. Its hooks, should it fire any within the next 30s, know it for
/// `rld`'s (`user_hooks::ASKING`). Its errors name it by its binary's name
/// (`claude agents --json`).
pub(crate) async fn ask(bin: &Path, args: &[&str], wait: Duration) -> Result<String> {
    let name = bin
        .file_name()
        .map_or_else(|| bin.to_string_lossy(), |n| n.to_string_lossy());
    let what = std::iter::once(name.as_ref())
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    let out = tokio::time::timeout(
        wait,
        tokio::process::Command::new(bin)
            .args(args)
            // Any hook it fires is not a session to tell of.
            .env(user_hooks::ASKING, user_hooks::asking_since())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow!("`{what}` did not answer in time"))?
    .with_context(|| format!("running `{what}`"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("`{what}` failed: {}", err.lines().next().unwrap_or("no reason given"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reader_a_profile_may_name_is_here() {
        for name in rl_detect::sessions::READERS {
            assert!(reader(name).is_some(), "{name}");
        }
        assert!(reader("tea-leaves").is_none());
    }

    #[test]
    fn no_agent_here_is_said_once() {
        let asker = Asker::new(Polling::default(), None);
        asker.not_heard(NotHeard {
            missing: true,
            why: anyhow!("`claude` is not on this machine"),
        });
        assert!(asker.asks.lock().unwrap().error.is_some(), "said once, at debug");
    }

    const S: Duration = Duration::from_secs(1);

    /// Claude's hooks, their file at `settings`.
    fn hooks_at(settings: &Path) -> UserHooks {
        let mut h = Registry::builtin().user_hooks("claude").unwrap().clone();
        h.file = vec![settings.to_string_lossy().into_owned()];
        h
    }

    /// An asker of Claude's with the default intervals, its hook there or
    /// not.
    fn asker(hooked: bool) -> (tempfile::TempDir, Asker) {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = hooks_at(&tmp.path().join("settings.json"));
        if hooked {
            user_hooks::install(&tmp.path().join("settings.json"), &hooks, Path::new("/x/rl")).unwrap();
        }
        let asker = Asker::new(Polling::default(), Some(hooks));
        (tmp, asker)
    }

    #[test]
    fn with_the_hook_there_it_asks_once_when_wanted_and_then_only_with_a_reason() {
        let (_tmp, asker) = asker(true);
        let t0 = Instant::now();
        assert_eq!(asker.due_in(t0, false), Duration::MAX, "nothing calls for one");
        asker.want();
        assert_eq!(asker.due_in(t0, false), Duration::ZERO, "the first in a while: now");
        asker.began(t0);
        asker.listed(HashSet::from(["o:claude:a".to_string()]));
        assert_eq!(asker.due_in(t0 + 50 * S, false), Duration::MAX);

        // A hook tells of one listed: nothing. Of one not: an ask, once,
        // and not sooner than `BETWEEN` after the last.
        assert!(!asker.told_of("o:claude:a"));
        assert!(asker.told_of("o:claude:b"));
        assert!(!asker.told_of("o:claude:b"), "called for already");
        assert_eq!(asker.due_in(t0 + S, false), BETWEEN - S);
        assert_eq!(asker.due_in(t0 + 10 * S, false), Duration::ZERO);
        asker.began(t0 + 10 * S);
        asker.listed(HashSet::from(["o:claude:a".to_string()]));
        assert!(!asker.told_of("o:claude:b"), "asked for it once; not listed still");
        asker.forget("o:claude:b");
        assert!(asker.told_of("o:claude:b"), "gone, and told of again");

        // A background session listed: about once a minute.
        let (_tmp, asker) = self::asker(true);
        asker.began(t0);
        assert_eq!(asker.due_in(t0 + 20 * S, true), BACKGROUND - 20 * S);
        assert_eq!(asker.due_in(t0 + BACKGROUND, true), Duration::ZERO);
    }

    #[test]
    fn a_failed_ask_is_tried_again_later_but_no_agent_here_is_not() {
        let (_tmp, asker) = asker(true);
        let t0 = Instant::now();
        asker.want();
        asker.began(t0);
        asker.not_heard(NotHeard {
            missing: false,
            why: anyhow!("`claude agents --json` did not answer in time"),
        });
        assert_eq!(
            asker.due_in(t0 + S, false),
            UNHOOKED - S,
            "again, not as soon as a hook's"
        );
        asker.began(t0 + UNHOOKED);
        asker.listed(HashSet::new());
        assert_eq!(asker.due_in(t0 + UNHOOKED + S, false), Duration::MAX, "heard: done");

        // One a hook called for that failed: the hook may call again.
        assert!(asker.told_of("o:claude:x"));
        asker.began(t0 + 2 * UNHOOKED);
        asker.not_heard(NotHeard {
            missing: false,
            why: anyhow!("`claude agents --json` failed: it broke"),
        });
        assert!(asker.told_of("o:claude:x"), "it may call for one again");
        asker.began(t0 + 3 * UNHOOKED);
        asker.listed(HashSet::from(["o:claude:x".to_string()]));
        assert_eq!(asker.due_in(t0 + 3 * UNHOOKED + S, false), Duration::MAX);

        // No `claude` here: nothing to try again.
        let (_tmp, asker) = self::asker(true);
        asker.want();
        asker.began(t0);
        asker.not_heard(NotHeard {
            missing: true,
            why: anyhow!("`claude` is not on this machine"),
        });
        assert_eq!(asker.due_in(t0 + 10 * UNHOOKED, false), Duration::MAX);
    }

    #[test]
    fn a_hook_answered_by_the_ask_on_its_way_calls_for_no_other() {
        let (_tmp, asker) = asker(true);
        let t0 = Instant::now();
        asker.want();
        asker.began(t0);
        // Its first word lands while the ask is on its way, which lists it.
        assert!(asker.told_of("o:claude:new"));
        asker.listed(HashSet::from(["o:claude:new".to_string()]));
        assert_eq!(asker.due_in(t0 + S, false), Duration::MAX);
    }

    #[test]
    fn without_the_hook_it_asks_every_half_minute_and_reads_the_file_once_a_minute() {
        let (tmp, asker) = asker(false);
        let t0 = Instant::now();
        assert_eq!(asker.due_in(t0, false), Duration::ZERO);
        asker.began(t0);
        assert_eq!(asker.due_in(t0 + 10 * S, false), UNHOOKED - 10 * S);
        assert_eq!(asker.due_in(t0 + UNHOOKED, false), Duration::ZERO);
        // The hook added: not seen until the file is read again.
        let settings = tmp.path().join("settings.json");
        user_hooks::install(&settings, &hooks_at(&settings), Path::new("/x/rl")).unwrap();
        asker.began(t0 + UNHOOKED);
        assert_eq!(asker.due_in(t0 + RECHECK - S, false), S, "as read at t0: not there");
        assert_eq!(
            asker.due_in(t0 + RECHECK + S, false),
            Duration::MAX,
            "read again after a minute"
        );
    }

    #[test]
    fn an_agent_with_no_hooks_file_placed_is_asked_as_unhooked() {
        // The built-in's file is placed from the environment; none is set.
        let hooks = Registry::builtin().user_hooks("claude").cloned();
        let asker = Asker::new(Polling::default(), hooks);
        let t0 = Instant::now();
        asker.began(t0);
        assert_eq!(asker.due_in(t0 + S, false), UNHOOKED - S);
    }

    #[tokio::test]
    async fn a_registry_with_no_claude_binary_is_not_heard_as_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("no-claude-here");
        std::fs::write(
            tmp.path().join("claude.yaml"),
            format!("bins: [\"{}\"]\n", gone.display()),
        )
        .unwrap();
        let reg = Registry::load_from(tmp.path()).unwrap();
        let listers = Lister::all(&reg, &Polling::default());
        assert_eq!(
            listers.iter().map(|l| l.agent.as_str()).collect::<Vec<_>>(),
            ["claude"],
            "only Claude's profile says how it lists them"
        );
        let Err(e) = listers[0].hear(&reg, WAIT).await else {
            panic!("heard a claude that is not there");
        };
        assert!(e.missing, "{:#}", e.why);
    }

    #[test]
    fn an_error_is_keyed_by_what_failed_not_by_what_it_said() {
        let a = "`claude agents --json` failed: 12:00:01 socket gone";
        let b = "`claude agents --json` failed: 12:00:06 socket gone";
        assert_eq!(error_key(a), error_key(b));
        assert_ne!(error_key(a), error_key("`claude agents --json` did not answer in time"));
        assert_eq!(
            error_key("reading `claude agents --json`: expected value"),
            "reading `claude agents --json`"
        );
    }
}
