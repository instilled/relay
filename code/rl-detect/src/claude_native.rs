//! Claude's own background sessions, 1:1 as `claude agents` shows them:
//! `claude agents --json --all` (the done ones too) says which there are,
//! and each one's `jobs/<id>/state.json` under Claude's config folder
//! (`$CLAUDE_CONFIG_DIR`, else `~/.claude`) adds what the listing leaves
//! out — its one-line summary (`detail`), the pull request it opened, the
//! colour you picked for it (`color`, one of `COLORS`), when
//! it was made and when it finished. A field neither says stays empty.
//!
//! What the profile's `sessions:` block says is how it is run: `list`,
//! `attach` (`claude attach <id>`), `start` (`claude --bg`), `name` (`-n
//! <name>`), and the profile's `resume` for carrying one on (`claude
//! --resume <session> --bg`).

use crate::harness::{self, CarryOn, Harness, NativeRow, NativeState, Pr, Started};
use crate::mcp_entry::{How, Place};
use crate::registry::{PROMPT, Profile, SESSION};
use crate::sessions::{ID, NAME};
use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rl_core::state::RunState;
use serde::Deserialize;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The reader a profile's `sessions.list` names for this listing.
pub const READER: &str = "claude-agents";

/// The `kind` the listing gives a session in Claude's background; the
/// others are open in a terminal, and their hooks tell of them.
const BACKGROUND: &str = "background";

/// Claude Code as a harness: its binary and the arguments its profile gives.
#[derive(Debug, Clone)]
pub struct Claude {
    profile: Profile,
    found: bool,
    harness: String,
    bin: String,
    list: Vec<String>,
    attach: Vec<String>,
    start: Vec<String>,
    name: Vec<String>,
    resume: Vec<String>,
    stop: Vec<String>,
    /// Where a first prompt goes, after `start` (the profile's `prompt`).
    prompt: Vec<String>,
    /// Claude's config folder, where it is not the environment's.
    config: Option<PathBuf>,
}

impl Claude {
    /// The harness of `profile`, run as `bin` (taken to be on this machine:
    /// `found` says otherwise). None where its profile says nothing of how
    /// it lists its sessions.
    pub fn of(profile: &Profile, bin: PathBuf) -> Option<Claude> {
        let s = profile.sessions.as_ref()?;
        Some(Claude {
            profile: profile.clone(),
            found: true,
            harness: profile.name.clone(),
            bin: bin.to_string_lossy().into_owned(),
            list: s.list.as_ref()?.args.clone(),
            attach: s.attach.clone(),
            start: s.start.clone(),
            name: s.name.clone(),
            resume: profile.resume.clone(),
            stop: s.stop.clone(),
            prompt: if profile.prompt.is_empty() {
                vec![PROMPT.to_string()]
            } else {
                profile.prompt.clone()
            },
            config: None,
        })
    }

    /// The same, its binary on this machine or not.
    pub fn found(mut self, found: bool) -> Claude {
        self.found = found;
        self
    }

    /// The same, reading Claude's files under `dir` rather than where the
    /// environment says.
    pub fn with_config_dir(mut self, dir: PathBuf) -> Claude {
        self.config = Some(dir);
        self
    }

    /// `$CLAUDE_CONFIG_DIR/jobs`, else `~/.claude/jobs`.
    fn jobs_dir(&self) -> Option<PathBuf> {
        let config = self.config.clone().or_else(|| {
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|d| !d.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))
        })?;
        Some(config.join("jobs"))
    }

    /// What starts a session in its background, named and prompted if
    /// given (`claude --bg -n <name> <prompt>`). None: its profile says no
    /// `start`.
    pub fn start_argv(&self, name: Option<&str>, prompt: Option<&str>) -> Option<Vec<String>> {
        if self.start.is_empty() {
            return None;
        }
        let mut args = self.start.clone();
        if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
            args.extend(self.name.iter().map(|a| a.replace(NAME, n)));
        }
        if let Some(p) = prompt.filter(|p| !p.trim().is_empty()) {
            args.extend(self.prompt.iter().map(|a| a.replace(PROMPT, p)));
        }
        Some(self.argv(&args))
    }

    fn argv<'a>(&self, args: impl IntoIterator<Item = &'a String>) -> Vec<String> {
        std::iter::once(self.bin.clone())
            .chain(args.into_iter().cloned())
            .collect()
    }
}

impl Harness for Claude {
    fn profile(&self) -> &Profile {
        &self.profile
    }

    fn bin(&self) -> &str {
        &self.bin
    }

    fn found(&self) -> bool {
        self.found
    }

    fn lists(&self) -> bool {
        true
    }

    fn list_within(&self, wait: Duration) -> Result<Vec<NativeRow>> {
        let text = harness::run(&self.argv(&self.list), None, true, wait)?;
        let jobs = self.jobs_dir();
        Ok(rows(&text, jobs.as_deref())?
            .into_iter()
            .map(|mut r| {
                r.harness = self.harness.clone();
                r
            })
            .collect())
    }

    fn attach_argv(&self, id: &str) -> Option<Vec<String>> {
        let args: Vec<String> = self.attach.iter().map(|a| a.replace(ID, id)).collect();
        Some(self.argv(&args))
    }

    /// In its background (`claude --resume <id> --bg`), where its profile
    /// says how to start one there; else held, as its `resume` says.
    fn carry_on(&self, session_id: &str) -> Option<CarryOn> {
        match self.carry_on_argv(session_id) {
            Some(argv) => Some(CarryOn::Background(argv)),
            None => self
                .profile
                .resume_argv(self.bin.clone(), session_id)
                .map(CarryOn::Held),
        }
    }

    /// In its background (`claude --bg`), where its profile says how; else
    /// held.
    fn start(&self, cwd: &Path, name: Option<&str>, prompt: Option<&str>, before: &HashSet<String>) -> Result<Started> {
        match self.start_argv(name, prompt) {
            Some(argv) => self.carry_on_there(&argv, cwd, before).map(Started::Id),
            None => Ok(Started::Held(self.profile.prompt_args(prompt.unwrap_or("")))),
        }
    }

    fn carry_on_there(&self, argv: &[String], cwd: &Path, before: &HashSet<String>) -> Result<String> {
        begin(|wait| self.list_within(wait), argv, cwd, before, LISTED_WAIT)
    }

    fn stops(&self, row: &NativeRow) -> bool {
        !self.stop.is_empty() && !row.state.state.is_terminal()
    }

    fn stop(&self, id: &str, cwd: &Path) -> Result<()> {
        let argv = self.stop_argv(id).context("its profile says no `stop`")?;
        harness::run(&argv, Some(cwd), false, harness::LIST_WAIT).map(drop)
    }

    /// Through `claude mcp add|remove --scope user`, which owns its file.
    fn mcp(&self, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Option<Place>> {
        let how = How::ClaudeCli {
            cli: harness::cli_name(self),
            bin: self.found.then(|| PathBuf::from(&self.bin)),
        };
        harness::mcp_place(self, env, how)
    }

    /// Claude Code's conversation id, the same one its hooks report.
    fn caller_id_var(&self) -> Option<&'static str> {
        Some(SESSION_ID_VAR)
    }

    /// Claude Code has no way to tell one of its sessions anything from
    /// outside; its hooks carry the news instead (`rl hook`).
    fn mail_by_hooks(&self) -> bool {
        true
    }

    /// `claude agents` lists its background sessions by the name `name`,
    /// which Claude's `SendMessage` addresses.
    fn wake_advice(&self, name: &str) -> Option<String> {
        Some(format!(
            "{name} is a Claude session — to wake it now, also SendMessage it (\"relay mail waiting: call inbox\")."
        ))
    }
}

/// The variable Claude Code sets to a session's conversation id.
pub const SESSION_ID_VAR: &str = "CLAUDE_CODE_SESSION_ID";

impl Claude {
    /// What carries the conversation `session_id` on in its background
    /// (`claude --resume <id> --bg`). None: its profile says no `start` or
    /// no `resume`.
    pub fn carry_on_argv(&self, session_id: &str) -> Option<Vec<String>> {
        if self.start.is_empty() || self.resume.is_empty() {
            return None;
        }
        let resume: Vec<String> = self.resume.iter().map(|a| a.replace(SESSION, session_id)).collect();
        Some(self.argv(resume.iter().chain(&self.start)))
    }

    /// What stops its session `id` (`claude stop <id>`). None: its profile
    /// says no `stop`.
    pub fn stop_argv(&self, id: &str) -> Option<Vec<String>> {
        if self.stop.is_empty() {
            return None;
        }
        let args: Vec<String> = self.stop.iter().map(|a| a.replace(ID, id)).collect();
        Some(self.argv(&args))
    }
}

/// How long a session a start began has to come onto Claude's list.
pub const LISTED_WAIT: Duration = Duration::from_secs(10);
/// How often Claude is asked meanwhile.
const LISTED_EVERY: Duration = Duration::from_millis(250);

/// Runs `argv`, which starts or carries on a session in Claude's own
/// background, in `cwd`, and says the id of the session it began — once
/// `list` (Claude's listing, for no longer than it is given) has it, never
/// on what it printed alone: a background session not among `before` (the
/// ids its last listing had), in `cwd`, by an id it printed. The listing is
/// asked for up to `listed`. Nothing listed so is an error in the words it
/// printed last.
pub fn begin(
    list: impl Fn(Duration) -> Result<Vec<NativeRow>>,
    argv: &[String],
    cwd: &Path,
    before: &HashSet<String>,
    listed: Duration,
) -> Result<String> {
    let printed = harness::run(argv, Some(cwd), false, harness::START_WAIT)?;
    let ids = started_ids(&printed);
    let deadline = Instant::now() + listed;
    loop {
        // The whole loop, its listings too, is bounded by `listed`.
        let left = deadline.saturating_duration_since(Instant::now());
        if !ids.is_empty()
            && !left.is_zero()
            && let Ok(rows) = list(left.min(harness::LIST_WAIT))
            && let Some(id) = ids
                .iter()
                .find(|id| !before.contains(*id) && rows.iter().any(|r| r.id == **id && harness::same_dir(&r.cwd, cwd)))
        {
            return Ok(id.clone());
        }
        if ids.is_empty() || Instant::now() >= deadline {
            let last = printed.lines().rev().find(|l| !l.trim().is_empty());
            bail!("{}", last.map_or("it printed nothing", str::trim));
        }
        std::thread::sleep(LISTED_EVERY);
    }
}

/// The ids `claude --bg` may have printed for what it started: the word
/// after `attach ` first, where it says how to attach, then every word of
/// eight hex digits. Which it is, only the listing says.
pub fn started_ids(printed: &str) -> Vec<String> {
    let word = |s: &str| {
        s.split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .next()
            .filter(|w| !w.is_empty())
            .map(str::to_ascii_lowercase)
    };
    let mut ids: Vec<String> = printed
        .match_indices("attach ")
        .filter_map(|(at, a)| word(&printed[at + a.len()..]))
        .collect();
    ids.extend(
        printed
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| w.len() == 8 && w.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_ascii_lowercase),
    );
    let mut seen = std::collections::HashSet::new();
    ids.retain(|i| seen.insert(i.clone()));
    ids
}

/// An entry of `claude agents --json`, as Claude writes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listed {
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    /// Unix milliseconds: when its process started.
    #[serde(default)]
    pub started_at: Option<i64>,
}

/// `jobs/<id>/state.json`: what of it the list shows.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub children: Option<Vec<Child>>,
    /// The conversation it carries on now, which its hooks tell of.
    #[serde(default)]
    pub resume_session_id: Option<String>,
    #[serde(default)]
    pub created_at: Option<Timestamp>,
    /// When it last ended — finished, failed or stopped.
    #[serde(default)]
    pub last_terminal_at: Option<Timestamp>,
    /// The colour you picked for it, by name, as Claude writes it: taken
    /// as any value, so one Relay does not know never spoils the rest.
    #[serde(default)]
    pub color: Option<serde_json::Value>,
}

/// The colours a Claude session can pick, as its `state.json` names them.
pub const COLORS: [&str; 8] = ["red", "blue", "green", "yellow", "purple", "orange", "pink", "cyan"];

/// Something a session made: a pull request (`kind: pr`, its number as
/// `id`), a page, ….
#[derive(Debug, Clone, Deserialize)]
pub struct Child {
    pub id: String,
    pub href: String,
    pub kind: String,
}

/// Claude's background sessions in `claude agents --json`. An entry that
/// does not read — no id, no `sessionId`, no whole folder — is skipped, not
/// the whole list; one open in a terminal is not a background session.
pub fn parse(text: &str) -> Result<Vec<Listed>> {
    let all: Vec<serde_json::Value> = serde_json::from_str(text).context("reading `claude agents --json`")?;
    Ok(all
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Listed>(v).ok())
        .filter(|l| {
            l.kind.as_deref() == Some(BACKGROUND) && !l.id.is_empty() && !l.session_id.is_empty() && l.cwd.is_absolute()
        })
        .collect())
}

/// How much of the end of a job's `timeline.jsonl` is read for its last
/// entry: the file only grows, to megabytes.
const TIMELINE_TAIL: u64 = 64 * 1024;

/// An entry of `jobs/<id>/timeline.jsonl`: when it came.
#[derive(Deserialize)]
struct Entry {
    at: Timestamp,
}

/// When the last entry of `jobs/<id>/timeline.jsonl` came — when the job
/// came to stand as it does — read from the file's end only.
pub fn last_entry_at(jobs: &Path, id: &str) -> Option<Timestamp> {
    use std::io::{Read, Seek, SeekFrom};
    if id.is_empty() || id.contains(['/', '\\', '.']) {
        return None;
    }
    let mut f = std::fs::File::open(jobs.join(id).join("timeline.jsonl")).ok()?;
    let from = f.metadata().ok()?.len().saturating_sub(TIMELINE_TAIL);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    let text = String::from_utf8_lossy(&tail);
    let mut lines: Vec<&str> = text.lines().collect();
    if from > 0 && !lines.is_empty() {
        // Cut into by where the read began.
        lines.remove(0);
    }
    lines
        .iter()
        .rev()
        .find_map(|l| serde_json::from_str::<Entry>(l).ok())
        .map(|e| e.at)
}

/// `jobs/<id>/state.json`, where it is there and reads.
pub fn job(jobs: &Path, id: &str) -> Option<Job> {
    if id.is_empty() || id.contains(['/', '\\', '.']) {
        return None;
    }
    let bytes = std::fs::read(jobs.join(id).join("state.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The listing as rows, each filled in from its job file under `jobs`.
pub fn rows(text: &str, jobs: Option<&Path>) -> Result<Vec<NativeRow>> {
    Ok(parse(text)?
        .into_iter()
        .map(|l| {
            let j = jobs.and_then(|d| job(d, &l.id));
            let last = jobs.and_then(|d| last_entry_at(d, &l.id));
            row(l, j, last)
        })
        .collect())
}

/// How Claude says one stands, in its words: a session waiting on you
/// `needs input`; one done whose process is still up, or at rest, `idle`;
/// one done and gone `completed`. A word it says that is not known here is
/// kept as said.
pub fn state(state: Option<&str>, status: Option<&str>, process: bool) -> NativeState {
    let (words, state) = match state.unwrap_or_default() {
        "blocked" => ("needs input", RunState::NeedsInput),
        "done" if process => ("idle", RunState::Idle),
        "done" => ("completed", RunState::Finished),
        "failed" => ("failed", RunState::Failed),
        "stopped" => ("stopped", RunState::Stopped),
        "working" if status == Some("idle") => ("idle", RunState::Idle),
        "working" => ("working", RunState::Working),
        other => (other, RunState::Idle),
    };
    NativeState {
        words: words.to_string(),
        state,
    }
}

/// A listed session, with what its job file says and when its timeline's
/// last entry came (`last`).
pub fn row(l: Listed, job: Option<Job>, last: Option<Timestamp>) -> NativeRow {
    let j = job.unwrap_or_default();
    let said = l.state.as_deref().or(j.state.as_deref());
    let st = state(said, l.status.as_deref(), l.pid.is_some());
    let named = |n: &Option<String>| n.clone().filter(|n| !n.trim().is_empty() && !n.starts_with("<<"));
    let name = named(&l.name).or_else(|| named(&j.name)).unwrap_or_else(|| {
        l.cwd
            .file_name()
            .map_or_else(|| l.id.clone(), |f| f.to_string_lossy().into_owned())
    });
    let pr = j.children.iter().flatten().find(|c| c.kind == "pr").and_then(|c| {
        Some(Pr {
            number: c.id.parse().ok()?,
            href: c.href.clone(),
        })
    });
    let started = j
        .created_at
        .or_else(|| l.started_at.and_then(|ms| Timestamp::from_millisecond(ms).ok()));
    // Its age stops when it did, however it ended.
    let ended = j.last_terminal_at.filter(|_| st.state.is_terminal());
    let color = j
        .color
        .as_ref()
        .and_then(|c| c.as_str())
        .filter(|c| COLORS.contains(c))
        .map(String::from);
    NativeRow {
        color,
        harness: crate::user_hooks::CLAUDE.into(),
        session_id: j.resume_session_id.filter(|s| !s.is_empty()).unwrap_or(l.session_id),
        id: l.id,
        name,
        cwd: l.cwd,
        state: st,
        summary: j.detail.filter(|d| !d.trim().is_empty()),
        started,
        ended,
        at: ended.or(last),
        pr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Registry;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/claude-native")
    }

    fn listed() -> String {
        std::fs::read_to_string(fixtures().join("agents.json")).unwrap()
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn claude() -> Claude {
        let r = Registry::builtin();
        Claude::of(r.profile("claude").unwrap(), "claude".into()).unwrap()
    }

    #[test]
    fn the_listing_reads_as_claudes_background_sessions_only() {
        let l = parse(&listed()).unwrap();
        let ids: Vec<&str> = l.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "8af077cd", "4a088e61", "51eef448", "5286f002", "f40c833b", "059a4700", "a8b1e728", "a7dafda6"
            ],
            "the one open in a terminal is not among them"
        );
        let skipped = r#"[
          {"id":"b1","cwd":"/w","kind":"background","sessionId":"s1","state":"working","new":{"x":1}},
          {"cwd":"/w","kind":"background","sessionId":"s2"},
          {"id":"b3","cwd":"w/rel","kind":"background","sessionId":"s3"},
          {"id":"b4","cwd":"/w","kind":"background","sessionId":""},
          {"id":"b5","cwd":"/w","sessionId":"s5"},
          "not even an object"
        ]"#;
        let ids: Vec<String> = parse(skipped).unwrap().into_iter().map(|l| l.id).collect();
        assert_eq!(ids, ["b1"], "no id, a relative folder, no session id, no kind: skipped");
        assert!(parse("{}").is_err(), "not a list at all");
    }

    #[test]
    fn each_row_is_filled_in_from_its_job_file() {
        let rows = rows(&listed(), Some(&fixtures().join("jobs"))).unwrap();
        let r = &rows[0];
        assert_eq!(
            (r.harness.as_str(), r.id.as_str(), r.name.as_str()),
            ("claude", "8af077cd", "pf/onb: asset list onboarding")
        );
        assert_eq!(r.cwd, PathBuf::from("/w/flint-platform"));
        assert_eq!(r.summary.as_deref(), Some("the summary of 8af077cd"));
        assert_eq!(r.started, Some(at("2026-08-19T15:38:47.165Z")), "from when it was made");
        assert_eq!(r.ended, None);
        assert_eq!(
            r.at,
            Some(at("2026-11-03T20:33:40.508Z")),
            "when its timeline last said something, not when its file was touched"
        );
        let untimed = rows.iter().find(|r| r.id == "5286f002").unwrap();
        assert_eq!(untimed.at, None, "no timeline, nothing to say since when");
        // Carried on in another conversation: the one its hooks tell of.
        let carried = &rows[1];
        assert_eq!(carried.session_id, "2132ce0a-647c-43a1-9c71-fa758bc20c12");
        assert_eq!(carried.pr, None, "a page it made is no pull request");
        // Done: its age stops when it did.
        let done = rows.iter().find(|r| r.id == "059a4700").unwrap();
        assert_eq!(done.ended, Some(at("2026-11-06T12:25:21.449Z")));
        assert_eq!(done.at, done.ended);
        // A placeholder name gives way to the folder's.
        let placeholder = rows.iter().find(|r| r.id == "a8b1e728").unwrap();
        assert_eq!(placeholder.name, "flint-agent-marketplace-internal");
    }

    #[test]
    fn a_session_with_no_job_file_has_an_empty_summary() {
        let rows = rows(&listed(), Some(&fixtures().join("jobs"))).unwrap();
        let r = rows.iter().find(|r| r.id == "a7dafda6").unwrap();
        assert_eq!((r.summary.as_deref(), r.pr.as_ref(), r.ended), (None, None, None));
        assert_eq!(r.session_id, "a7dafda6-d951-451e-ba3a-b20c53cc4e3d", "the listing's");
        assert_eq!(
            r.started,
            Timestamp::from_millisecond(1794094089598).ok(),
            "the listing's"
        );
        let none = super::rows(&listed(), None).unwrap();
        assert!(none.iter().all(|r| r.summary.is_none()), "no config folder at all");
    }

    #[test]
    fn the_state_is_in_claudes_own_words() {
        let rows = rows(&listed(), Some(&fixtures().join("jobs"))).unwrap();
        let words: Vec<(&str, &str, RunState)> = rows
            .iter()
            .map(|r| (r.id.as_str(), r.state.words.as_str(), r.state.state))
            .collect();
        assert_eq!(
            words,
            [
                ("8af077cd", "needs input", RunState::NeedsInput),
                ("4a088e61", "needs input", RunState::NeedsInput),
                ("51eef448", "idle", RunState::Idle),
                ("5286f002", "working", RunState::Working),
                ("f40c833b", "idle", RunState::Idle),
                ("059a4700", "completed", RunState::Finished),
                ("a8b1e728", "completed", RunState::Finished),
                ("a7dafda6", "needs input", RunState::NeedsInput),
            ]
        );
        assert_eq!(state(Some("failed"), None, false).state, RunState::Failed);
        assert_eq!(state(Some("stopped"), None, false).words, "stopped");
        let new = state(Some("pondering"), None, true);
        assert_eq!(
            (new.words.as_str(), new.state),
            ("pondering", RunState::Idle),
            "kept as said"
        );
    }

    #[test]
    fn its_age_stops_however_it_ended() {
        let listed = |state: &str, pid: Option<u32>| Listed {
            id: "b1".into(),
            session_id: "s1".into(),
            name: None,
            cwd: "/w".into(),
            kind: Some(BACKGROUND.into()),
            state: Some(state.into()),
            status: None,
            pid,
            started_at: None,
        };
        let ended_at = at("2026-11-06T12:25:21.449Z");
        let job = Job {
            last_terminal_at: Some(ended_at),
            ..Job::default()
        };
        for state in ["done", "failed", "stopped"] {
            let r = row(listed(state, None), Some(job.clone()), None);
            assert_eq!((r.ended, r.at), (Some(ended_at), Some(ended_at)), "{state}");
        }
        // Done, its process still up: at rest, not ended.
        let r = row(
            listed("done", Some(7)),
            Some(job.clone()),
            Some(at("2026-11-07T00:00:00Z")),
        );
        assert_eq!((r.ended, r.at), (None, Some(at("2026-11-07T00:00:00Z"))));
        let r = row(listed("working", Some(7)), Some(job), None);
        assert_eq!(r.ended, None, "an earlier end is not this run's");
    }

    #[test]
    fn a_session_keeps_the_colour_it_picked_and_an_unknown_one_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        for (id, state) in [
            ("c1", r#"{"color":"pink","detail":"x"}"#),
            ("c2", r#"{"detail":"x"}"#),
            ("c3", r#"{"color":"chartreuse"}"#),
            ("c4", r#"{"color":7,"detail":"kept"}"#),
        ] {
            std::fs::create_dir_all(tmp.path().join(id)).unwrap();
            std::fs::write(tmp.path().join(id).join("state.json"), state).unwrap();
        }
        let color = |id: &str| {
            let j = job(tmp.path(), id).expect("it reads");
            row(listed_one(id), Some(j), None).color
        };
        assert_eq!(color("c1").as_deref(), Some("pink"));
        assert_eq!(color("c2"), None, "most have none");
        assert_eq!(color("c3"), None, "a name not in the palette is no colour");
        assert_eq!(color("c4"), None, "nor is one that is not a name");
        assert_eq!(job(tmp.path(), "c4").unwrap().detail.as_deref(), Some("kept"));
        for c in ["red", "blue", "green", "yellow", "purple", "orange", "pink", "cyan"] {
            assert!(COLORS.contains(&c), "{c}");
        }
    }

    fn listed_one(id: &str) -> Listed {
        let text =
            format!(r#"[{{"id":"{id}","sessionId":"s-{id}","kind":"background","cwd":"/w","state":"working"}}]"#);
        parse(&text).unwrap().remove(0)
    }

    #[test]
    fn the_timeline_is_read_from_its_end() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("b1");
        std::fs::create_dir_all(&dir).unwrap();
        // Longer than what is read of it, its last line not an entry.
        let filler = format!(
            "{{\"at\":\"2026-01-01T00:00:00Z\",\"detail\":\"{}\"}}\n",
            "x".repeat(1000)
        );
        let mut text = filler.repeat(100);
        text.push_str("{\"at\":\"2026-11-07T10:00:00Z\",\"state\":\"working\"}\n");
        text.push_str("not json\n");
        std::fs::write(dir.join("timeline.jsonl"), text).unwrap();
        assert_eq!(last_entry_at(tmp.path(), "b1"), Some(at("2026-11-07T10:00:00Z")));
        assert_eq!(last_entry_at(tmp.path(), "none"), None);
        assert_eq!(last_entry_at(tmp.path(), "../b1"), None);
    }

    #[test]
    fn the_pr_is_the_first_one_it_opened() {
        let rows = rows(&listed(), Some(&fixtures().join("jobs"))).unwrap();
        let pr = |id: &str| rows.iter().find(|r| r.id == id).unwrap().pr.clone();
        assert_eq!(
            pr("059a4700"),
            Some(Pr {
                number: 6,
                href: "https://github.com/acme/flint-os/pull/6".into()
            })
        );
        assert_eq!(pr("51eef448").map(|p| p.number), Some(1358));
        assert_eq!(pr("a8b1e728"), None);
    }

    fn listed_at(id: &str, cwd: &Path) -> NativeRow {
        NativeRow {
            harness: "claude".into(),
            id: id.into(),
            session_id: format!("s-{id}"),
            name: id.into(),
            cwd: cwd.into(),
            state: NativeState {
                words: "working".into(),
                state: RunState::Working,
            },
            summary: None,
            started: None,
            ended: None,
            at: None,
            pr: None,
            color: None,
        }
    }

    /// A listing that says what it is told to, one answer per ask, the last
    /// kept.
    fn scripted(said: Vec<Result<Vec<NativeRow>, String>>) -> impl Fn(Duration) -> Result<Vec<NativeRow>> {
        let said = std::sync::Mutex::new(said);
        move |_| {
            let mut said = said.lock().unwrap();
            let next = if said.len() > 1 {
                said.remove(0)
            } else {
                said[0].clone()
            };
            next.map_err(|e| anyhow::anyhow!(e))
        }
    }

    #[test]
    fn a_started_id_is_only_one_claude_then_lists_new_in_its_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let here = tmp.path();
        let elsewhere = Path::new("/elsewhere");
        let echo = |s: &str| vec!["echo".to_string(), s.into()];
        let before: HashSet<String> = ["0ld00001".to_string()].into();
        // What it printed names one already there, one in another folder,
        // and the new one, which shows on the third ask.
        let list = scripted(vec![
            Ok(vec![listed_at("0ld00001", here)]),
            Err("no answer".into()),
            Ok(vec![
                listed_at("0ld00001", here),
                listed_at("aaaa0001", elsewhere),
                listed_at("bbbb0001", here),
            ]),
        ]);
        let id = begin(list, &echo("0ld00001 aaaa0001 bbbb0001"), here, &before, LISTED_WAIT).unwrap();
        assert_eq!(id, "bbbb0001");
        // Nothing listed it: the error is what it printed last.
        let list = scripted(vec![Ok(vec![listed_at("0ld00001", here)])]);
        let at = Instant::now();
        let printed = "echo started 0ld00001; echo 'not logged in'";
        let e = begin(
            &list,
            &["sh".into(), "-c".into(), printed.into()],
            here,
            &before,
            Duration::from_millis(600),
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "not logged in");
        assert!(at.elapsed() < Duration::from_secs(3));
        // No id in what it printed at all: said at once.
        let e = begin(&list, &echo("no such session"), here, &before, LISTED_WAIT).unwrap_err();
        assert_eq!(e.to_string(), "no such session");
    }

    #[test]
    fn a_start_waits_no_longer_than_its_deadline_for_a_slow_listing() {
        // Takes whatever it is given, as a hung harness would.
        let slow = |wait: Duration| -> Result<Vec<NativeRow>> {
            std::thread::sleep(wait);
            Err(anyhow::anyhow!("no answer"))
        };
        let tmp = tempfile::tempdir().unwrap();
        let at = Instant::now();
        let e = begin(
            slow,
            &["echo".into(), "abcd1234".into()],
            tmp.path(),
            &HashSet::new(),
            Duration::from_millis(400),
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "abcd1234");
        assert!(at.elapsed() < Duration::from_secs(2), "{:?}", at.elapsed());
    }

    #[test]
    fn it_runs_what_the_profile_says() {
        let c = claude();
        assert_eq!(c.attach_argv("ab12cd34").unwrap(), ["claude", "attach", "ab12cd34"]);
        assert_eq!(c.start_argv(None, None).unwrap(), ["claude", "--bg"]);
        assert_eq!(
            c.start_argv(Some("fix it"), Some("fix the login")).unwrap(),
            ["claude", "--bg", "-n", "fix it", "fix the login"]
        );
        assert_eq!(c.start_argv(Some(" "), Some("")).unwrap(), ["claude", "--bg"]);
        assert_eq!(
            c.start_argv(None, Some("fix the login\nthen test")).unwrap(),
            ["claude", "--bg", "fix the login\nthen test"],
            "a prompt of several lines is one argument"
        );
        assert_eq!(c.carry_on_argv("s-1").unwrap(), ["claude", "--resume", "s-1", "--bg"]);
        assert_eq!(c.stop_argv("ab12cd34").unwrap(), ["claude", "stop", "ab12cd34"]);
        let mut row = listed_at("ab12cd34", Path::new("/w"));
        assert!(c.stops(&row), "one still going");
        row.state.state = RunState::Finished;
        assert!(!c.stops(&row), "nothing left to stop");
        assert!(
            !c.never_held(),
            "a Claude Relay starts in a terminal of its own is held"
        );
        assert_eq!(c.ping_argv("ab12cd34", "x"), None, "nothing tells one from outside");
        // What it says to attach to goes first, then any other likely word.
        assert_eq!(
            started_ids("build deadbeef from cafe0001 · claude attach 9be483ad\n"),
            ["9be483ad", "deadbeef", "cafe0001"]
        );
        assert_eq!(started_ids("Session ABCDEF12 started"), ["abcdef12"]);
        assert!(started_ids("no id here, 1234567 or 123456789").is_empty());
    }

    #[test]
    fn it_lists_by_running_claude_and_reading_its_jobs() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("claude");
        // Prints the listing only when asked as Relay asks: `agents --json
        // --all`, its hooks told it is an ask.
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\n[ \"$*\" = 'agents --json --all' ] && [ -n \"$RELAY_ASKING\" ] && exec cat '{}'\nexit 9\n",
                fixtures().join("agents.json").display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let r = Registry::builtin();
        let c = Claude::of(r.profile("claude").unwrap(), bin)
            .unwrap()
            .with_config_dir(fixtures());
        let rows = c.list().unwrap();
        assert_eq!(rows.len(), 8);
        assert_eq!(rows[0].summary.as_deref(), Some("the summary of 8af077cd"));
    }

    /// Against the real `claude`: `cargo test -p rl-detect -- --ignored`.
    #[test]
    #[ignore = "runs the real claude CLI"]
    fn the_real_claude_lists_its_background_sessions() {
        let r = Registry::builtin();
        let p = r.profile("claude").unwrap();
        let bin = crate::registry::find_bin(&p.bins).expect("claude on PATH");
        let rows = Claude::of(p, bin).unwrap().list().unwrap();
        for row in &rows {
            assert_eq!(row.id.len(), 8, "{row:?}");
            assert!(row.cwd.is_absolute(), "{row:?}");
            assert!(!row.state.words.is_empty(), "{row:?}");
        }
    }
}
