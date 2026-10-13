//! Claude's own sessions: told by its user-level hook, as Codex's and
//! Gemini's are (`hooked`), and filled in by asking `claude agents --json`
//! now and then, in the background (`Outsiders::serve`) — never on a
//! digest's path.
//!
//! A hook cannot say all of it: whether a session is in Claude's
//! background, its attach `id`, its `name`, its `pid` and when it started;
//! nor can it tell of a session that began before the hook was there, or
//! before `rld` was. `claude agents` says those, but costs a Node start
//! (some 0.13s and 160 MB) each time, so it is asked only with a reason
//! (`Asker`), and only while something is showing the list:
//! - the first time the list is wanted in a while;
//! - when a hook tells of a session the last ask did not list (once for
//!   each, and no more than once every `BETWEEN`);
//! - every `BACKGROUND` while one of Claude's background sessions is
//!   listed: whether Claude runs user-level hooks in those is not
//!   documented, and this is the safety net;
//! - every `UNHOOKED` while Claude's user-level hook is not in its
//!   settings (read once every `RECHECK` at most), as nothing else would
//!   tell.
//!
//! A failed ask leaves the list as it was. Nothing is asked while the
//! claude profile's binary is not on this machine.
//!
//! `rld` spawns no `claude` of its own beyond `claude agents` and `claude
//! stop`, which are not sessions: every other is a run (see `outside`).

use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use rl_detect::Registry;
use rl_detect::user_hooks;
use rl_proto::methods::Outside;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// The profile whose sessions these are.
pub const AGENT: &str = user_hooks::CLAUDE;
/// The `kind` Claude gives a session in its background.
pub const BACKGROUND_KIND: &str = "background";
/// How often `serve` wakes, to let go of sessions that ended without a
/// word and to see whether an ask is due.
pub const TICK: Duration = Duration::from_secs(5);
/// The least time from one ask to the next that a hook called for.
pub const BETWEEN: Duration = Duration::from_secs(3);
/// How often `claude` is asked while a background session is listed.
pub const BACKGROUND: Duration = Duration::from_secs(60);
/// How often it is asked while Claude's user-level hook is not there.
pub const UNHOOKED: Duration = Duration::from_secs(30);
/// How often Claude's settings are read for the hook, at most.
pub const RECHECK: Duration = Duration::from_secs(60);
/// How long one ask may take.
pub const WAIT: Duration = Duration::from_secs(10);
/// `RELAY_CLAUDE_SESSIONS=off` keeps `rld` from asking; hooks still tell.
/// The repo's `.cargo/config.toml` sets it for everything cargo runs — the
/// tests, the `rl` and `rld` they start, and `cargo run` too, where
/// `RELAY_CLAUDE_SESSIONS=on cargo run --bin rld` asks anyway (cargo leaves
/// a variable already set alone). Any other value, or none, asks.
const OFF: &str = "RELAY_CLAUDE_SESSIONS";

/// When `claude agents` is asked, and how long an ask may take.
#[derive(Debug, Clone)]
pub struct Polling {
    pub tick: Duration,
    pub between: Duration,
    pub background: Duration,
    pub unhooked: Duration,
    pub recheck: Duration,
    pub wait: Duration,
    /// Claude's user-level settings, where its hook is when `rl setup`
    /// added it; none, and it is taken as not there. Placed from `rld`'s
    /// own environment as it started (`CLAUDE_CONFIG_DIR`, `HOME`): an `rl
    /// setup` run where that says otherwise writes a file `rld` does not
    /// read, and `rld` asks every `unhooked` as if there were no hook.
    pub settings: Option<PathBuf>,
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
            settings: None,
        }
    }
}

impl Polling {
    /// The default, with Claude's settings where this environment places
    /// them — unless `RELAY_CLAUDE_SESSIONS=off`.
    pub fn from_env() -> Option<Self> {
        match std::env::var(OFF) {
            Ok(v) if v == "off" => None,
            _ => Some(Self {
                settings: user_hooks::file(AGENT, |k| std::env::var_os(k)),
                ..Self::default()
            }),
        }
    }
}

/// Why `claude` was not heard.
pub(crate) struct NotHeard {
    /// No `claude` here: how many machines are, not worth a warning.
    missing: bool,
    why: anyhow::Error,
}

/// Asks `claude agents --json`, for no longer than `wait`.
pub(crate) async fn hear(registry: &Registry, wait: Duration) -> Result<Vec<Outside>, NotHeard> {
    let bin = claude_bin(registry).map_err(|why| NotHeard { missing: true, why })?;
    let text = ask(&bin, &["agents", "--json"], wait)
        .await
        .map_err(|why| NotHeard { missing: false, why })?;
    parse(&text).map_err(|why| NotHeard { missing: false, why })
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
    /// Whether Claude's settings had Relay's hook, and when that was read.
    hooked: Option<(bool, Instant)>,
    /// Why `claude` was not heard last time, in words that stay put
    /// (`error_key`): logged only when those change.
    error: Option<String>,
}

/// When to ask `claude agents`, and what the last ask said.
pub(crate) struct Asker {
    polling: Polling,
    asks: Mutex<Asks>,
}

impl Asker {
    pub fn new(polling: Polling) -> Self {
        Self {
            polling,
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
    /// `background`: whether one of Claude's background sessions is
    /// listed. Whether Claude's hook is there is read from its settings,
    /// no more than once every `recheck`.
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

    /// Whether Claude's settings have Relay's hook in them, as they read.
    /// A file that does not read has none that runs.
    fn read_hooked(&self) -> bool {
        self.polling
            .settings
            .as_deref()
            .is_some_and(|f| user_hooks::installed(f, AGENT).unwrap_or(false))
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
            info!(sessions = listed.len(), "claude sessions: listed again");
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
    /// `unhooked` at the least, so a broken `claude` is not asked every few
    /// seconds — and a hook may call for one again. Not where there is no
    /// `claude` here at all.
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
                debug!(error = %why, "claude sessions not listed");
            } else {
                warn!(error = %why, "claude sessions not listed; the list stands as it was");
            }
            a.error = Some(key);
        }
    }
}

/// What kind of failure, without what changes from one to the next (a
/// timestamp in Claude's stderr, say): the words before the first `: `.
/// `ask`'s and `parse`'s errors all say what failed first.
fn error_key(why: &str) -> &str {
    why.split(": ").next().unwrap_or(why)
}

/// A line of `claude agents --json`, as Claude writes it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listed {
    session_id: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    cwd: PathBuf,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
    /// Unix milliseconds.
    #[serde(default)]
    started_at: Option<i64>,
    #[serde(default)]
    waiting_for: Option<String>,
}

/// `claude agents --json` as sessions. An entry that does not read — no
/// `sessionId`, no `cwd` — is skipped, not the whole list; values are kept
/// as Claude writes them, ones Relay does not know yet too.
pub(crate) fn parse(text: &str) -> Result<Vec<Outside>> {
    let listed: Vec<serde_json::Value> = serde_json::from_str(text).context("reading `claude agents --json`")?;
    Ok(listed
        .into_iter()
        .filter_map(|v| match serde_json::from_value::<Listed>(v) {
            Ok(l) => Some(l),
            Err(e) => {
                debug!(error = %e, "claude agents: an entry skipped");
                None
            }
        })
        // A folder it is in is a whole path: an empty or relative one
        // would make a group that is no folder.
        .filter(|l| !l.session_id.is_empty() && l.cwd.is_absolute())
        .map(|l| {
            let name = l
                .name
                .filter(|n| !n.trim().is_empty() && !n.starts_with("<<"))
                .unwrap_or_else(|| {
                    l.cwd
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_else(|| l.session_id.clone())
                });
            Outside {
                agent: AGENT.into(),
                session_id: l.session_id,
                id: l.id,
                name,
                cwd: l.cwd,
                kind: l.kind.unwrap_or_default(),
                status: l.status.unwrap_or_default(),
                state: l.state,
                pid: l.pid,
                started_at: l.started_at.and_then(|ms| Timestamp::from_millisecond(ms).ok()),
                waiting_for: l.waiting_for,
                run: None,
                new: false,
                workspace: None,
                state_at: None,
                was: None,
            }
        })
        .collect())
}

/// The claude profile's binary, where it is on this machine.
pub(crate) fn claude_bin(registry: &Registry) -> Result<PathBuf> {
    let p = registry.profile(AGENT).ok_or_else(|| anyhow!("no claude profile"))?;
    rl_detect::registry::find_bin(&p.bins).ok_or_else(|| anyhow!("`claude` is not on this machine"))
}

/// Runs `claude` with `args`, for no longer than `wait`; its output. Its
/// hooks, should it fire any within the next 30s, know it for `rld`'s
/// (`user_hooks::ASKING`).
pub(crate) async fn ask(bin: &Path, args: &[&str], wait: Duration) -> Result<String> {
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
    .map_err(|_| anyhow!("`claude {}` did not answer in time", args.join(" ")))?
    .with_context(|| format!("running `claude {}`", args.join(" ")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "`claude {}` failed: {}",
            args.join(" "),
            err.lines().next().unwrap_or("no reason given")
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_agents_json_reads_as_sessions() {
        let text = r#"[
          {"pid":1,"id":"8af077cd","cwd":"/w/app","kind":"background","startedAt":1789789820657,
           "sessionId":"8af077cd-5fe3","name":"asset list","status":"idle","state":"done"},
          {"pid":2,"cwd":"/w/relay","kind":"interactive","startedAt":1789821153884,
           "sessionId":"42fdfb2d","name":"ground work","status":"busy"},
          {"pid":3,"id":"a8b1e728","cwd":"/w/market","kind":"background",
           "sessionId":"0d06b838","name":"<<placeholder>>","status":"busy","state":"working"}
        ]"#;
        let s = parse(text).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(
            (s[0].id.as_deref(), s[0].name.as_str()),
            (Some("8af077cd"), "asset list")
        );
        assert_eq!(
            (s[0].agent.as_str(), s[0].key()),
            ("claude", "o:claude:8af077cd-5fe3".into())
        );
        assert!(!s[0].in_a_terminal() && !s[0].working());
        assert!(s[1].in_a_terminal() && s[1].working());
        assert_eq!(s[2].name, "market", "a placeholder name gives way to the folder's");
        assert!(s[2].working());
    }

    #[test]
    fn the_new_fields_read_and_what_does_not_is_skipped() {
        let text = r#"[
          {"pid":41,"id":"b1","cwd":"/w/app","kind":"background","startedAt":1789789820657,
           "sessionId":"s-one","status":"waiting","waitingFor":"permission prompt","state":"pondering",
           "somethingNew":{"x":1}},
          {"id":"b2","cwd":"/w/app","kind":"background","state":"done"},
          {"cwd":"/w/app","kind":"interactive","sessionId":"","status":"idle"},
          {"sessionId":"s-nocwd","kind":"interactive"},
          {"cwd":"","kind":"interactive","sessionId":"s-emptycwd","status":"idle"},
          {"cwd":"w/rel","kind":"interactive","sessionId":"s-relcwd","status":"idle"},
          "not even an object",
          {"cwd":"/w/gone","kind":"background","sessionId":"s-two","id":"b3","state":"stopped"}
        ]"#;
        let s = parse(text).unwrap();
        assert_eq!(
            s.iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(),
            ["s-one", "s-two"],
            "no sessionId, an empty one, no cwd, an empty or relative one, not an object: skipped"
        );
        let one = &s[0];
        assert_eq!(one.pid, Some(41));
        assert_eq!(
            one.started_at,
            Some(Timestamp::from_millisecond(1789789820657).unwrap())
        );
        assert_eq!(one.status, "waiting");
        assert_eq!(one.waiting_for.as_deref(), Some("permission prompt"));
        assert_eq!(
            one.state.as_deref(),
            Some("pondering"),
            "a state not known yet, kept as said"
        );
        let two = &s[1];
        assert_eq!((two.pid, two.started_at, two.status.as_str()), (None, None, ""));
        assert!(parse("{}").is_err(), "not a list at all");
        let k = parse(r#"[{"cwd":"/w","sessionId":"s"}]"#).unwrap().remove(0);
        assert_eq!(k.kind, "", "no kind given, none assumed");
        assert!(k.in_a_terminal(), "no id: nothing to attach to");
    }

    #[test]
    fn no_claude_here_is_said_once() {
        let asker = Asker::new(Polling::default());
        asker.not_heard(NotHeard {
            missing: true,
            why: anyhow!("`claude` is not on this machine"),
        });
        assert!(asker.asks.lock().unwrap().error.is_some(), "said once, at debug");
    }

    const S: Duration = Duration::from_secs(1);

    /// An asker with the default intervals, its hook there or not.
    fn asker(hooked: bool) -> (tempfile::TempDir, Asker) {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        if hooked {
            user_hooks::install(&settings, AGENT, Path::new("/x/rl")).unwrap();
        }
        let asker = Asker::new(Polling {
            settings: Some(settings),
            ..Polling::default()
        });
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
    fn a_failed_ask_is_tried_again_later_but_no_claude_here_is_not() {
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
        user_hooks::install(&tmp.path().join("settings.json"), AGENT, Path::new("/x/rl")).unwrap();
        asker.began(t0 + UNHOOKED);
        assert_eq!(asker.due_in(t0 + RECHECK - S, false), S, "as read at t0: not there");
        assert_eq!(
            asker.due_in(t0 + RECHECK + S, false),
            Duration::MAX,
            "read again after a minute"
        );
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
        let Err(e) = hear(&reg, WAIT).await else {
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
