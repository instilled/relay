//! Drift events: what Relay noticed of a harness that changed under it
//! (`rl_detect::drift`), kept under `$RELAY_HOME/drift/events.json` — one
//! event per check and harness version, counted — and reported as a GitHub
//! issue on Relay's repository by `rl report` (`/report` in the list), only
//! on your yes.
//!
//! Nothing of a conversation is kept: an event says what was expected,
//! what happened, the versions, and at most a redacted sketch of what was
//! seen. Nothing is sent anywhere but by `gh`, run on your yes; without
//! `gh`, a link is printed for you to open.

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rl_core::Home;
use rl_detect::drift::Noticed;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

/// Relay's own version and the commit it was built from (`build.rs`).
pub const RELAY_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RELAY_COMMIT: &str = env!("RL_GIT_COMMIT");

/// Where issues go: `owner/name` of Relay's repository (Cargo.toml's
/// `repository`).
pub fn repo() -> &'static str {
    env!("CARGO_PKG_REPOSITORY")
        .trim_end_matches('/')
        .trim_start_matches("https://github.com/")
}

/// The label an issue is posted with, where the repository has it.
pub const LABEL: &str = "drift";

/// The longest prefilled new-issue link printed.
pub const MAX_URL: usize = 7000;

/// One drift: a check that failed against one version of a harness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub check: String,
    pub harness: String,
    /// The harness's version, `unknown` where it would not say.
    pub version: String,
    pub what: String,
    #[serde(default)]
    pub evidence: Option<String>,
    pub relay: String,
    pub commit: String,
    pub os: String,
    pub count: u64,
    pub first_seen: Timestamp,
    pub last_seen: Timestamp,
    /// Where it was reported: the issue's or comment's URL, or `link
    /// shown` where only a link was printed.
    #[serde(default)]
    pub reported: Option<String>,
    #[serde(default)]
    pub dismissed: bool,
}

impl Event {
    /// Not reported, not dismissed: what the footer and `rl report` offer.
    pub fn pending(&self) -> bool {
        self.reported.is_none() && !self.dismissed
    }

    /// `claude 2.1.330`.
    pub fn harness_version(&self) -> String {
        format!("{} {}", self.harness, self.version)
    }
}

/// `$RELAY_HOME/drift`.
pub fn dir(home: &Home) -> PathBuf {
    home.path().join("drift")
}

fn events_file(dir: &Path) -> PathBuf {
    dir.join("events.json")
}

/// Every event kept, the oldest first.
pub fn load(dir: &Path) -> Vec<Event> {
    std::fs::read(events_file(dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Changes the events under a lock (several `rl` may notice at once).
fn change<T>(dir: &Path, f: impl FnOnce(&mut Vec<Event>) -> T) -> Result<T> {
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("events.lock"))?;
    lock.lock()?;
    let mut all = load(dir);
    let out = f(&mut all);
    let text = serde_json::to_vec_pretty(&all)?;
    rl_detect::drift::write_atomic(&events_file(dir), &text)?;
    Ok(out)
}

/// Keeps `n`, seen `at`, with `version` its harness's: a new event, or
/// one more of the same check and version.
pub fn record_as(dir: &Path, n: &Noticed, version: &str, at: Timestamp) -> Result<()> {
    change(dir, |all| {
        if let Some(e) = all.iter_mut().find(|e| e.check == n.check && e.version == version) {
            e.count += 1;
            e.last_seen = at;
            e.what = n.what.clone();
            if n.evidence.is_some() {
                e.evidence = n.evidence.clone();
            }
            return;
        }
        all.push(Event {
            check: n.check.clone(),
            harness: n.harness.clone(),
            version: version.to_string(),
            what: n.what.clone(),
            evidence: n.evidence.clone(),
            relay: RELAY_VERSION.into(),
            commit: RELAY_COMMIT.into(),
            os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            count: 1,
            first_seen: at,
            last_seen: at,
            reported: None,
            dismissed: false,
        });
    })
}

/// The version of `n`'s harness: its binary's, else the one on `PATH` by
/// its name's.
fn version_of(n: &Noticed) -> String {
    let bin = match &n.bin {
        Some(b) if b.is_absolute() => Some(b.clone()),
        Some(b) => rl_detect::registry::find_bin(&[b.display().to_string()]),
        None => rl_detect::registry::find_bin(std::slice::from_ref(&n.harness)),
    };
    bin.and_then(|b| rl_detect::drift::version(&b))
        .unwrap_or_else(|| "unknown".into())
}

/// Keeps `n` now, its version asked (once per binary).
pub fn record(dir: &Path, n: &Noticed) {
    let _ = record_as(dir, n, &version_of(n), Timestamp::now());
}

/// From now on, what this process notices is kept under `home`.
pub fn install(home: &Home) {
    let d = dir(home);
    let to = d.clone();
    rl_detect::drift::set_sink(d, Arc::new(move |n: &Noticed| record(&to, n)));
}

/// The footer's notice: the latest pending event, and how many more —
/// `codex 0.170.0: thread/name/set gone, rename stays in Relay · /report`.
pub fn notice(events: &[Event]) -> Option<String> {
    let pending: Vec<&Event> = events.iter().filter(|e| e.pending()).collect();
    let e = pending.iter().max_by_key(|e| e.last_seen)?;
    let more = match pending.len() - 1 {
        0 => String::new(),
        n => format!(" (+{n} more)"),
    };
    Some(format!(
        "{}: {}{more} · /report",
        e.harness_version(),
        e.what.replace('`', "")
    ))
}

/// The lines `rl setup --check` adds under `drift:`, one per pending event.
pub fn lines(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.pending())
        .map(|e| format!("  {}: {} [{}] · rl report", e.harness_version(), e.what, e.check))
        .collect()
}

/// What Relay expected, for an issue, by the check's family.
pub fn expected(check: &str, harness: &str) -> String {
    if check == rl_detect::claude_native::AGENTS_JSON {
        let bg = rl_detect::claude_native::REQUIRED_BACKGROUND
            .iter()
            .map(|f| format!("`{f}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return format!(
            "`claude agents --json --all` prints a JSON list whose entries each have `kind`, and those in the background {bg}."
        );
    }
    if check == rl_detect::claude_native::BG_ID {
        return "`claude --bg` prints the id of the session it started (`claude attach <id>`).".into();
    }
    if let Some(m) = check.strip_prefix("codex.schema.") {
        return format!(
            "`codex app-server generate-json-schema` describes the method `{m}` with the params Relay sends and the fields it reads."
        );
    }
    if let Some(m) = check.strip_prefix("codex.rpc.") {
        return format!("Codex's app-server daemon answers `{m}` with the params Relay sends.");
    }
    if let Some((h, w)) = check.split_once(".cli.") {
        return format!("`{h} {w}` takes the arguments Relay runs it with.");
    }
    format!("{harness} behaves as Relay expects ({check}).")
}

/// The issue's title: `drift: claude 2.1.330 — claude --bg printed no
/// session id [claude.bg_id]`. The check id and version in it are how an
/// open issue for the same is found again.
pub fn title(e: &Event) -> String {
    format!(
        "drift: {} — {} [{}]",
        e.harness_version(),
        e.what.replace('`', ""),
        e.check
    )
}

/// The issue's body, in Markdown.
pub fn body(e: &Event) -> String {
    let at = |t: Timestamp| t.strftime("%Y-%m-%d %H:%M UTC").to_string();
    let mut b = String::new();
    b.push_str(&format!(
        "Relay noticed that {} no longer behaves as it expects.\n\n",
        e.harness
    ));
    b.push_str(&format!(
        "### What Relay expected\n\n{}\n\n",
        expected(&e.check, &e.harness)
    ));
    b.push_str(&format!("### What happened\n\n{}\n\n", e.what));
    b.push_str("### Versions\n\n| | |\n|---|---|\n");
    b.push_str(&format!("| relay | {} ({}) |\n", e.relay, e.commit));
    b.push_str(&format!("| {} | {} |\n", e.harness, e.version));
    b.push_str(&format!("| OS | {} |\n\n", e.os));
    b.push_str(&format!(
        "### Check\n\n- id: `{}`\n- seen {} time{}, first {}, last {}\n\n",
        e.check,
        e.count,
        if e.count == 1 { "" } else { "s" },
        at(e.first_seen),
        at(e.last_seen)
    ));
    if let Some(ev) = &e.evidence {
        b.push_str(&format!("### Evidence (redacted)\n\n```\n{ev}\n```\n\n"));
    }
    b.push_str("Reported from `rl report`.\n");
    b
}

/// The comment added to an open issue that reports the same already.
pub fn comment(e: &Event) -> String {
    format!(
        "Seen again with relay {} ({}) on {}: {} time{}, last {}.\n\nReported from `rl report`.\n",
        e.relay,
        e.commit,
        e.os,
        e.count,
        if e.count == 1 { "" } else { "s" },
        e.last_seen.strftime("%Y-%m-%d %H:%M UTC")
    )
}

/// `s` for a URL's query: all but the unreserved characters escaped.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// What is said where a body was cut short to fit a link.
pub const CUT: &str = "\n\n_(cut short to fit a link; `rl report --dry-run` prints it whole)_";

/// The prefilled new-issue link for `title` and `body`, no longer than
/// `MAX_URL`: the body cut short, and said so, where it must be.
pub fn new_issue_url(title: &str, body: &str) -> String {
    let url = |b: &str| {
        format!(
            "https://github.com/{}/issues/new?title={}&labels={LABEL}&body={}",
            repo(),
            encode(title),
            encode(b)
        )
    };
    let whole = url(body);
    if whole.len() <= MAX_URL {
        return whole;
    }
    let chars: Vec<char> = body.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let b: String = chars[..mid].iter().collect::<String>() + CUT;
        if url(&b).len() <= MAX_URL {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    url(&(chars[..lo].iter().collect::<String>() + CUT))
}

/// Marks the event of `check` and `version` reported at `to`.
pub fn mark_reported(dir: &Path, check: &str, version: &str, to: &str) -> Result<()> {
    change(dir, |all| {
        for e in all.iter_mut().filter(|e| e.check == check && e.version == version) {
            e.reported = Some(to.to_string());
        }
    })
}

/// Dismisses every pending event, without posting anything: how many.
pub fn dismiss(dir: &Path) -> Result<usize> {
    change(dir, |all| {
        let mut n = 0;
        for e in all.iter_mut().filter(|e| e.pending()) {
            e.dismissed = true;
            n += 1;
        }
        n
    })
}

/// `gh`, where it is on `PATH` and logged in.
struct Gh(PathBuf);

impl Gh {
    fn find() -> Option<Gh> {
        let bin = rl_detect::registry::find_bin(&["gh".to_string()])?;
        let ok = Command::new(&bin)
            .args(["auth", "status"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(Gh(bin))
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let out = Command::new(&self.0)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .context("running gh")?;
        if !out.status.success() {
            bail!(
                "gh {}: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// An open issue whose title has `check` and `version`: its number and
    /// URL.
    fn existing(&self, e: &Event) -> Option<(u64, String)> {
        #[derive(Deserialize)]
        struct Issue {
            number: u64,
            url: String,
        }
        let search = format!("{} {} in:title", e.check, e.version);
        let said = self
            .run(&[
                "issue",
                "list",
                "--repo",
                repo(),
                "--search",
                &search,
                "--state",
                "open",
                "--json",
                "number,url",
            ])
            .ok()?;
        let found: Vec<Issue> = serde_json::from_str(&said).ok()?;
        found.into_iter().next().map(|i| (i.number, i.url))
    }

    /// Posts the issue (`--label drift`, else without where the label is
    /// not there): its URL.
    fn create(&self, title: &str, body_file: &Path) -> Result<String> {
        let file = body_file.to_string_lossy();
        let base = [
            "issue",
            "create",
            "--repo",
            repo(),
            "--title",
            title,
            "--body-file",
            &file,
        ];
        let labelled: Vec<&str> = base.iter().copied().chain(["--label", LABEL]).collect();
        let said = match self.run(&labelled) {
            Ok(s) => s,
            Err(e) if e.to_string().contains("label") => self.run(&base)?,
            Err(e) => return Err(e),
        };
        Ok(said.lines().last().unwrap_or("").to_string())
    }

    fn comment(&self, number: u64, body_file: &Path) -> Result<String> {
        let n = number.to_string();
        let file = body_file.to_string_lossy();
        let said = self.run(&["issue", "comment", &n, "--repo", repo(), "--body-file", &file])?;
        Ok(said.lines().last().unwrap_or("").to_string())
    }
}

/// Asks `question` and reads a line: yes only on `y` or `yes`.
fn ask(input: &mut dyn BufRead, out: &mut dyn Write, question: &str) -> Result<bool> {
    write!(out, "{question} [y/N] ")?;
    out.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    let a = line.trim().to_ascii_lowercase();
    Ok(a == "y" || a == "yes")
}

/// `rl report`: each pending event shown as the issue it would be, and
/// posted only on a yes — through `gh` where it is set up (a comment on
/// an open issue for the same, where there is one), else as a link to
/// open. `dry_run`: shown, nothing posted or asked.
pub fn report(dir: &Path, dry_run: bool, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<()> {
    let pending: Vec<Event> = load(dir).into_iter().filter(Event::pending).collect();
    if pending.is_empty() {
        writeln!(out, "nothing to report: no harness has changed under Relay")?;
        return Ok(());
    }
    let gh = if dry_run { None } else { Gh::find() };
    let n = pending.len();
    for (i, e) in pending.iter().enumerate() {
        let (title, body) = (title(e), body(e));
        writeln!(out, "── issue {} of {n} ──\n\n{title}\n\n{body}", i + 1)?;
        if dry_run {
            writeln!(out, "(dry run: nothing posted)\n")?;
            continue;
        }
        let file = dir.join("issue-body.md");
        match &gh {
            Some(gh) => {
                if let Some((number, url)) = gh.existing(e) {
                    writeln!(out, "An open issue reports this already: {url}")?;
                    if ask(
                        input,
                        out,
                        &format!("Add a comment with this count to #{number} on {}?", repo()),
                    )? {
                        std::fs::write(&file, comment(e))?;
                        let said = gh.comment(number, &file)?;
                        let at = if said.is_empty() { url } else { said };
                        mark_reported(dir, &e.check, &e.version, &at)?;
                        writeln!(out, "commented: {at}\n")?;
                    } else {
                        writeln!(out, "nothing posted\n")?;
                    }
                    continue;
                }
                if ask(input, out, &format!("Post this as an issue on {}?", repo()))? {
                    std::fs::write(&file, &body)?;
                    let url = gh.create(&title, &file)?;
                    mark_reported(dir, &e.check, &e.version, &url)?;
                    writeln!(out, "posted: {url}\n")?;
                } else {
                    writeln!(out, "nothing posted\n")?;
                }
            }
            None => {
                if ask(input, out, &format!("Post this as an issue on {}?", repo()))? {
                    writeln!(
                        out,
                        "gh is not set up here, so open this link to post it yourself:\n\n{}\n",
                        new_issue_url(&title, &body)
                    )?;
                    mark_reported(dir, &e.check, &e.version, "link shown")?;
                } else {
                    writeln!(out, "nothing posted\n")?;
                }
            }
        }
    }
    let _ = std::fs::remove_file(dir.join("issue-body.md"));
    Ok(())
}

/// `rl doctor`: the agents found, their versions and how each stands
/// against what Relay uses of it (`Harness::check`: what is off is noticed
/// as drift too), then every drift event, reported or not, with its count.
pub fn doctor(home: &Home, out: &mut dyn Write) -> Result<()> {
    writeln!(out, "relay {RELAY_VERSION} ({RELAY_COMMIT})")?;
    let registry = rl_detect::Registry::builtin();
    for h in rl_detect::harness::on_path(&registry) {
        let i = h.info();
        if !i.found || i.kind != rl_core::state::RunKind::Agent {
            continue;
        }
        let c = h.check();
        let v = c.version.clone().unwrap_or_else(|| "version unknown".into());
        writeln!(out, "{} {v} · {}", i.name, i.bin)?;
        for (check, what) in &c.off {
            writeln!(out, "  off: {what} [{check}]")?;
        }
    }
    let events = load(&dir(home));
    if events.is_empty() {
        writeln!(out, "drift: none noticed")?;
        return Ok(());
    }
    writeln!(out, "drift:")?;
    for e in &events {
        let state = match (&e.reported, e.dismissed) {
            (Some(at), _) => format!("reported {at}"),
            (None, true) => "dismissed".into(),
            (None, false) => "not reported · rl report".into(),
        };
        writeln!(
            out,
            "  {}: {} [{}] ×{} · {state}",
            e.harness_version(),
            e.what,
            e.check,
            e.count
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn noticed(check: &str) -> Noticed {
        Noticed::new("claude", None, check, "`claude --bg` printed no session id")
    }

    #[test]
    fn the_same_check_and_version_is_one_event_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        record_as(d, &noticed("claude.bg_id"), "2.1.330", at("2026-11-01T10:00:00Z")).unwrap();
        record_as(d, &noticed("claude.bg_id"), "2.1.330", at("2026-11-02T10:00:00Z")).unwrap();
        record_as(d, &noticed("claude.bg_id"), "2.1.331", at("2026-11-03T10:00:00Z")).unwrap();
        let all = load(d);
        assert_eq!(all.len(), 2, "one per version");
        assert_eq!(all[0].count, 2);
        assert_eq!(all[0].first_seen, at("2026-11-01T10:00:00Z"));
        assert_eq!(all[0].last_seen, at("2026-11-02T10:00:00Z"));
        assert_eq!(all[1].count, 1);
        assert_eq!(all[0].relay, RELAY_VERSION);
    }

    #[test]
    fn the_footer_says_the_latest_pending_and_goes_once_reported_or_dismissed() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        assert_eq!(notice(&load(d)), None);
        let n = Noticed::new(
            "codex",
            None,
            "codex.schema.thread/name/set",
            "thread/name/set gone, rename stays in Relay",
        );
        record_as(d, &n, "0.170.0", at("2026-11-02T10:00:00Z")).unwrap();
        assert_eq!(
            notice(&load(d)).as_deref(),
            Some("codex 0.170.0: thread/name/set gone, rename stays in Relay · /report")
        );
        record_as(d, &noticed("claude.bg_id"), "2.1.330", at("2026-11-01T10:00:00Z")).unwrap();
        assert_eq!(
            notice(&load(d)).as_deref(),
            Some("codex 0.170.0: thread/name/set gone, rename stays in Relay (+1 more) · /report")
        );
        mark_reported(d, "codex.schema.thread/name/set", "0.170.0", "https://x/1").unwrap();
        assert_eq!(
            notice(&load(d)).as_deref(),
            Some("claude 2.1.330: claude --bg printed no session id · /report")
        );
        assert_eq!(dismiss(d).unwrap(), 1);
        assert_eq!(notice(&load(d)), None);
        assert_eq!(load(d).len(), 2, "kept, for rl doctor");
    }

    fn event() -> Event {
        Event {
            check: "claude.agents_json".into(),
            harness: "claude".into(),
            version: "2.1.330".into(),
            what: "`claude agents --json` changed shape; sessions it lists may be missing".into(),
            evidence: Some("entries without sessionId, state".into()),
            relay: "0.1.0".into(),
            commit: "c4be74e".into(),
            os: "macos aarch64".into(),
            count: 3,
            first_seen: at("2026-11-01T10:00:00Z"),
            last_seen: at("2026-11-02T11:30:00Z"),
            reported: None,
            dismissed: false,
        }
    }

    #[test]
    fn an_issue_says_what_was_expected_what_happened_and_the_versions() {
        let e = event();
        assert_eq!(
            title(&e),
            "drift: claude 2.1.330 — claude agents --json changed shape; sessions it lists may be missing [claude.agents_json]"
        );
        assert_eq!(
            body(&e),
            "Relay noticed that claude no longer behaves as it expects.

### What Relay expected

`claude agents --json --all` prints a JSON list whose entries each have `kind`, and those in the background `id`, `sessionId`, `cwd`, `state`.

### What happened

`claude agents --json` changed shape; sessions it lists may be missing

### Versions

| | |
|---|---|
| relay | 0.1.0 (c4be74e) |
| claude | 2.1.330 |
| OS | macos aarch64 |

### Check

- id: `claude.agents_json`
- seen 3 times, first 2026-11-01 10:00 UTC, last 2026-11-02 11:30 UTC

### Evidence (redacted)

```
entries without sessionId, state
```

Reported from `rl report`.
"
        );
        assert!(expected("claude.cli.rm", "claude").contains("`claude rm`"));
        assert!(expected("codex.rpc.turn/start", "codex").contains("`turn/start`"));
        assert!(expected("codex.rpc.thread/delete", "codex").contains("`thread/delete`"));
    }

    #[test]
    fn a_link_is_encoded_and_cut_short_to_fit() {
        assert_eq!(encode("a b/c&d"), "a%20b%2Fc%26d");
        let short = new_issue_url("drift: x", "body");
        assert_eq!(
            short,
            format!(
                "https://github.com/{}/issues/new?title=drift%3A%20x&labels=drift&body=body",
                repo()
            )
        );
        let long = "é".repeat(5000);
        let url = new_issue_url("t", &long);
        assert!(url.len() <= MAX_URL, "{}", url.len());
        assert!(url.ends_with(&encode(CUT)), "says it was cut");
        assert!(url.len() > MAX_URL - 20, "as much as fits");
    }

    #[test]
    fn the_repo_is_cargos() {
        assert_eq!(repo(), "instilled/relay");
    }
}
