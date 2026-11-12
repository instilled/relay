//! Drift: an expectation Relay has of a harness that failed in real use —
//! the harness changed under it (a field gone from `claude agents --json`, a
//! method its daemon no longer knows, a subcommand it no longer takes).
//!
//! Where it is noticed (`notice`) only says what was expected and what
//! happened; never anything of a conversation. What keeps it is the sink
//! `rl` installs (`set_sink`): its store under `$RELAY_HOME/drift/`, which
//! adds the versions and counts. With no sink, a notice goes nowhere — so a
//! test never writes a user's files; `capture` collects them on its thread.
//!
//! Each check has a stable id: `claude.agents_json`, `claude.bg_id`,
//! `codex.rpc.<method>`, `codex.schema.<method>`, `<harness>.cli.<word>`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// One failed expectation, as noticed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Noticed {
    /// The harness: `claude`.
    pub harness: String,
    /// Its binary, where known: what its version is asked of.
    pub bin: Option<PathBuf>,
    /// The check's stable id: `claude.agents_json`.
    pub check: String,
    /// What happened, in a line: `` `claude --bg` printed no session id ``.
    pub what: String,
    /// A structural sketch of what was seen, redacted (`redact`); never
    /// conversation content.
    pub evidence: Option<String>,
}

impl Noticed {
    pub fn new(harness: &str, bin: Option<&Path>, check: &str, what: impl Into<String>) -> Noticed {
        Noticed {
            harness: harness.to_string(),
            bin: bin.map(Path::to_path_buf),
            check: check.to_string(),
            what: what.into(),
            evidence: None,
        }
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Noticed {
        self.evidence = Some(redact(&evidence.into()));
        self
    }
}

/// What keeps a notice.
pub type Sink = Arc<dyn Fn(&Noticed) + Send + Sync>;

static SINK: RwLock<Option<Sink>> = RwLock::new(None);
/// Where the caches of versions and checks live (`$RELAY_HOME/drift`).
static DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

thread_local! {
    static CAPTURE: RefCell<Option<Vec<Noticed>>> = const { RefCell::new(None) };
}

/// Installs what keeps every notice from now on, and where caches go.
pub fn set_sink(dir: PathBuf, sink: Sink) {
    *DIR.write().unwrap_or_else(|e| e.into_inner()) = Some(dir);
    *SINK.write().unwrap_or_else(|e| e.into_inner()) = Some(sink);
}

/// Where the caches live, once a sink is installed.
pub fn dir() -> Option<PathBuf> {
    DIR.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A failed expectation: to `capture` on this thread if it collects, else
/// to the sink, else nowhere.
pub fn notice(n: Noticed) {
    let kept = CAPTURE.with(|c| match c.borrow_mut().as_mut() {
        Some(v) => {
            v.push(n.clone());
            true
        }
        None => false,
    });
    if kept {
        return;
    }
    let sink = SINK.read().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(sink) = sink {
        sink(&n);
    }
}

/// Runs `f`, collecting what it notices on this thread rather than keeping
/// it: for tests.
pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<Noticed>) {
    let before = CAPTURE.with(|c| c.borrow_mut().replace(Vec::new()));
    let r = f();
    let got = CAPTURE
        .with(|c| std::mem::replace(&mut *c.borrow_mut(), before))
        .unwrap_or_default();
    (r, got)
}

/// `s` with what could be a user's own taken out: their home folder as `~`,
/// and anything after it to the end of the path as `<project>` where it
/// names a folder under it.
pub fn redact(s: &str) -> String {
    let home = std::env::var("HOME").ok().filter(|h| h.len() > 1);
    redact_with(s, home.as_deref())
}

/// `redact`, the home folder given. A quoted span (`'…'`, `"…"`) is kept
/// only where it is one plain word — a flag or a subcommand, as a CLI
/// quotes what it did not take — else it might be a prompt: `'…'`.
pub fn redact_with(s: &str, home: Option<&str>) -> String {
    let plain = |w: &str| {
        w.len() <= 24
            && !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '/' | '_' | '.'))
    };
    let mut scrubbed = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find(['\'', '"', '`']) {
        let q = rest[at..].chars().next().unwrap_or('\'');
        let after = &rest[at + 1..];
        let Some(end) = after.find(q) else { break };
        let inner = &after[..end];
        scrubbed.push_str(&rest[..at]);
        scrubbed.push(q);
        scrubbed.push_str(if plain(inner) { inner } else { "…" });
        scrubbed.push(q);
        rest = &after[end + 1..];
    }
    scrubbed.push_str(rest);
    let s = scrubbed.as_str();
    let mut out = String::with_capacity(s.len());
    for (i, word) in s.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&redact_word(word, home));
    }
    out
}

fn redact_word(word: &str, home: Option<&str>) -> String {
    // A path starts a word, or follows punctuation (`cwd=/x`, `(/x`); a `/`
    // inside a word (`thread/read`) is no path.
    let start = word
        .char_indices()
        .find(|&(i, c)| (c == '/' || c == '~') && word[..i].chars().next_back().is_none_or(|p| !p.is_alphanumeric()))
        .map_or(word.len(), |(i, _)| i);
    let (lead, rest) = word.split_at(start);
    if rest.is_empty() {
        return word.to_string();
    }
    let trail_at = rest
        .rfind(|c: char| c.is_alphanumeric() || c == '/' || c == '_' || c == '-' || c == '.')
        .map_or(0, |i| i + rest[i..].chars().next().map_or(1, char::len_utf8));
    let (path, trail) = rest.split_at(trail_at);
    let tail = |p: &str| -> String {
        if p.trim_matches('/').is_empty() {
            p.to_string()
        } else {
            "/<project>".into()
        }
    };
    let path = match home {
        Some(h) if path == h => "~".to_string(),
        Some(h) if path.starts_with(&format!("{h}/")) => format!("~{}", tail(&path[h.len()..])),
        _ if path.starts_with("~/") => format!("~{}", tail(&path[1..])),
        _ if path.starts_with('/') => tail(path),
        _ => path.to_string(),
    };
    format!("{lead}{path}{trail}")
}

/// The version `bin` says it is (`<bin> --version`: its first word that
/// reads as one), asked once per binary and kept by its path and when it
/// was last changed — in this process, and under `dir()` across runs.
pub fn version(bin: &Path) -> Option<String> {
    static MEMO: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    let key = stamp(bin)?;
    if let Some(v) = MEMO.lock().ok()?.get_or_insert_default().get(&key) {
        return Some(v.clone());
    }
    let file = dir().map(|d| d.join("versions.json"));
    let mut kept: HashMap<String, String> = file
        .as_ref()
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let v = match kept.get(&key) {
        Some(v) => v.clone(),
        None => {
            let argv = vec![bin.to_string_lossy().into_owned(), "--version".into()];
            let said = crate::harness::run_quiet(&argv, Duration::from_secs(5)).ok()?;
            let v = version_in(&said)?;
            kept.insert(key.clone(), v.clone());
            if let Some(f) = file
                && let Ok(text) = serde_json::to_vec_pretty(&kept)
            {
                let _ = std::fs::create_dir_all(f.parent().unwrap_or(Path::new("/")));
                let _ = write_atomic(&f, &text);
            }
            v
        }
    };
    MEMO.lock().ok()?.get_or_insert_default().insert(key, v.clone());
    Some(v)
}

/// What a binary is known by while it does not change: its path, and when
/// it was last written.
pub fn stamp(bin: &Path) -> Option<String> {
    let path = std::fs::canonicalize(bin).ok()?;
    let at = std::fs::metadata(&path).ok()?.modified().ok()?;
    let secs = at.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some(format!("{}@{secs}", path.display()))
}

/// The first word of `said` that reads as a version: `2.1.330 (Claude
/// Code)` → `2.1.330`, `codex-cli 0.167.2` → `0.167.2`.
pub fn version_in(said: &str) -> Option<String> {
    said.split_whitespace()
        .map(|w| w.trim_start_matches('v').trim_end_matches([',', ')', ';']))
        .find(|w| w.contains('.') && w.starts_with(|c: char| c.is_ascii_digit()))
        .map(str::to_string)
}

/// Writes `bytes` to `path` through a file beside it, so a reader never
/// sees half of it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notice_is_captured_on_its_thread_and_goes_nowhere_without_a_sink() {
        let ((), got) = capture(|| notice(Noticed::new("claude", None, "claude.bg_id", "no id")));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].check, "claude.bg_id");
        let ((), none) = capture(|| ());
        assert!(none.is_empty());
        // No capture, no sink in tests: nothing happens.
        notice(Noticed::new("claude", None, "x", "y"));
    }

    #[test]
    fn paths_are_redacted_to_home_and_project() {
        let r = |s: &str| redact_with(s, Some("/Users/ann"));
        assert_eq!(r("in /Users/ann/w/shop: no"), "in ~/<project>: no");
        assert_eq!(r("at /Users/ann"), "at ~");
        assert_eq!(r("`~/code/x` failed"), "`…` failed");
        assert_eq!(r("~/code/x failed"), "~/<project> failed");
        assert_eq!(
            r("error: unexpected argument 'fix the login' found"),
            "error: unexpected argument '…' found",
            "what may be a prompt is not kept"
        );
        assert_eq!(r("error: unknown option '--bg'"), "error: unknown option '--bg'");
        assert_eq!(r("/opt/x/codex"), "/<project>");
        assert_eq!(r("thread/name/set gone"), "thread/name/set gone", "not a path");
        assert_eq!(r("(cwd=/tmp/a)"), "(cwd=/<project>)");
        assert!(!r("/Users/ann/secret-client/app").contains("secret"));
    }

    #[test]
    fn a_version_is_read_from_what_it_says() {
        assert_eq!(version_in("2.1.330 (Claude Code)").as_deref(), Some("2.1.330"));
        assert_eq!(version_in("codex-cli 0.167.2\n").as_deref(), Some("0.167.2"));
        assert_eq!(version_in("agy v1.4.0").as_deref(), Some("1.4.0"));
        assert_eq!(version_in("nothing here"), None);
    }

    #[test]
    fn a_version_is_asked_once_per_binary() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("claude");
        let count = tmp.path().join("count");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho x >> '{}'\necho '2.1.330 (Claude Code)'\n",
                count.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(version(&bin).as_deref(), Some("2.1.330"));
        assert_eq!(version(&bin).as_deref(), Some("2.1.330"));
        assert_eq!(
            std::fs::read_to_string(&count).unwrap().lines().count(),
            1,
            "asked once"
        );
        // Changed: asked again.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&bin, "#!/bin/sh\necho '2.1.331 (Claude Code)'\n").unwrap();
        assert_eq!(version(&bin).as_deref(), Some("2.1.331"));
    }
}
