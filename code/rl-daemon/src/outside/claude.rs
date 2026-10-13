//! Claude's own sessions: what `claude agents --json` says, asked in the
//! background (`Outsiders::serve`) and kept here, so a digest only reads
//! what was last heard and never waits on `claude`.
//!
//! Asked only while something is showing the list, and only while the
//! claude profile's binary is on this machine. A failed ask keeps the last
//! good list for a grace period, then the list is empty until `claude`
//! answers again.
//!
//! So a digest after a quiet minute has none yet, and a one-off one (`rl
//! since`) never has them: only a panel, asking every second, keeps the list
//! filled. Asking `claude` from the digest instead would make every digest
//! wait on it.
//!
//! `rld` spawns no `claude` of its own beyond `claude agents` and `claude
//! stop`, which are not sessions: every other is a run (see `outside`).

use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use rl_detect::Registry;
use rl_proto::methods::Outside;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// The profile whose sessions these are.
pub const AGENT: &str = "claude";
/// How often `claude agents --json` is asked while the list is showing.
pub const EVERY: Duration = Duration::from_secs(5);
/// How long the last good list stands while `claude` fails to answer.
pub const GRACE: Duration = Duration::from_secs(30);
/// How long one ask may take.
pub const WAIT: Duration = Duration::from_secs(10);
/// `RELAY_CLAUDE_SESSIONS=off` keeps `rld` from asking. The repo's
/// `.cargo/config.toml` sets it for everything cargo runs — the tests, the
/// `rl` and `rld` they start, and `cargo run` too, where
/// `RELAY_CLAUDE_SESSIONS=on cargo run --bin rld` asks anyway (cargo leaves
/// a variable already set alone). Any other value, or none, asks.
const OFF: &str = "RELAY_CLAUDE_SESSIONS";

/// How often to ask, how long a list outlives a failure, and how long one
/// ask may take.
#[derive(Debug, Clone, Copy)]
pub struct Polling {
    pub every: Duration,
    pub grace: Duration,
    pub wait: Duration,
}

impl Default for Polling {
    fn default() -> Self {
        Self {
            every: EVERY,
            grace: GRACE,
            wait: WAIT,
        }
    }
}

impl Polling {
    /// The default, unless `RELAY_CLAUDE_SESSIONS=off`.
    pub fn from_env() -> Option<Self> {
        match std::env::var(OFF) {
            Ok(v) if v == "off" => None,
            _ => Some(Self::default()),
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
struct List {
    list: Vec<Outside>,
    /// When `claude` last answered.
    heard: Option<Instant>,
    /// Why it did not, last time, in words that stay put (`error_key`):
    /// logged only when those change.
    error: Option<String>,
}

/// Claude's sessions as last heard.
pub(crate) struct Kept {
    polling: Polling,
    kept: Mutex<List>,
}

impl Kept {
    pub fn new(polling: Polling) -> Self {
        Self {
            polling,
            kept: Mutex::default(),
        }
    }

    pub fn every(&self) -> Duration {
        self.polling.every
    }

    pub fn wait(&self) -> Duration {
        self.polling.wait
    }

    /// The sessions last heard of, if that was within the grace period.
    pub fn latest(&self) -> Vec<Outside> {
        let k = self.kept.lock().unwrap();
        match k.heard {
            Some(at) if at.elapsed() <= self.polling.grace => k.list.clone(),
            _ => vec![],
        }
    }

    /// Keeps what was heard, or says once why nothing was.
    pub fn keep(&self, heard: Result<Vec<Outside>, NotHeard>) {
        let mut k = self.kept.lock().unwrap();
        match heard {
            Ok(list) => {
                if k.error.take().is_some() {
                    info!(sessions = list.len(), "claude sessions: listed again");
                }
                if k.list != list {
                    debug!(sessions = list.len(), "claude sessions changed");
                }
                k.list = list;
                k.heard = Some(Instant::now());
            }
            Err(NotHeard { missing, why }) => {
                let why = format!("{why:#}");
                let key = error_key(&why).to_string();
                if k.error.as_deref() != Some(key.as_str()) {
                    if missing {
                        debug!(error = %why, "claude sessions not listed");
                    } else {
                        warn!(error = %why, "claude sessions not listed; the last list stands a while");
                    }
                    k.error = Some(key);
                }
            }
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

/// Runs `claude` with `args`, for no longer than `wait`; its output.
pub(crate) async fn ask(bin: &Path, args: &[&str], wait: Duration) -> Result<String> {
    let out = tokio::time::timeout(
        wait,
        tokio::process::Command::new(bin)
            .args(args)
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
    fn no_claude_here_leaves_the_list_empty() {
        let kept = Kept::new(Polling::default());
        kept.keep(Err(NotHeard {
            missing: true,
            why: anyhow!("`claude` is not on this machine"),
        }));
        assert!(kept.latest().is_empty());
        assert!(kept.kept.lock().unwrap().error.is_some(), "said once, at debug");
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
