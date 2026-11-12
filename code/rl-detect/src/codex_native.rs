//! Codex's own sessions, 1:1 as its shared app-server daemon has them: the
//! threads it has loaded (`thread/loaded/list`), each read as it stands
//! (`thread/read`) — its name, its folder, whether it waits on you, works
//! or is ready, and when it last moved — and the threads Relay started that
//! the daemon has since let go of (`thread/list`, by the originator Relay
//! gives itself, `relay`), at rest on disk. Another thread only on disk is
//! not a session; a sub-agent's thread is its parent's, not one of its own.
//!
//! The daemon speaks JSON-RPC over a WebSocket on its control socket;
//! `codex app-server proxy` (the profile's `list` args) bridges that
//! socket's bytes to stdio, so Relay speaks the WebSocket itself — the
//! handshake, and one text frame per message. With no daemon running the
//! proxy cannot connect, and Codex has no sessions of its own to list.
//!
//! Relay starts a Codex session on the daemon, never in a holder: a thread
//! (`thread/start` with the folder only, so your own approval and sandbox
//! settings hold) and its first turn (`turn/start` with the prompt); then it
//! lets go, and the daemon runs the turn with no client. No prompt, no turn
//! to start: `codex` itself runs in this terminal, and its TUI makes the
//! thread. With no daemon running, a start runs `codex app-server daemon
//! start` first.
//!
//! Enter on one runs the profile's `sessions.attach` with the thread's id
//! (`codex resume <id>`), which opens it through the same daemon: the
//! app-server rejoins a thread that is running (and shows an approval it
//! waits on), or loads one at rest. Stopping one interrupts its running
//! turn (`turn/interrupt`); the thread stays. Removing one deletes it for
//! good (`thread/delete`), its running turn interrupted first.

use crate::harness::{
    self, Actions, Attach, CarryOn, Harness, Info, Mail, NativeRow, NativeState, Notified, Owner, RenameHow, Renamed,
    Session, Start, Started, Steps, held,
};
use crate::mcp_entry::{How, Place};
use crate::registry::Profile;
use crate::sessions::{ID, Rename};
use anyhow::{Context, Result, anyhow, bail};
use jiff::Timestamp;
use rl_core::state::RunState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

/// The reader a profile's `sessions.list` names for this listing.
pub const READER: &str = "codex-threads";

/// How long a list may take, all of it: the proxy, the handshake and every
/// answer.
pub const LIST_WAIT: Duration = Duration::from_secs(5);

/// How long a start, or a stop, may take on the daemon.
pub const START_WAIT: Duration = Duration::from_secs(30);

/// What the proxy says when there is no daemon to connect to.
const NO_DAEMON: &str = "failed to connect to socket";

/// What starts the daemon where none runs (`codex app-server daemon --help`,
/// codex 0.167).
const DAEMON_START: [&str; 3] = ["app-server", "daemon", "start"];

/// The client name Relay gives the daemon; a thread it starts keeps it as
/// its `originator`, which is how Relay knows its own threads again.
pub const CLIENT: &str = "relay";

/// How many of Relay's threads at rest are listed, the latest first.
const RESTING: u32 = 50;

/// Codex as a harness: its binary and the arguments its profile gives.
#[derive(Debug, Clone)]
pub struct Codex {
    profile: Profile,
    found: bool,
    harness: String,
    bin: String,
    list: Vec<String>,
    /// The profile's `sessions.attach`: `{id}` is the thread's id.
    attach: Vec<String>,
    info: Info,
}

impl Codex {
    /// The harness of `profile`, run as `bin` (taken to be on this machine:
    /// `found` says otherwise). None where its profile says nothing of how
    /// it lists its sessions.
    pub fn of(profile: &Profile, bin: PathBuf) -> Option<Codex> {
        let s = profile.sessions.as_ref()?;
        Some(Codex {
            profile: profile.clone(),
            found: true,
            harness: profile.name.clone(),
            bin: bin.to_string_lossy().into_owned(),
            list: s.list.as_ref()?.args.clone(),
            attach: s.attach.clone(),
            info: held::info(profile, &bin.to_string_lossy(), true),
        })
    }

    /// The same, its binary on this machine or not.
    pub fn found(mut self, found: bool) -> Codex {
        self.found = found;
        self.info.found = found;
        self
    }

    fn argv<'a>(&self, args: impl IntoIterator<Item = &'a String>) -> Vec<String> {
        std::iter::once(self.bin.clone())
            .chain(args.into_iter().cloned())
            .collect()
    }

    /// Does `work` on the daemon, started first where none runs.
    fn on_daemon<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Daemon) -> Result<T> + Send + Clone + 'static,
    ) -> Result<T> {
        let proxy = self.argv(&self.list);
        if let Some(done) = connect(&proxy, START_WAIT, work.clone())? {
            return Ok(done);
        }
        let start: Vec<String> = DAEMON_START.iter().map(|a| a.to_string()).collect();
        harness::run(&self.argv(&start), None, true, START_WAIT)?;
        connect(&proxy, START_WAIT, work)?.context("no codex daemon to connect to, even once started")
    }
}

impl Codex {
    /// How this Codex stands against what Relay uses (`compat`), asked
    /// once per binary; may run it. None where it is not on this machine.
    fn compat(&self) -> Option<Compat> {
        self.found.then(|| compat(Path::new(&self.bin))).flatten()
    }

    /// The same, only where it is known already: runs nothing.
    fn compat_known(&self) -> Option<Compat> {
        self.found.then(|| compat_known(Path::new(&self.bin))).flatten()
    }

    /// Err, saying why, where `f` is off for this Codex.
    fn needs(&self, f: Feature) -> Result<()> {
        match self.compat().and_then(|c| c.why(f)) {
            Some(why) => bail!("{why}"),
            None => Ok(()),
        }
    }

    /// The method its profile renames a thread by, where it is a call.
    fn rename_method(&self) -> Option<String> {
        match self.profile.sessions.as_ref()?.rename.as_ref()? {
            Rename::Rpc(m) => Some(m.clone()),
            Rename::Type(_) => None,
        }
    }
}

impl Steps for Codex {
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
        let compat = self.compat().unwrap_or_default();
        if compat.off(Feature::List).is_some() {
            return Ok(Vec::new());
        }
        let resting = compat.off(Feature::Resting).is_none();
        let threads = ask_with(&self.argv(&self.list), wait.min(LIST_WAIT), resting)?;
        Ok(rows(threads)
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

    /// A prompt: a thread on the daemon, its first turn started. None: the
    /// TUI, here.
    fn start_in(&self, cwd: &Path, name: Option<&str>, prompt: Option<&str>, _: &HashSet<String>) -> Result<Started> {
        let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) else {
            return Ok(Started::Here(vec![self.bin.clone()]));
        };
        self.needs(Feature::Start)?;
        let (cwd, name, prompt) = (
            cwd.to_path_buf(),
            name.filter(|n| !n.trim().is_empty()).map(str::to_string),
            prompt.to_string(),
        );
        self.on_daemon(move |d| begin(d, &cwd, name.as_deref(), &prompt))
            .map(Started::Background)
    }

    fn never_held(&self) -> bool {
        true
    }

    /// A conversation is a thread on the daemon already: enter resumes it
    /// there (`attach_argv`), never held.
    fn carry_on(&self, session_id: &str) -> Option<CarryOn> {
        self.attach_argv(session_id).map(CarryOn::Attach)
    }

    /// A thread with a turn running: stop interrupts it.
    fn stops(&self, row: &NativeRow) -> bool {
        matches!(row.state.state, RunState::Working | RunState::NeedsInput)
            && self.compat_known().is_none_or(|c| c.off(Feature::Stop).is_none())
    }

    fn stop_id(&self, id: &str, _: &Path) -> Result<()> {
        self.needs(Feature::Stop)?;
        let id = id.to_string();
        self.on_daemon(move |d| interrupt(d, &id))
    }

    /// Any thread, running or at rest: `thread/delete` on its daemon. Not
    /// where this Codex no longer has it.
    fn removes(&self, _: &NativeRow) -> bool {
        self.compat_known().is_none_or(|c| c.off(Feature::Remove).is_none())
    }

    fn remove_id(&self, id: &str, _: &Path) -> Result<()> {
        self.needs(Feature::Remove)?;
        let stops = self.compat_known().is_none_or(|c| c.off(Feature::Stop).is_none());
        let id = id.to_string();
        self.on_daemon(move |d| delete(d, &id, stops))
    }

    /// The call its profile's `sessions.rename` names, on its daemon
    /// (`thread/name/set`): any thread, any name, an empty one too.
    /// Not where this Codex no longer has it: the name stays in Relay.
    fn renames(&self, _: &NativeRow, _: &str) -> bool {
        self.rename_method().is_some() && self.compat_known().is_none_or(|c| c.off(Feature::Rename).is_none())
    }

    fn rename_row(&self, row: &NativeRow, name: &str, _: &Path) -> Result<()> {
        self.needs(Feature::Rename)?;
        let method = self.rename_method().context("its profile says no `sessions.rename`")?;
        let (id, name) = (row.id.clone(), name.trim().to_string());
        self.on_daemon(move |d| set_name(d, &method, &id, &name))
    }

    /// `[mcp_servers.relay]` in its `config.toml`, written by Relay.
    fn mcp(&self, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Option<Place>> {
        harness::mcp_place(self, env, How::Toml)
    }
}

impl Codex {
    /// Whether `f` is on, as far as is known without running anything.
    fn has(&self, f: Feature) -> bool {
        self.compat_known().is_none_or(|c| c.off(f).is_none())
    }
}

impl Harness for Codex {
    fn info(&self) -> &Info {
        &self.info
    }

    fn sessions(&self, wait: Duration) -> harness::Listed {
        self.list_within(wait).into()
    }

    /// A prompt: a thread on the daemon, named where given, its first turn
    /// started. None: its TUI, here. A conversation carried on: resumed
    /// here, on the daemon. Never held: `words` are its prompt, and a flag
    /// among them cannot go to the daemon.
    fn start(&self, s: &Start) -> Result<Started> {
        if let Some(id) = &s.resume {
            return Ok(Started::Here(
                self.attach_argv(id).context("its profile says no `sessions.attach`")?,
            ));
        }
        let prompt = match &s.words {
            Some(words) => {
                if let Some(flag) = words.iter().find(|w| w.starts_with('-')) {
                    bail!(
                        "{} starts in its own background, where `{flag}` can't go: give it a prompt only",
                        self.harness
                    );
                }
                (!words.is_empty()).then(|| words.join(" "))
            }
            None => s.prompt.clone(),
        };
        let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) else {
            return Ok(Started::Here(vec![self.bin.clone()]));
        };
        self.needs(Feature::Start)?;
        let (cwd, name) = (s.cwd.clone(), s.name.clone().filter(|n| !n.trim().is_empty()));
        self.on_daemon(move |d| begin(d, &cwd, name.as_deref(), &prompt))
            .map(Started::Background)
    }

    /// Its own thread: `codex resume <id>` opens it through the daemon. One
    /// open elsewhere, at rest, is a thread on the daemon already: resumed
    /// here the same way, never held.
    fn attach(&self, s: &Session) -> Attach {
        match s.owner {
            Owner::Relay { .. } => held::attach(self, s),
            Owner::Own => self.attach_argv(&s.id).map_or(Attach::No, Attach::Argv),
            Owner::Elsewhere { .. } if s.state == RunState::Idle => {
                self.attach_argv(&s.id).map_or(Attach::No, Attach::Argv)
            }
            Owner::Elsewhere { .. } => Attach::No,
        }
    }

    /// A thread with a turn running stops (its turn interrupted); any
    /// removes (`thread/delete`); a rename is a call on the daemon — each
    /// where this Codex still has it.
    fn actions(&self, s: &Session) -> Actions {
        match s.owner {
            Owner::Relay { .. } => held::actions(s),
            Owner::Own => Actions {
                stop: matches!(s.state, RunState::Working | RunState::NeedsInput) && self.has(Feature::Stop),
                remove: self.has(Feature::Remove),
                rename: if self.rename_method().is_some() && self.has(Feature::Rename) {
                    RenameHow::Now
                } else {
                    RenameHow::RelayOnly
                },
            },
            Owner::Elsewhere { .. } => Actions::NONE,
        }
    }

    fn stop(&self, s: &Session) -> Result<()> {
        match s.owner {
            Owner::Own => Steps::stop_id(self, &s.id, &s.cwd),
            _ => held::stop(s),
        }
    }

    fn remove(&self, s: &Session) -> Result<()> {
        match s.owner {
            Owner::Own => Steps::remove_id(self, &s.id, &s.cwd),
            _ => held::remove(s),
        }
    }

    /// `thread/name/set` on its daemon: any thread of its own, any name,
    /// an empty one too. Where this Codex no longer has it, or the call
    /// fails, the name stays Relay's, and why is said.
    fn rename(&self, s: &Session, name: &str) -> Renamed {
        if s.owner != Owner::Own {
            return Renamed::RelayOnly(None);
        }
        let set = || -> Result<()> {
            self.needs(Feature::Rename)?;
            let method = self.rename_method().context("its profile says no `sessions.rename`")?;
            let (id, name) = (s.id.clone(), name.trim().to_string());
            self.on_daemon(move |d| set_name(d, &method, &id, &name))
        };
        match set() {
            Ok(()) => Renamed::Now,
            Err(e) => Renamed::RelayOnly(Some(format!("{e:#}"))),
        }
    }

    /// Queued on its thread, the thread resumed on the daemon (`codex
    /// queue … --resume`).
    fn notify(&self, s: &Session, mail: &Mail) -> Notified {
        held::notify(self, s, mail)
    }

    /// `[mcp_servers.relay]` in its `config.toml`, written by Relay.
    fn setup(&self, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Vec<crate::setup::Part>> {
        held::setup(self, env, How::Toml)
    }

    fn caller(&self, _: &dyn Fn(&str) -> Option<String>) -> Option<String> {
        None
    }

    /// Its app-server's schema against what Relay uses (`compat`).
    fn check(&self) -> harness::Compat {
        let c = self.compat().unwrap_or_default();
        harness::Compat {
            version: c.version.clone().or_else(|| self.info.version()),
            off: c.gone.iter().map(|g| (g.check(), g.words())).collect(),
        }
    }
}

/// What Relay does with Codex's daemon, each turned off on its own where
/// the daemon no longer has what it needs (`USES`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Feature {
    /// Its threads on the list.
    List,
    /// Relay's threads at rest, on the list too.
    Resting,
    /// A session started with a prompt.
    Start,
    /// A rename passed on to Codex.
    Rename,
    /// `/stop` interrupting a running turn.
    Stop,
    /// A thread deleted for good (`^x` twice's remove, `/rm`).
    Remove,
}

impl Feature {
    /// What is so while it is off, in words.
    pub fn off(self) -> &'static str {
        match self {
            Feature::List => "its threads are not listed",
            Feature::Resting => "its threads at rest are not listed",
            Feature::Start => "Relay cannot start one with a prompt",
            Feature::Rename => "rename stays in Relay",
            Feature::Stop => "Relay cannot stop its turns",
            Feature::Remove => "Relay cannot remove its threads",
        }
    }
}

/// Fields Relay reads of an answer: in `file` of the generated schema, of
/// its `def`inition (none: the answer itself).
#[derive(Debug)]
pub struct Reads {
    pub file: &'static str,
    pub def: Option<&'static str>,
    pub fields: &'static [&'static str],
}

/// One method Relay calls on the daemon: what needs it, the params Relay
/// sends, the fields it reads of the answer.
#[derive(Debug)]
pub struct Use {
    pub method: &'static str,
    pub feature: Feature,
    pub sends: &'static [&'static str],
    pub reads: &'static [Reads],
}

const THREAD_READ: &str = "v2/ThreadReadResponse.json";

/// Everything Relay uses of the app-server's protocol, as the calls below
/// (`listed`, `begin`, `set_name`, `interrupt`, `delete`, `Rpc::open`) make them:
/// checked against `codex app-server generate-json-schema` once per Codex
/// (`compat`).
pub const USES: &[Use] = &[
    Use {
        method: "initialize",
        feature: Feature::List,
        sends: &["clientInfo"],
        reads: &[],
    },
    Use {
        method: "thread/loaded/list",
        feature: Feature::List,
        sends: &["cursor"],
        reads: &[Reads {
            file: "v2/ThreadLoadedListResponse.json",
            def: None,
            fields: &["data", "nextCursor"],
        }],
    },
    Use {
        method: "thread/read",
        feature: Feature::List,
        sends: &["threadId", "includeTurns"],
        reads: &[
            Reads {
                file: THREAD_READ,
                def: None,
                fields: &["thread"],
            },
            Reads {
                file: THREAD_READ,
                def: Some("Thread"),
                fields: &[
                    "id",
                    "sessionId",
                    "name",
                    "preview",
                    "cwd",
                    "status",
                    "parentThreadId",
                    "originator",
                    "createdAt",
                    "updatedAt",
                ],
            },
        ],
    },
    Use {
        method: "thread/list",
        feature: Feature::Resting,
        sends: &["limit", "sortKey", "sourceKinds"],
        reads: &[Reads {
            file: "v2/ThreadListResponse.json",
            def: None,
            fields: &["data"],
        }],
    },
    Use {
        method: "thread/start",
        feature: Feature::Start,
        sends: &["cwd"],
        reads: &[Reads {
            file: "v2/ThreadStartResponse.json",
            def: None,
            fields: &["thread"],
        }],
    },
    Use {
        method: "turn/start",
        feature: Feature::Start,
        sends: &["threadId", "input"],
        reads: &[],
    },
    Use {
        method: "thread/name/set",
        feature: Feature::Rename,
        sends: &["threadId", "name"],
        reads: &[],
    },
    Use {
        method: "turn/interrupt",
        feature: Feature::Stop,
        sends: &["threadId", "turnId"],
        reads: &[
            Reads {
                file: THREAD_READ,
                def: Some("Thread"),
                fields: &["turns"],
            },
            Reads {
                file: THREAD_READ,
                def: Some("Turn"),
                fields: &["id", "status"],
            },
        ],
    },
    Use {
        method: "thread/delete",
        feature: Feature::Remove,
        sends: &["threadId"],
        reads: &[],
    },
];

/// A method Relay uses that the schema no longer has as Relay uses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gone {
    pub method: String,
    pub feature: Feature,
    /// What of it: `method` for the whole of it, else `params.cwd`,
    /// `Thread.cwd`, ….
    pub missing: Vec<String>,
}

impl Gone {
    /// The drift check's id: `codex.schema.thread/name/set`.
    pub fn check(&self) -> String {
        format!("codex.schema.{}", self.method)
    }

    /// In a line: `thread/name/set gone, rename stays in Relay`.
    pub fn words(&self) -> String {
        let what = if self.missing.iter().any(|m| m == "method") {
            format!("{} gone", self.method)
        } else {
            format!("{} lacks {}", self.method, self.missing.join(", "))
        };
        format!("{what}, {}", self.feature.off())
    }
}

/// How a Codex stands against `USES`: its version, and what is gone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compat {
    pub version: Option<String>,
    pub gone: Vec<Gone>,
}

impl Compat {
    /// What turns `f` off, where something does.
    pub fn off(&self, f: Feature) -> Option<&Gone> {
        self.gone.iter().find(|g| g.feature == f)
    }

    /// Why `f` is off, in a line with the version: `codex 0.170.0:
    /// thread/name/set gone, rename stays in Relay`.
    pub fn why(&self, f: Feature) -> Option<String> {
        let g = self.off(f)?;
        Some(match &self.version {
            Some(v) => format!("codex {v}: {}", g.words()),
            None => format!("codex: {}", g.words()),
        })
    }
}

/// `USES` checked against the schema `codex app-server generate-json-schema
/// --out <dir>` wrote in `dir`. Err: there is no schema there to tell by.
pub fn check_schema(dir: &Path) -> Result<Vec<Gone>> {
    let read = |f: &str| -> Result<Value> {
        let bytes = std::fs::read(dir.join(f)).with_context(|| format!("reading {f}"))?;
        serde_json::from_slice(&bytes).with_context(|| format!("reading {f}"))
    };
    let requests = read("ClientRequest.json")?;
    let entries = requests["oneOf"]
        .as_array()
        .context("ClientRequest.json lists no requests")?;
    let has = |v: &Value, k: &str| v["properties"].as_object().is_some_and(|p| p.contains_key(k));
    let mut gone = Vec::new();
    for u in USES {
        let entry = entries.iter().find(|e| {
            e["properties"]["method"]["enum"]
                .as_array()
                .is_some_and(|m| m.iter().any(|m| m == u.method))
        });
        let Some(entry) = entry else {
            gone.push(Gone {
                method: u.method.into(),
                feature: u.feature,
                missing: vec!["method".into()],
            });
            continue;
        };
        let mut missing = Vec::new();
        let params = entry["properties"]["params"]["$ref"]
            .as_str()
            .and_then(|r| r.rsplit('/').next())
            .map(|n| &requests["definitions"][n]);
        for s in u.sends {
            if !params.is_some_and(|p| has(p, s)) {
                missing.push(format!("params.{s}"));
            }
        }
        for r in u.reads {
            let file = read(r.file).unwrap_or(Value::Null);
            let at = match r.def {
                Some(d) => &file["definitions"][d],
                None => &file,
            };
            for f in r.fields {
                if !has(at, f) {
                    missing.push(format!("{}.{f}", r.def.unwrap_or("answer")));
                }
            }
        }
        if !missing.is_empty() {
            gone.push(Gone {
                method: u.method.into(),
                feature: u.feature,
                missing,
            });
        }
    }
    Ok(gone)
}

/// What generates the schema (`codex app-server generate-json-schema
/// --help`, codex 0.167).
const SCHEMA: [&str; 2] = ["app-server", "generate-json-schema"];

type Memo = std::sync::Mutex<Option<HashMap<String, Option<Compat>>>>;
static COMPAT: Memo = std::sync::Mutex::new(None);

/// The file under `drift::dir()` that keeps what each Codex came to.
const COMPAT_FILE: &str = "codex-schema.json";

fn kept_compat() -> HashMap<String, Option<Compat>> {
    crate::drift::dir()
        .and_then(|d| std::fs::read(d.join(COMPAT_FILE)).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// How the Codex at `bin` stands, where it is known already — in this
/// process, or kept from an earlier run — without running anything. None:
/// not known yet, or it could not tell.
pub fn compat_known(bin: &Path) -> Option<Compat> {
    let key = crate::drift::stamp(bin)?;
    let mut memo = COMPAT.lock().ok()?;
    let memo = memo.get_or_insert_default();
    if let Some(c) = memo.get(&key) {
        return c.clone();
    }
    let kept = kept_compat().remove(&key)?;
    memo.insert(key, kept.clone());
    kept
}

/// How the Codex at `bin` stands: known (`compat_known`), else asked once
/// for this binary — its schema generated in a folder of its own and
/// checked (`check_schema`), what is gone noticed as drift. None: it could
/// not tell (no such command, no schema written), and nothing is turned
/// off.
pub fn compat(bin: &Path) -> Option<Compat> {
    let key = crate::drift::stamp(bin)?;
    if let Some(known) = COMPAT.lock().ok()?.get_or_insert_default().get(&key) {
        return known.clone();
    }
    if let Some(kept) = kept_compat().get(&key) {
        COMPAT.lock().ok()?.get_or_insert_default().insert(key, kept.clone());
        return kept.clone();
    }
    let out = std::env::temp_dir().join(format!(
        "rl-codex-schema-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let mut argv = vec![bin.to_string_lossy().into_owned()];
    argv.extend(SCHEMA.iter().map(|a| a.to_string()));
    argv.extend(["--out".to_string(), out.to_string_lossy().into_owned()]);
    let ran = harness::run_quiet(&argv, START_WAIT);
    let gone = ran.ok().and_then(|_| check_schema(&out).ok());
    let _ = std::fs::remove_dir_all(&out);
    let compat = gone.map(|gone| Compat {
        version: crate::drift::version(bin),
        gone,
    });
    if let Some(c) = &compat {
        for g in &c.gone {
            crate::drift::notice(crate::drift::Noticed::new(
                crate::user_hooks::CODEX,
                Some(bin),
                &g.check(),
                g.words(),
            ));
        }
    }
    COMPAT
        .lock()
        .ok()?
        .get_or_insert_default()
        .insert(key.clone(), compat.clone());
    if let Some(dir) = crate::drift::dir() {
        let mut kept = kept_compat();
        kept.insert(key, compat.clone());
        if let Ok(text) = serde_json::to_vec_pretty(&kept) {
            let _ = std::fs::create_dir_all(&dir);
            let _ = crate::drift::write_atomic(&dir.join(COMPAT_FILE), &text);
        }
    }
    compat
}

/// A thread as the app-server writes it: what of it the list shows.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// Usually its first prompt.
    #[serde(default)]
    pub preview: Option<String>,
    pub cwd: PathBuf,
    pub status: Status,
    /// Set on a sub-agent's thread: its parent's.
    #[serde(default)]
    pub parent_thread_id: Option<String>,
    /// The client that started it: `relay` for Relay's (`CLIENT`).
    #[serde(default)]
    pub originator: Option<String>,
    /// Unix seconds.
    #[serde(default)]
    pub created_at: Option<i64>,
    /// Unix seconds: when it last moved.
    #[serde(default)]
    pub updated_at: Option<i64>,
}

/// How a thread stands at runtime, as the app-server says.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub active_flags: Vec<String>,
}

/// How Codex says a thread stands: one active and waiting on an approval or
/// an answer `needs input`, one active `working`, one loaded at rest
/// `ready`, one the app-server lost to an error `error`, one only on disk
/// `resting`. A word it says that is not known here is kept as said.
pub fn state(status: &Status) -> NativeState {
    let waiting = status
        .active_flags
        .iter()
        .any(|f| f == "waitingOnApproval" || f == "waitingOnUserInput");
    let (words, state) = match status.kind.as_str() {
        "active" if waiting => ("needs input", RunState::NeedsInput),
        "active" => ("working", RunState::Working),
        "idle" => ("ready", RunState::Idle),
        "notLoaded" => ("resting", RunState::Idle),
        "systemError" => ("error", RunState::Failed),
        other => (other, RunState::Idle),
    };
    NativeState {
        words: words.to_string(),
        state,
    }
}

/// The threads as rows: Codex's sessions, not its sub-agents' threads, not
/// one only on disk that Relay did not start, not one without a whole
/// folder, and not an empty one — no name and no prompt yet. Codex's TUI
/// makes those as it opens and again after a turn, and the daemon lists
/// them until it unloads them; as rows they were a second session named
/// after the folder, with nothing in it. A thread listed twice stands
/// once, as first listed.
pub fn rows(threads: Vec<Thread>) -> Vec<NativeRow> {
    let blank = |s: &Option<String>| s.as_deref().is_none_or(|s| s.trim().is_empty());
    let mut seen = HashSet::new();
    threads
        .into_iter()
        .filter(|t| {
            !t.id.is_empty()
                && t.parent_thread_id.is_none()
                && (t.status.kind != "notLoaded" || t.originator.as_deref() == Some(CLIENT))
                && t.cwd.is_absolute()
                && !(blank(&t.name) && blank(&t.preview))
        })
        .filter(|t| seen.insert(t.id.clone()))
        .map(row)
        .collect()
}

/// A thread, as a row: its name, else its first prompt's first line, else
/// its folder's.
pub fn row(t: Thread) -> NativeRow {
    let named = |n: &Option<String>| {
        n.as_deref()
            .and_then(|n| n.lines().map(str::trim).find(|l| !l.is_empty()))
            .map(str::to_string)
    };
    let name = named(&t.name).or_else(|| named(&t.preview)).unwrap_or_else(|| {
        t.cwd
            .file_name()
            .map_or_else(|| t.id.clone(), |f| f.to_string_lossy().into_owned())
    });
    let second = |s: Option<i64>| s.and_then(|s| Timestamp::from_second(s).ok());
    let started = second(t.created_at);
    NativeRow {
        harness: crate::user_hooks::CODEX.into(),
        session_id: t.session_id.filter(|s| !s.is_empty()).unwrap_or_else(|| t.id.clone()),
        state: state(&t.status),
        id: t.id,
        name,
        cwd: t.cwd,
        summary: None,
        started,
        ended: None,
        at: second(t.updated_at).or(started),
        pr: None,
        color: None,
    }
}

/// The daemon, through the proxy's pipes.
pub type Daemon = Rpc<ChildStdout, ChildStdin>;

/// Runs `argv` (the proxy) and asks the daemon behind it for its threads,
/// for no longer than `wait`. No daemon: none.
pub fn ask(argv: &[String], wait: Duration) -> Result<Vec<Thread>> {
    ask_with(argv, wait, true)
}

/// `ask`, Relay's threads at rest asked for too only with `resting`.
pub fn ask_with(argv: &[String], wait: Duration, resting: bool) -> Result<Vec<Thread>> {
    let listed = move |d: &mut Daemon| listed_with(d, resting);
    Ok(connect(argv, wait, listed)?.unwrap_or_default())
}

/// Runs `argv` (the proxy), opens the daemon behind it (`Rpc::open`) and
/// does `work` there, for no longer than `wait`; then the proxy goes. None:
/// there is no daemon to connect to. Its hooks know the run for Relay's own
/// ask.
pub fn connect<T: Send + 'static>(
    argv: &[String],
    wait: Duration,
    work: impl FnOnce(&mut Daemon) -> Result<T> + Send + 'static,
) -> Result<Option<T>> {
    let (bin, args) = argv.split_first().ok_or_else(|| anyhow!("nothing to run"))?;
    let bin_path = bin.clone();
    let mut child = Command::new(bin);
    child
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("RL_SESSION")
        .env_remove("RL_SESSION_AGENT")
        .env(crate::user_hooks::ASKING, crate::user_hooks::asking_since())
        // A group of its own, so what it starts goes with it at the wait.
        .process_group(0);
    let mut child = child.spawn().context("running `codex app-server`")?;
    let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
    let mut stderr = child.stderr.take();
    let (err_tx, err_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        let _ = err_tx.send(s);
    });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let said = match (stdout, stdin) {
            (Some(r), Some(w)) => Rpc::open(r, w).and_then(|mut d| {
                d.bin = Some(PathBuf::from(&bin_path));
                work(&mut d)
            }),
            _ => Err(anyhow!("no pipe to `codex app-server proxy`")),
        };
        let _ = tx.send(said);
    });
    let said = rx.recv_timeout(wait);
    // Done or not, the proxy goes: its input closes, and it is killed.
    harness::kill_group(&mut child);
    // Its stderr has ended with it, or something it left holds the pipe: no
    // reason to wait long.
    let err = err_rx.recv_timeout(Duration::from_millis(200)).unwrap_or_default();
    match said {
        Ok(Ok(done)) => Ok(Some(done)),
        Ok(Err(_)) if err.contains(NO_DAEMON) => Ok(None),
        Ok(Err(e)) => match err.lines().find(|l| !l.trim().is_empty()) {
            Some(why) => bail!("`codex app-server proxy` failed: {why}"),
            None => Err(e.context("`codex app-server proxy`")),
        },
        Err(_) => bail!("`codex app-server proxy` did not answer in {}s", wait.as_secs()),
    }
}

/// The daemon's threads over the WebSocket that `r` and `w` carry: its
/// loaded ones, then Relay's at rest.
pub fn threads(r: impl Read, w: impl Write) -> Result<Vec<Thread>> {
    listed(&mut Rpc::open(r, w)?)
}

/// The daemon's loaded threads (`thread/loaded/list`, then `thread/read`
/// for each; one gone before it is read is left out), then the threads
/// Relay started that it has let go of (`thread/list`, the latest
/// `RESTING`; a daemon that cannot say adds none).
pub fn listed<R: Read, W: Write>(rpc: &mut Rpc<R, W>) -> Result<Vec<Thread>> {
    listed_with(rpc, true)
}

/// `listed`, Relay's threads at rest asked for only with `resting`.
pub fn listed_with<R: Read, W: Write>(rpc: &mut Rpc<R, W>, resting: bool) -> Result<Vec<Thread>> {
    let mut ids: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = rpc.call("thread/loaded/list", json!({"cursor": cursor}))?;
        let page: Loaded = serde_json::from_value(page).context("reading `thread/loaded/list`")?;
        ids.extend(page.data);
        match page.next_cursor {
            Some(c) if !c.is_empty() => cursor = Some(c),
            _ => break,
        }
    }
    let reads = ids
        .iter()
        .map(|id| ("thread/read", json!({"threadId": id, "includeTurns": false})))
        .collect();
    let mut threads: Vec<Thread> = rpc
        .call_all(reads)?
        .into_iter()
        .filter_map(|said| {
            let read: Read1 = serde_json::from_value(said.ok()?).ok()?;
            Some(read.thread)
        })
        .collect();
    if !resting {
        return Ok(threads);
    }
    let resting = rpc
        .call(
            "thread/list",
            json!({"limit": RESTING, "sortKey": "updated_at", "sourceKinds": ["vscode", "appServer"]}),
        )
        .ok()
        .and_then(|page| serde_json::from_value::<Listed>(page).ok())
        .map(|page| page.data)
        .unwrap_or_default();
    threads.extend(
        resting
            .into_iter()
            .filter_map(|t| serde_json::from_value::<Thread>(t).ok())
            .filter(|t| t.originator.as_deref() == Some(CLIENT) && t.status.kind == "notLoaded"),
    );
    Ok(threads)
}

/// Starts a thread in `cwd` on the daemon, named `name` if given, and its
/// first turn on `prompt`: the thread's id. Only the folder is given, so
/// the thread takes your own settings (model, approvals, sandbox).
pub fn begin<R: Read, W: Write>(rpc: &mut Rpc<R, W>, cwd: &Path, name: Option<&str>, prompt: &str) -> Result<String> {
    let started = rpc.call("thread/start", json!({"cwd": cwd}))?;
    let id = started["thread"]["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("`thread/start` said no thread id")?
        .to_string();
    if let Some(name) = name {
        rpc.call("thread/name/set", json!({"threadId": id, "name": name}))?;
    }
    rpc.call(
        "turn/start",
        json!({"threadId": id, "input": [{"type": "text", "text": prompt}]}),
    )?;
    Ok(id)
}

/// Names the thread `id` `name` by `method` (`thread/name/set`).
pub fn set_name<R: Read, W: Write>(rpc: &mut Rpc<R, W>, method: &str, id: &str, name: &str) -> Result<()> {
    rpc.call(method, json!({"threadId": id, "name": name}))?;
    Ok(())
}

/// Interrupts the turn the thread `id` is running (`turn/interrupt`).
pub fn interrupt<R: Read, W: Write>(rpc: &mut Rpc<R, W>, id: &str) -> Result<()> {
    let read = rpc.call("thread/read", json!({"threadId": id, "includeTurns": true}))?;
    let turn = read["thread"]["turns"]
        .as_array()
        .and_then(|turns| turns.iter().rev().find(|t| t["status"] == "inProgress"))
        .and_then(|t| t["id"].as_str())
        .context("no turn is running in it")?
        .to_string();
    rpc.call("turn/interrupt", json!({"threadId": id, "turnId": turn}))?;
    Ok(())
}

/// Deletes the thread `id` for good (`thread/delete`); with `stops`, the
/// turn it is running interrupted first. One that cannot be read (at rest,
/// gone) has no turn running.
pub fn delete<R: Read, W: Write>(rpc: &mut Rpc<R, W>, id: &str, stops: bool) -> Result<()> {
    if stops
        && let Ok(read) = rpc.call("thread/read", json!({"threadId": id, "includeTurns": true}))
        && let Some(turn) = read["thread"]["turns"]
            .as_array()
            .and_then(|turns| turns.iter().rev().find(|t| t["status"] == "inProgress"))
            .and_then(|t| t["id"].as_str())
    {
        rpc.call("turn/interrupt", json!({"threadId": id, "turnId": turn}))?;
    }
    rpc.call("thread/delete", json!({"threadId": id}))?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Loaded {
    data: Vec<String>,
    #[serde(default)]
    next_cursor: Option<String>,
}

/// A page of `thread/list`, each thread read on its own: one that does not
/// read is left out, not the page.
#[derive(Deserialize)]
struct Listed {
    data: Vec<Value>,
}

#[derive(Deserialize)]
struct Read1 {
    thread: Thread,
}

/// JSON-RPC calls over the WebSocket, answered in any order, among
/// notifications.
pub struct Rpc<R: Read, W: Write> {
    ws: Ws<R, W>,
    next: u64,
    /// The Codex behind it, where known: whose version a drift names.
    pub bin: Option<PathBuf>,
}

/// JSON-RPC's codes for a method the server does not have, and for params
/// it does not take: what a daemon that changed says.
const NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

impl<R: Read, W: Write> Rpc<R, W> {
    /// Takes the WebSocket that `r` and `w` carry, and introduces Relay
    /// (`initialize`, then `initialized`) as `CLIENT`.
    pub fn open(r: R, w: W) -> Result<Rpc<R, W>> {
        let mut rpc = Rpc {
            ws: Ws::open(r, w)?,
            next: 0,
            bin: None,
        };
        rpc.call(
            "initialize",
            json!({"clientInfo": {"name": CLIENT, "title": "Relay", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        rpc.ws.send(&json!({"jsonrpc": "2.0", "method": "initialized"}))?;
        Ok(rpc)
    }

    /// One call: its result, or its error as `<method>: <message>`.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.call_all(vec![(method, params)])?
            .remove(0)
            .map_err(|e| anyhow!("{method}: {e}"))
    }

    /// Sends every call, then reads until each is answered: its result, or
    /// the error's message, in the order called.
    fn call_all(&mut self, calls: Vec<(&str, Value)>) -> Result<Vec<Result<Value, String>>> {
        let first = self.next + 1;
        for (method, params) in calls.iter() {
            self.next += 1;
            self.ws
                .send(&json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}))?;
        }
        let mut answers: HashMap<u64, Result<Value, String>> = HashMap::new();
        while answers.len() < calls.len() {
            let m: Value = serde_json::from_str(&self.ws.recv()?).context("reading an app-server message")?;
            let Some(id) = m.get("id").and_then(Value::as_u64) else {
                continue;
            };
            if !(first..=self.next).contains(&id) || m.get("method").is_some() {
                continue;
            }
            if let Some(e) = m.get("error")
                && let Some(code @ (NOT_FOUND | INVALID_PARAMS)) = e.get("code").and_then(Value::as_i64)
                && let Some((method, _)) = calls.get((id - first) as usize)
            {
                let why = if code == NOT_FOUND {
                    "does not know"
                } else {
                    "does not take the params Relay sends to"
                };
                let message = e.get("message").and_then(Value::as_str).unwrap_or("");
                crate::drift::notice(
                    crate::drift::Noticed::new(
                        crate::user_hooks::CODEX,
                        self.bin.as_deref(),
                        &format!("codex.rpc.{method}"),
                        format!("its daemon {why} `{method}` ({code})"),
                    )
                    .with_evidence(message),
                );
            }
            let said = match m.get("error") {
                Some(e) => Err(e
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("no reason given")
                    .to_string()),
                None => Ok(m.get("result").cloned().unwrap_or(Value::Null)),
            };
            answers.insert(id, said);
        }
        Ok((first..=self.next).filter_map(|id| answers.remove(&id)).collect())
    }
}

/// The client end of a WebSocket (RFC 6455), as much of it as the
/// app-server needs: text messages, a ping answered, a close an error.
struct Ws<R: Read, W: Write> {
    r: BufReader<R>,
    w: W,
}

/// The largest message read; the app-server says it sends none larger
/// unfragmented (`x-codex-websocket-max-unfragmented-message-bytes`).
const MAX_MESSAGE: u64 = 16 << 20;

/// The handshake's key. Any sixteen bytes do: it proves nothing on a
/// socket only this user can open (RFC 6455's own sample).
const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

/// What every frame Relay sends is masked with; a client must mask, and
/// nothing between it and the daemon reads it.
const MASK: [u8; 4] = [0x52, 0x6c, 0x61, 0x79];

impl<R: Read, W: Write> Ws<R, W> {
    /// Asks to upgrade, and reads the answer's head to its end.
    fn open(r: R, mut w: W) -> Result<Ws<R, W>> {
        write!(
            w,
            "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )?;
        w.flush()?;
        let mut r = BufReader::new(r);
        let mut status = String::new();
        if r.read_line(&mut status)? == 0 {
            bail!("the app-server closed before it answered");
        }
        if status.split_whitespace().nth(1) != Some("101") {
            bail!("the app-server did not take the WebSocket: {}", status.trim());
        }
        loop {
            let mut line = String::new();
            if r.read_line(&mut line)? == 0 {
                bail!("the app-server closed in its answer");
            }
            if line.trim().is_empty() {
                break;
            }
        }
        Ok(Ws { r, w })
    }

    fn frame(&mut self, opcode: u8, data: &[u8]) -> Result<()> {
        let mut f = vec![0x80 | opcode];
        let n = data.len();
        match n {
            0..=125 => f.push(0x80 | n as u8),
            126..=0xffff => {
                f.push(0x80 | 126);
                f.extend((n as u16).to_be_bytes());
            }
            _ => {
                f.push(0x80 | 127);
                f.extend((n as u64).to_be_bytes());
            }
        }
        f.extend(MASK);
        f.extend(data.iter().enumerate().map(|(i, b)| b ^ MASK[i % 4]));
        self.w.write_all(&f)?;
        self.w.flush()?;
        Ok(())
    }

    fn send(&mut self, m: &Value) -> Result<()> {
        self.frame(0x1, m.to_string().as_bytes())
    }

    /// The next text message, whole.
    fn recv(&mut self) -> Result<String> {
        let mut message = Vec::new();
        loop {
            let mut head = [0u8; 2];
            self.r.read_exact(&mut head).context("the app-server closed")?;
            let (fin, opcode, masked) = (head[0] & 0x80 != 0, head[0] & 0x0f, head[1] & 0x80 != 0);
            let n = match head[1] & 0x7f {
                126 => {
                    let mut b = [0u8; 2];
                    self.r.read_exact(&mut b)?;
                    u64::from(u16::from_be_bytes(b))
                }
                127 => {
                    let mut b = [0u8; 8];
                    self.r.read_exact(&mut b)?;
                    u64::from_be_bytes(b)
                }
                n => u64::from(n),
            };
            if n + message.len() as u64 > MAX_MESSAGE {
                bail!("the app-server sent a message larger than {MAX_MESSAGE} bytes");
            }
            let mut mask = [0u8; 4];
            if masked {
                self.r.read_exact(&mut mask)?;
            }
            let mut data = vec![0u8; n as usize];
            self.r.read_exact(&mut data)?;
            if masked {
                data.iter_mut().enumerate().for_each(|(i, b)| *b ^= mask[i % 4]);
            }
            match opcode {
                0x0 | 0x1 => {
                    message.extend(data);
                    if fin {
                        return String::from_utf8(message).context("the app-server sent no text");
                    }
                }
                0x8 => bail!("the app-server closed the connection"),
                0x9 => self.frame(0xa, &data)?,
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Registry;
    use std::os::unix::net::UnixStream;
    use std::path::Path;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/codex-native")
    }

    /// What the app-server answered, recorded (`answers.json`): by method,
    /// and `thread/read` by thread id.
    fn answers() -> Value {
        serde_json::from_str(&std::fs::read_to_string(fixtures().join("answers.json")).unwrap()).unwrap()
    }

    /// A stand-in app-server: takes the upgrade, then answers each call as
    /// recorded (its id its caller's), the notifications first. Returns
    /// what it was sent, each message whole.
    fn serve(s: UnixStream) -> std::thread::JoinHandle<Vec<Value>> {
        std::thread::spawn(move || {
            let answers = answers();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut w = s;
            let mut asked = String::new();
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                asked.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            assert!(asked.contains("Upgrade: websocket"), "{asked}");
            w.write_all(b"HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\n\r\n")
                .unwrap();
            let send = |w: &mut UnixStream, m: &Value| {
                let data = m.to_string().into_bytes();
                let mut f = vec![0x81];
                match data.len() {
                    n @ 0..=125 => f.push(n as u8),
                    n => {
                        f.push(126);
                        f.extend((n as u16).to_be_bytes());
                    }
                }
                f.extend(data);
                w.write_all(&f).unwrap();
            };
            for n in answers["notifications"].as_array().unwrap() {
                send(&mut w, n);
            }
            let mut sent = Vec::new();
            let mut ws = Ws { r, w: std::io::sink() };
            while let Ok(text) = ws.recv() {
                let m: Value = serde_json::from_str(&text).unwrap();
                let method = m["method"].as_str().unwrap().to_string();
                sent.push(m.clone());
                let Some(id) = m.get("id") else { continue };
                let mut answer = match method.as_str() {
                    "thread/read" => answers["thread/read"][m["params"]["threadId"].as_str().unwrap()].clone(),
                    _ => answers[method.as_str()].clone(),
                };
                answer["id"] = id.clone();
                send(&mut w, &answer);
            }
            sent
        })
    }

    fn methods(sent: &[Value]) -> Vec<&str> {
        sent.iter().map(|m| m["method"].as_str().unwrap()).collect()
    }

    /// What the stand-in app-server was sent while `talk` spoke to it.
    fn talk<T>(talk: impl FnOnce(&mut Rpc<UnixStream, UnixStream>) -> T) -> (T, Vec<Value>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let server = serve(theirs);
        let mut rpc = Rpc::open(ours.try_clone().unwrap(), ours.try_clone().unwrap()).unwrap();
        let said = talk(&mut rpc);
        ours.shutdown(std::net::Shutdown::Both).unwrap();
        (said, server.join().unwrap())
    }

    #[test]
    fn it_asks_the_daemon_for_its_loaded_threads_over_a_websocket() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let server = serve(theirs);
        let threads = threads(ours.try_clone().unwrap(), ours.try_clone().unwrap()).unwrap();
        ours.shutdown(std::net::Shutdown::Both).unwrap();
        let ids: Vec<&str> = threads.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "01a1b669-4a41-7f31-977b-e8435c8a708a",
                "01a11d20-8cab-7061-885d-f86e8e313f11",
                "01a0b3af-810e-7c22-a2e1-4f7a99ebbd3a",
                "019eb0b3-b03b-7393-915f-967456b2082a",
                "01a1b670-0000-7000-8000-000000000001",
                "01a1ca12-0000-7000-8000-00000000000a",
            ],
            "one gone before it was read is left out; Relay's own at rest after"
        );
        let sent = server.join().unwrap();
        let methods = methods(&sent);
        assert_eq!(&methods[..3], ["initialize", "initialized", "thread/loaded/list"]);
        assert_eq!(
            sent[0]["params"]["clientInfo"]["name"], CLIENT,
            "how its threads are known again"
        );
        assert!(
            methods[3..9].iter().all(|m| *m == "thread/read"),
            "only reads: {methods:?}"
        );
        assert_eq!(methods[9..], ["thread/list"]);
    }

    #[test]
    fn a_start_is_a_thread_in_the_folder_and_its_first_turn_on_the_prompt() {
        let (id, sent) = talk(|rpc| begin(rpc, Path::new("/w/relay"), Some("login"), "fix the login").unwrap());
        assert_eq!(id, "01a1ca12-27f5-7ca0-a8ef-8593acea7ed9");
        assert_eq!(
            methods(&sent),
            [
                "initialize",
                "initialized",
                "thread/start",
                "thread/name/set",
                "turn/start"
            ]
        );
        assert_eq!(
            sent[2]["params"],
            json!({"cwd": "/w/relay"}),
            "the folder only: your own approvals and sandbox hold"
        );
        assert_eq!(sent[3]["params"], json!({"threadId": id, "name": "login"}));
        assert_eq!(
            sent[4]["params"],
            json!({"threadId": id, "input": [{"type": "text", "text": "fix the login"}]})
        );
        let (_, sent) = talk(|rpc| begin(rpc, Path::new("/w/relay"), None, "x").unwrap());
        assert!(!methods(&sent).contains(&"thread/name/set"), "no name, none set");
    }

    #[test]
    fn a_rename_names_the_thread_on_the_daemon() {
        let id = "01a1ca12-27f5-7ca0-a8ef-8593acea7ed9";
        let ((), sent) = talk(|rpc| set_name(rpc, "thread/name/set", id, "login").unwrap());
        assert_eq!(methods(&sent)[2..], ["thread/name/set"]);
        assert_eq!(sent[2]["params"], json!({"threadId": id, "name": "login"}));
        assert_eq!(
            codex("codex".into()).rename_method().as_deref(),
            Some("thread/name/set")
        );
    }

    #[test]
    fn a_stop_interrupts_the_turn_running() {
        let ((), sent) = talk(|rpc| interrupt(rpc, "01a1ca12-27f5-7ca0-a8ef-8593acea7ed9").unwrap());
        assert_eq!(
            methods(&sent)[2..],
            ["thread/read", "turn/interrupt"],
            "its turns read, the running one interrupted"
        );
        assert_eq!(
            sent[3]["params"],
            json!({"threadId": "01a1ca12-27f5-7ca0-a8ef-8593acea7ed9", "turnId": "01a1ca12-2800-7260-8442-e7a848675cd1"})
        );
        let (e, _) = talk(|rpc| interrupt(rpc, "01a11d20-8cab-7061-885d-f86e8e313f11").unwrap_err());
        assert_eq!(e.to_string(), "no turn is running in it");
    }

    #[test]
    fn a_remove_deletes_the_thread_its_running_turn_interrupted_first() {
        let busy = "01a1ca12-27f5-7ca0-a8ef-8593acea7ed9";
        let ((), sent) = talk(|rpc| delete(rpc, busy, true).unwrap());
        assert_eq!(methods(&sent)[2..], ["thread/read", "turn/interrupt", "thread/delete"]);
        assert_eq!(sent[4]["params"], json!({"threadId": busy}));
        let resting = "01a11d20-8cab-7061-885d-f86e8e313f11";
        let ((), sent) = talk(|rpc| delete(rpc, resting, true).unwrap());
        assert_eq!(
            methods(&sent)[2..],
            ["thread/read", "thread/delete"],
            "nothing running: deleted as it is"
        );
        let ((), sent) = talk(|rpc| delete(rpc, busy, false).unwrap());
        assert_eq!(methods(&sent)[2..], ["thread/delete"], "no stop on this codex");
    }

    fn listed() -> Vec<NativeRow> {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let _server = serve(theirs);
        let t = threads(ours.try_clone().unwrap(), ours.try_clone().unwrap()).unwrap();
        ours.shutdown(std::net::Shutdown::Both).unwrap();
        rows(t)
    }

    #[test]
    fn the_rows_are_codexs_sessions_in_its_own_words() {
        let rows = listed();
        let seen: Vec<(&str, &str, &str, RunState)> = rows
            .iter()
            .map(|r| (r.id.as_str(), r.name.as_str(), r.state.words.as_str(), r.state.state))
            .collect();
        assert_eq!(
            seen,
            [
                (
                    "01a1b669-4a41-7f31-977b-e8435c8a708a",
                    "What’s worthwhile doing",
                    "needs input",
                    RunState::NeedsInput
                ),
                (
                    "01a11d20-8cab-7061-885d-f86e8e313f11",
                    "Assess this project",
                    "working",
                    RunState::Working
                ),
                (
                    "01a0b3af-810e-7c22-a2e1-4f7a99ebbd3a",
                    "what   can oyu do",
                    "ready",
                    RunState::Idle
                ),
                (
                    "019eb0b3-b03b-7393-915f-967456b2082a",
                    "Review PR 243 changes",
                    "error",
                    RunState::Failed
                ),
                (
                    "01a1ca12-0000-7000-8000-00000000000a",
                    "fix the login",
                    "resting",
                    RunState::Idle
                ),
            ],
            "unnamed: its first prompt's first line; a sub-agent's thread is not a session; \
             of the threads at rest only Relay's, and one loaded once"
        );
        let r = &rows[1];
        assert_eq!((r.harness.as_str(), r.cwd.as_path()), ("codex", Path::new("/w/relay")));
        assert_eq!(r.session_id, r.id, "what its hooks tell of");
        assert_eq!(r.started, Timestamp::from_second(1791490034).ok());
        assert_eq!(r.at, Timestamp::from_second(1791720371).ok(), "when it last moved");
        assert_eq!((r.ended, r.summary.as_ref(), r.pr.as_ref()), (None, None, None));
    }

    #[test]
    fn the_state_words_cover_what_codex_says() {
        let s = |kind: &str, flags: &[&str]| {
            state(&Status {
                kind: kind.into(),
                active_flags: flags.iter().map(|f| f.to_string()).collect(),
            })
        };
        assert_eq!(s("active", &["waitingOnUserInput"]).words, "needs input");
        assert_eq!(s("active", &["somethingNew"]).state, RunState::Working);
        let new = s("pondering", &[]);
        assert_eq!(
            (new.words.as_str(), new.state),
            ("pondering", RunState::Idle),
            "kept as said"
        );
        let disk = Thread {
            id: "t".into(),
            session_id: None,
            name: None,
            preview: None,
            cwd: "/w/x".into(),
            status: Status {
                kind: "notLoaded".into(),
                active_flags: Vec::new(),
            },
            parent_thread_id: None,
            originator: None,
            created_at: None,
            updated_at: None,
        };
        let relative = Thread {
            cwd: "w/x".into(),
            status: Status {
                kind: "idle".into(),
                active_flags: Vec::new(),
            },
            ..disk.clone()
        };
        assert!(
            rows(vec![disk.clone(), relative.clone()]).is_empty(),
            "only on disk; no whole folder"
        );
        let bare = Thread {
            cwd: "/w/x".into(),
            ..relative
        };
        assert!(
            rows(vec![bare.clone()]).is_empty(),
            "no name, no prompt: an empty thread Codex's TUI made, no session"
        );
        let blank = Thread {
            preview: Some("  \n".into()),
            name: Some(String::new()),
            ..bare.clone()
        };
        assert!(rows(vec![blank]).is_empty(), "blank is empty too");
        let prompted = Thread {
            preview: Some("\nfix the login\nand more".into()),
            ..bare.clone()
        };
        let r = &rows(vec![prompted])[0];
        assert_eq!(
            (r.name.as_str(), r.session_id.as_str(), r.at),
            ("fix the login", "t", None),
            "its first prompt's first line"
        );
        let named = Thread {
            name: Some("login".into()),
            ..bare
        };
        assert_eq!(rows(vec![named])[0].name, "login", "named, though nothing asked yet");
    }

    fn codex(bin: PathBuf) -> Codex {
        let r = Registry::builtin();
        Codex::of(r.profile("codex").unwrap(), bin).unwrap()
    }

    #[test]
    fn it_runs_what_the_profile_says() {
        let c = codex("codex".into());
        assert_eq!(c.argv(&c.list), ["codex", "app-server", "proxy"]);
        assert_eq!(
            c.attach_argv("01a11d20-8cab-7061-885d-f86e8e313f11").unwrap(),
            ["codex", "resume", "01a11d20-8cab-7061-885d-f86e8e313f11"]
        );
        assert_eq!(
            Steps::start_in(&c, Path::new("/w"), None, None, &HashSet::new()).unwrap(),
            Started::Here(vec!["codex".into()]),
            "no prompt: its TUI, here"
        );
        assert!(c.never_held());
        assert_eq!(
            c.carry_on("s"),
            Some(CarryOn::Attach(vec!["codex".into(), "resume".into(), "s".into()])),
            "a thread is on the daemon already: resumed there"
        );
        let mut row = row(Thread {
            id: "t".into(),
            session_id: None,
            name: None,
            preview: None,
            cwd: "/w".into(),
            status: Status {
                kind: "active".into(),
                active_flags: Vec::new(),
            },
            parent_thread_id: None,
            originator: None,
            created_at: None,
            updated_at: None,
        });
        assert!(c.stops(&row), "a turn running");
        row.state = state(&Status {
            kind: "idle".into(),
            active_flags: Vec::new(),
        });
        assert!(!c.stops(&row), "nothing running to stop");
        assert_eq!(
            c.ping_argv("01a1", "mail from x").unwrap(),
            [
                "codex",
                "queue",
                "--thread",
                "01a1",
                "--message",
                "mail from x",
                "--resume"
            ]
        );
    }

    fn script(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("codex");
        std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[test]
    fn with_no_daemon_codex_has_no_sessions_of_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        // What the proxy says with no socket to connect to.
        let bin = script(
            tmp.path(),
            "[ \"$*\" = 'app-server proxy' ] && [ -n \"$RELAY_ASKING\" ] || exit 9\n\
             echo 'Error: failed to connect to socket at /h/.codex/app-server-control/app-server-control.sock' >&2\n\
             exit 1",
        );
        assert_eq!(codex(bin).list().unwrap(), Vec::new());
    }

    #[test]
    fn a_proxy_that_fails_or_hangs_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = script(tmp.path(), "echo 'Error: something else' >&2; exit 2");
        let e = codex(bin).list().unwrap_err();
        assert_eq!(e.to_string(), "`codex app-server proxy` failed: Error: something else");
        let bin = script(tmp.path(), "exec sleep 30");
        let at = std::time::Instant::now();
        let e = ask(&[bin.to_string_lossy().into_owned()], Duration::from_millis(300)).unwrap_err();
        assert!(e.to_string().contains("did not answer"), "{e}");
        assert!(at.elapsed() < Duration::from_secs(10), "given up at the wait");
    }

    #[test]
    fn a_hung_proxy_goes_with_what_it_started() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = tmp.path().join("pid");
        // The pid is put in place whole (`mv`), so it is there entire or not
        // at all.
        let bin = script(
            tmp.path(),
            &format!(
                "sleep 20 &\necho $! > '{p}.tmp' && mv '{p}.tmp' '{p}'\nwait",
                p = pid.display()
            ),
        );
        // Under load the proxy can be given up on before it has started
        // anything: then there is nothing to have gone, and it is asked again.
        let mut started = None;
        for _ in 0..10 {
            let at = std::time::Instant::now();
            let e = ask(&[bin.to_string_lossy().into_owned()], Duration::from_millis(500)).unwrap_err();
            assert!(e.to_string().contains("did not answer"), "{e}");
            assert!(
                at.elapsed() < Duration::from_secs(10),
                "given up at the wait: {:?}",
                at.elapsed()
            );
            if let Ok(p) = std::fs::read_to_string(&pid) {
                started = Some(p.trim().parse::<i32>().unwrap());
                break;
            }
        }
        let pid = started.expect("the proxy started its child in one of ten tries");
        let by = std::time::Instant::now() + Duration::from_secs(20);
        let gone = || nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err();
        while !gone() && std::time::Instant::now() < by {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gone(), "its whole group is killed");
    }

    fn schema_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/codex-schema")
    }

    /// A copy of the schema fixture in `dir`, as `edit` leaves it.
    fn schema_with(dir: &Path, edit: impl FnOnce(&mut Value, &mut Value)) -> PathBuf {
        let out = dir.join("schema");
        std::fs::create_dir_all(out.join("v2")).unwrap();
        for f in std::fs::read_dir(schema_fixture().join("v2")).unwrap() {
            let f = f.unwrap();
            std::fs::copy(f.path(), out.join("v2").join(f.file_name())).unwrap();
        }
        let read =
            |f: &str| -> Value { serde_json::from_slice(&std::fs::read(schema_fixture().join(f)).unwrap()).unwrap() };
        let mut req = read("ClientRequest.json");
        let mut thread = read(THREAD_READ);
        edit(&mut req, &mut thread);
        std::fs::write(out.join("ClientRequest.json"), req.to_string()).unwrap();
        std::fs::write(out.join(THREAD_READ), thread.to_string()).unwrap();
        out
    }

    #[test]
    fn the_schema_codex_writes_has_everything_relay_uses() {
        assert_eq!(
            check_schema(&schema_fixture()).unwrap(),
            vec![],
            "codex 0.167's, trimmed"
        );
        let tmp = tempfile::tempdir().unwrap();
        assert!(check_schema(tmp.path()).is_err(), "no schema: cannot tell");
    }

    #[test]
    fn a_method_or_field_gone_turns_off_only_what_needs_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = schema_with(tmp.path(), |req, thread| {
            req["oneOf"].as_array_mut().unwrap().retain(|e| {
                !matches!(
                    e["properties"]["method"]["enum"][0].as_str(),
                    Some("thread/name/set" | "thread/delete")
                )
            });
            req["definitions"]["TurnInterruptParams"]["properties"]
                .as_object_mut()
                .unwrap()
                .remove("turnId");
            thread["definitions"]["Thread"]["properties"]
                .as_object_mut()
                .unwrap()
                .remove("cwd");
        });
        let gone = check_schema(&dir).unwrap();
        let words: Vec<String> = gone.iter().map(Gone::words).collect();
        assert_eq!(
            words,
            [
                "thread/read lacks Thread.cwd, its threads are not listed",
                "thread/name/set gone, rename stays in Relay",
                "turn/interrupt lacks params.turnId, Relay cannot stop its turns",
                "thread/delete gone, Relay cannot remove its threads",
            ]
        );
        assert_eq!(gone[1].check(), "codex.schema.thread/name/set");
        assert_eq!(gone[3].check(), "codex.schema.thread/delete");
        let c = Compat {
            version: Some("0.170.0".into()),
            gone,
        };
        assert_eq!(
            c.why(Feature::Rename).as_deref(),
            Some("codex 0.170.0: thread/name/set gone, rename stays in Relay")
        );
        assert!(c.off(Feature::Start).is_none() && c.off(Feature::Resting).is_none());
    }

    #[test]
    fn a_codex_without_thread_name_set_keeps_renames_in_relay_and_says_so_once() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("codex");
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/stand-in-codex-daemon"),
            &bin,
        )
        .unwrap();
        std::fs::write(tmp.path().join("codex.schema"), schema_fixture().display().to_string()).unwrap();
        std::fs::write(tmp.path().join("codex.schema-drop"), "thread/name/set\n").unwrap();
        let h = crate::harness::of(Registry::builtin().profile("codex").unwrap(), Some(bin.clone()));
        let row = NativeRow {
            harness: "codex".into(),
            id: "t1".into(),
            session_id: "t1".into(),
            name: "x".into(),
            cwd: "/w".into(),
            state: state(&Status {
                kind: "active".into(),
                active_flags: vec![],
            }),
            summary: None,
            started: None,
            ended: None,
            at: None,
            pr: None,
            color: None,
        };
        assert!(h.renames(&row, "y"), "not known yet: as ever");
        let (e, noticed) = crate::drift::capture(|| Steps::rename_row(h.as_ref(), &row, "y", tmp.path()).unwrap_err());
        assert_eq!(
            e.to_string(),
            "codex 0.167.2: thread/name/set gone, rename stays in Relay"
        );
        assert_eq!(noticed.len(), 1);
        assert_eq!(noticed[0].check, "codex.schema.thread/name/set");
        assert_eq!(noticed[0].bin.as_deref(), Some(bin.as_path()));
        assert!(!h.renames(&row, "y"), "known now: the name stays in Relay");
        assert!(h.stops(&row), "the rest goes on");
        let ((), again) = crate::drift::capture(|| {
            let _ = Steps::rename_row(h.as_ref(), &row, "y", tmp.path());
        });
        assert!(again.is_empty(), "checked once per binary");
    }

    #[test]
    fn a_method_the_daemon_does_not_know_is_drift() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut r = BufReader::new(theirs.try_clone().unwrap());
            let mut w = theirs;
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            w.write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n").unwrap();
            let mut ws = Ws { r, w: std::io::sink() };
            while let Ok(text) = ws.recv() {
                let m: Value = serde_json::from_str(&text).unwrap();
                let Some(id) = m.get("id") else { continue };
                let answer = match m["method"].as_str().unwrap() {
                    "initialize" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
                    "thread/start" => {
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "unknown field `cwd` in /srv/ann/x"}})
                    }
                    "thread/read" => {
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32600, "message": "thread not found"}})
                    }
                    m => {
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {m}")}})
                    }
                };
                let data = answer.to_string().into_bytes();
                let mut f = vec![0x81, data.len() as u8];
                f.extend(data);
                w.write_all(&f).unwrap();
            }
        });
        let mut rpc = Rpc::open(ours.try_clone().unwrap(), ours.try_clone().unwrap()).unwrap();
        let ((), noticed) = crate::drift::capture(|| {
            assert!(set_name(&mut rpc, "thread/name/set", "t1", "x").is_err());
            assert!(rpc.call("thread/read", json!({"threadId": "t1"})).is_err());
            assert!(rpc.call("thread/start", json!({"cwd": "/w"})).is_err());
            assert!(delete(&mut rpc, "t1", true).is_err());
        });
        ours.shutdown(std::net::Shutdown::Both).unwrap();
        server.join().unwrap();
        let checks: Vec<&str> = noticed.iter().map(|n| n.check.as_str()).collect();
        assert_eq!(
            checks,
            [
                "codex.rpc.thread/name/set",
                "codex.rpc.thread/start",
                "codex.rpc.thread/delete"
            ],
            "a thread not found is no drift"
        );
        assert_eq!(noticed[0].what, "its daemon does not know `thread/name/set` (-32601)");
        assert_eq!(
            noticed[1].evidence.as_deref(),
            Some("unknown field `cwd` in /<project>")
        );
    }

    /// Against the real daemon: `cargo test -p rl-detect -- --ignored`.
    /// Only reads.
    #[test]
    #[ignore = "asks the real codex app-server daemon"]
    fn the_real_codex_lists_its_loaded_threads() {
        let r = Registry::builtin();
        let p = r.profile("codex").unwrap();
        let bin = crate::registry::find_bin(&p.bins).expect("codex on PATH");
        for row in Codex::of(p, bin).unwrap().list().unwrap() {
            assert!(row.cwd.is_absolute(), "{row:?}");
            assert!(!row.state.words.is_empty(), "{row:?}");
        }
    }
}
