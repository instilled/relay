//! An agent's own sessions, as its profile tells of them: the `sessions:`
//! block of `detect/<agent>.yaml`. An agent with none has no own sessions
//! on the list; one with a block is told of by its hooks (`hooks`), listed
//! by asking it (`list`), and opened where it runs in its own background
//! (`attach`). The facts per agent are all here, read from its profile;
//! what is the same for every agent is code: the hooks file's format
//! (`user_hooks`) and `rl hook`.
//!
//! ```yaml
//! sessions:
//!   hooks:
//!     file: ["$CODEX_HOME/hooks.json", "~/.codex/hooks.json"]
//!     timeout: 3s
//!     unit: s
//!     async: true
//!     after_install: "codex asks you to trust it once: open codex, /hooks"
//!     events:
//!       SessionStart: started
//!       UserPromptSubmit: { state: working, prompt: true }
//!       PermissionRequest: { state: needs, waiting_for: "permission prompt",
//!                            detail: tool_name }
//!       PostToolUse: carries-on
//!       Stop: idle
//!       SessionEnd: ended
//!   list: { args: [agents, --json, --all], reader: claude-agents }
//!   attach: [attach, "{id}"]
//!   start: [--bg]
//!   name: [-n, "{name}"]
//!   stop: [stop, "{id}"]
//! ```
//!
//! What an event says is a state word, or the same in a map with more:
//! - `started`: a session begins, and stands idle. One known already is
//!   not changed by it (its start may come after its first prompt), and a
//!   session its process held before is over.
//! - `idle`, `working`, `needs` (it waits on you; `waiting_for` says on
//!   what), `ended`, `nothing`.
//! - `carries-on`: a tool done. It ends a wait and keeps a turn working,
//!   and never wakes an idle session.
//! - `prompt: true`: the payload's `prompt` is the one you typed, which
//!   names a run of the agent's that has no name yet.
//! - `detail`: the payload field `rl hook` keeps along with the event.
//!
//! An event can be narrowed by one field of its payload: `{ by:
//! notification_type, permission_prompt: needs, idle_prompt: idle }`. A
//! value not listed says nothing.

use crate::claude::clip;
use anyhow::{Result, bail};
use rl_core::time::parse_duration;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::path::PathBuf;

/// The list readers Relay has, by the name a profile's `list.reader` gives:
/// each is a harness's with a background of its own (`harness::of`), reading
/// one agent's own
/// listing of its sessions.
pub const READERS: [&str; 2] = [crate::claude_native::READER, crate::codex_native::READER];

/// How one of an agent's own sessions stands, as `Told::Stands` says: it
/// waits for a prompt, works, or waits on you.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Busy,
    Waiting,
}

/// Where a session's attach id goes in a profile's `attach`.
pub const ID: &str = "{id}";

/// Where a session's name goes in a profile's `name`.
pub const NAME: &str = "{name}";

/// Where a ping's words go in a profile's `ping`.
pub const TEXT: &str = "{text}";

/// The `sessions:` block as written. Everything optional so files can be
/// merged, field by field, a user's over a built-in.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionsFile {
    hooks: Option<HooksFile>,
    list: Option<ListFile>,
    attach: Option<Vec<String>>,
    start: Option<Vec<String>>,
    name: Option<Vec<String>>,
    stop: Option<Vec<String>>,
    ping: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct HooksFile {
    file: Option<Vec<String>>,
    timeout: Option<String>,
    unit: Option<String>,
    #[serde(rename = "async")]
    background: Option<bool>,
    marker: Option<String>,
    after_install: Option<String>,
    from_env: Option<FromEnv>,
    events: Option<Map<String, Value>>,
    settings: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListFile {
    args: Vec<String>,
    reader: String,
}

/// Variables the agent sets for its hooks, read where the payload lacks
/// the session's id or folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FromEnv {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
}

impl SessionsFile {
    pub(crate) fn merge(&mut self, over: SessionsFile) {
        if let Some(h) = over.hooks {
            let mine = self.hooks.get_or_insert_with(Default::default);
            macro_rules! take {
                ($($f:ident),*) => {$(if h.$f.is_some() { mine.$f = h.$f; })*};
            }
            take!(
                file,
                timeout,
                unit,
                background,
                marker,
                after_install,
                from_env,
                events,
                settings
            );
        }
        if over.list.is_some() {
            self.list = over.list;
        }
        if over.attach.is_some() {
            self.attach = over.attach;
        }
        if over.start.is_some() {
            self.start = over.start;
        }
        if over.name.is_some() {
            self.name = over.name;
        }
        if over.stop.is_some() {
            self.stop = over.stop;
        }
        if over.ping.is_some() {
            self.ping = over.ping;
        }
    }
}

/// An agent's own sessions, as its profile tells of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sessions {
    /// Its hooks, which tell of them; none, and it has no hooks Relay sets.
    pub hooks: Option<UserHooks>,
    /// How it lists them itself; none, and only its hooks tell.
    pub list: Option<List>,
    /// The arguments that open one in its own background here, after its
    /// binary: `{id}` is the session's attach id. Empty where it has none.
    pub attach: Vec<String>,
    /// The arguments that start one in its own background, after its
    /// binary (`[--bg]`); a prompt goes last. Empty where it has no
    /// background: Relay holds its sessions.
    pub start: Vec<String>,
    /// What names one it starts, after `start`: `{name}` is the name.
    pub name: Vec<String>,
    /// The arguments that stop one in its own background, after its
    /// binary: `{id}` is its attach id. Empty where it has no way to.
    pub stop: Vec<String>,
    /// The arguments that tell one of its sessions something from outside
    /// it, after its binary: `{id}` is the session's (codex: its thread's),
    /// `{text}` the words. Empty where nothing outside can.
    pub ping: Vec<String>,
}

/// How an agent lists its own sessions: its binary with `args`, read by
/// one of the `READERS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    pub args: Vec<String>,
    pub reader: String,
}

/// The unit the hooks file writes a timeout in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Seconds,
    Millis,
}

/// An agent's own hooks: where they go, how each is written, and what each
/// event says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserHooks {
    /// The profile's name, which the hook's command says.
    pub agent: String,
    /// Where the file is: the first of these whose variables are all set
    /// (`$NAME`; `~` is `$HOME`).
    pub file: Vec<String>,
    /// How long the agent waits for a hook, in `unit`.
    pub timeout: u64,
    pub unit: Unit,
    /// Whether the agent runs the hooks in its background (codex's
    /// `async`). Never an end's: the agent may not wait for it as it exits.
    pub background: bool,
    /// The word after the event in the command, where a Relay run of the
    /// agent's has hooks of its own beside these (Claude's): it tells this
    /// copy apart, and this copy is then never the run's signal.
    pub marker: Option<String>,
    /// What the agent asks of you once its hooks are added, if anything.
    pub after_install: Option<String>,
    pub from_env: FromEnv,
    /// The events hooked, in order, and what each says.
    pub events: Vec<(String, Event)>,
    /// The agent's own settings set beside the hooks, each by its dotted
    /// path in the file (`ui.useAlternateBuffer`), in order: what makes it
    /// draw well under Relay. Set only where the file has no value there.
    pub settings: Vec<(String, Value)>,
}

/// What an event says: always the same, or by one field of its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Says(Says),
    By { field: String, cases: Vec<(String, Says)> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Says {
    pub state: State,
    pub prompt: bool,
    pub waiting_for: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Started,
    Idle,
    Working,
    Needs,
    CarriesOn,
    Ended,
    Nothing,
}

const STATES: &str = "started, idle, working, needs, carries-on, ended, nothing";

impl State {
    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "started" => Self::Started,
            "idle" => Self::Idle,
            "working" => Self::Working,
            "needs" => Self::Needs,
            "carries-on" => Self::CarriesOn,
            "ended" => Self::Ended,
            "nothing" => Self::Nothing,
            _ => return None,
        })
    }
}

/// What an event says about one of an agent's own sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Told<'a> {
    /// It stands so now: a `Status`,
    /// and what it waits on, if anything.
    Stands {
        status: Status,
        waiting_for: Option<&'a str>,
    },
    /// It is over.
    Ended,
    /// Nothing about how it stands; only that it is still there.
    Nothing,
}

/// The word `rl hook` knows after the event: the only marker there is.
pub const MARKER: &str = "--user";

/// The block, compiled: every word checked, `name.yaml` named where one is
/// wrong.
pub(crate) fn compile(name: &str, file: SessionsFile) -> Result<Sessions> {
    let hooks = file.hooks.map(|h| compile_hooks(name, h)).transpose()?;
    let list = match file.list {
        Some(l) if !READERS.contains(&l.reader.as_str()) => bail!(
            "{name}.yaml: sessions.list.reader `{}` is not a reader Relay has ({})",
            l.reader,
            READERS.join(", ")
        ),
        Some(l) => Some(List {
            args: l.args,
            reader: l.reader,
        }),
        None => None,
    };
    let attach = file.attach.unwrap_or_default();
    if !attach.is_empty() && !attach.iter().any(|a| a.contains(ID)) {
        bail!("{name}.yaml: sessions.attach says nowhere where the id goes ({ID})");
    }
    let start = file.start.unwrap_or_default();
    let named = file.name.unwrap_or_default();
    if !named.is_empty() && !named.iter().any(|a| a.contains(NAME)) {
        bail!("{name}.yaml: sessions.name says nowhere where the name goes ({NAME})");
    }
    let stop = file.stop.unwrap_or_default();
    if !stop.is_empty() && !stop.iter().any(|a| a.contains(ID)) {
        bail!("{name}.yaml: sessions.stop says nowhere where the id goes ({ID})");
    }
    let ping = file.ping.unwrap_or_default();
    for (word, what) in [(ID, "id"), (TEXT, "text")] {
        if !ping.is_empty() && !ping.iter().any(|a| a.contains(word)) {
            bail!("{name}.yaml: sessions.ping says nowhere where the {what} goes ({word})");
        }
    }
    Ok(Sessions {
        hooks,
        list,
        attach,
        start,
        name: named,
        stop,
        ping,
    })
}

/// Whether `word` may stand in a hook's command as it is: an agent's or an
/// event's name, written there unquoted and read back word by word
/// (`user_hooks::is_relays`). No space, no quote, nothing a shell reads.
fn plain(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn compile_hooks(name: &str, h: HooksFile) -> Result<UserHooks> {
    let at = |what: &str| format!("{name}.yaml: sessions.hooks.{what}");
    if !plain(name) {
        bail!("{name}.yaml: an agent with hooks is named only with letters, digits, `_`, `.` and `-`");
    }
    let file = h.file.filter(|f| !f.is_empty());
    let Some(file) = file else {
        bail!("{} is missing: where the hooks file is", at("file"))
    };
    let unit = match h.unit.as_deref().unwrap_or("s") {
        "s" => Unit::Seconds,
        "ms" => Unit::Millis,
        other => bail!("{} `{other}` is not s or ms", at("unit")),
    };
    let Some(timeout) = h.timeout else {
        bail!("{} is missing: how long the agent waits for a hook", at("timeout"))
    };
    let Some(d) = parse_duration(&timeout).filter(|d| d.is_positive()) else {
        bail!("{} `{timeout}` is not a duration (500ms, 3s)", at("timeout"))
    };
    let ms = d.as_millis().max(0) as u64;
    let timeout = match unit {
        Unit::Seconds if !ms.is_multiple_of(1000) => bail!("{} `{timeout}` is not whole seconds", at("timeout")),
        Unit::Seconds => ms / 1000,
        Unit::Millis => ms,
    };
    if let Some(m) = &h.marker
        && m != MARKER
    {
        bail!("{} `{m}` is not one `rl hook` knows ({MARKER})", at("marker"));
    }
    let events = h.events.unwrap_or_default();
    if events.is_empty() {
        bail!("{} is missing: the events hooked", at("events"));
    }
    let events = events
        .into_iter()
        .map(|(event, v)| {
            let here = at(&format!("events.{event}"));
            if !plain(&event) {
                bail!("{here}: an event is named only with letters, digits, `_`, `.` and `-`");
            }
            let said = compile_event(&v).map_err(|e| anyhow::anyhow!("{here}: {e}"))?;
            Ok((event, said))
        })
        .collect::<Result<Vec<_>>>()?;
    let prompts: Vec<&str> = events
        .iter()
        .filter(|(_, said)| matches!(said, Event::Says(s) if s.prompt))
        .map(|(e, _)| e.as_str())
        .collect();
    if prompts.len() > 1 {
        bail!(
            "{}: only one event carries the prompt typed, not {}",
            at("events"),
            prompts.join(" and ")
        );
    }
    let settings = h
        .settings
        .unwrap_or_default()
        .into_iter()
        .map(|(key, v)| {
            if key.split('.').any(str::is_empty) {
                bail!(
                    "{} `{key}` is not a dotted path (ui.useAlternateBuffer)",
                    at("settings")
                );
            }
            if key.split('.').next() == Some("hooks") {
                bail!("{} `{key}`: `hooks` is the hooks', not a setting", at("settings"));
            }
            Ok((key, v))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(UserHooks {
        agent: name.to_string(),
        file,
        timeout,
        unit,
        background: h.background.unwrap_or(false),
        marker: h.marker,
        after_install: h.after_install,
        from_env: h.from_env.unwrap_or_default(),
        events,
        settings,
    })
}

fn compile_event(v: &Value) -> Result<Event> {
    let Some(map) = v.as_object().filter(|m| m.contains_key("by")) else {
        return compile_says(v).map(Event::Says);
    };
    let Some(field) = map["by"].as_str() else {
        bail!("`by` is not a field's name")
    };
    let cases = map
        .iter()
        .filter(|(k, _)| *k != "by")
        .map(|(k, v)| {
            let said = compile_says(v).map_err(|e| anyhow::anyhow!("{k}: {e}"))?;
            if said.prompt {
                bail!("{k}: `prompt` is an event's, not a case's");
            }
            Ok((k.clone(), said))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Event::By {
        field: field.to_string(),
        cases,
    })
}

fn compile_says(v: &Value) -> Result<Says> {
    let state = |word: &str| State::parse(word).ok_or_else(|| anyhow::anyhow!("`{word}` is not a state ({STATES})"));
    match v {
        Value::String(word) => Ok(Says {
            state: state(word)?,
            prompt: false,
            waiting_for: None,
            detail: None,
        }),
        Value::Object(m) => {
            let text = |k: &str| -> Result<Option<String>> {
                match m.get(k) {
                    None => Ok(None),
                    Some(Value::String(s)) => Ok(Some(s.clone())),
                    Some(other) => bail!("`{k}` is not words: {other}"),
                }
            };
            if let Some(k) = m
                .keys()
                .find(|k| !["state", "prompt", "waiting_for", "detail"].contains(&k.as_str()))
            {
                bail!("`{k}` is not something an event says");
            }
            let Some(word) = text("state")? else {
                bail!("no `state` ({STATES})")
            };
            let prompt = match m.get("prompt") {
                None => false,
                Some(Value::Bool(b)) => *b,
                Some(other) => bail!("`prompt` is not true or false: {other}"),
            };
            Ok(Says {
                state: state(&word)?,
                prompt,
                waiting_for: text("waiting_for")?,
                detail: text("detail")?,
            })
        }
        other => bail!("{other} is not a state ({STATES}) nor a map"),
    }
}

impl UserHooks {
    /// The events hooked, in order.
    pub fn event_names(&self) -> impl Iterator<Item = &str> {
        self.events.iter().map(|(e, _)| e.as_str())
    }

    fn event(&self, event: &str) -> Option<&Event> {
        self.events.iter().find(|(e, _)| e == event).map(|(_, said)| said)
    }

    /// What `event` says, narrowed by `value`, its payload's field where
    /// the event is narrowed by one.
    fn says(&self, event: &str, value: impl FnOnce(&str) -> Option<String>) -> Option<&Says> {
        match self.event(event)? {
            Event::Says(s) => Some(s),
            Event::By { field, cases } => {
                let v = value(field)?;
                cases.iter().find(|(c, _)| *c == v).map(|(_, s)| s)
            }
        }
    }

    /// The state an event says before narrowing: any of its cases' where
    /// it is narrowed.
    fn states(&self, event: &str) -> impl Iterator<Item = State> + '_ {
        let said: Vec<&Says> = match self.event(event) {
            Some(Event::Says(s)) => vec![s],
            Some(Event::By { cases, .. }) => cases.iter().map(|(_, s)| s).collect(),
            None => vec![],
        };
        said.into_iter().map(|s| s.state)
    }

    /// Whether `event` begins a session.
    pub fn starts(&self, event: &str) -> bool {
        self.states(event).any(|s| s == State::Started)
    }

    /// Whether `event` ends the session.
    pub fn ends(&self, event: &str) -> bool {
        self.states(event).any(|s| s == State::Ended)
    }

    /// Whether `event` only carries a turn on: a tool done. Codex runs its
    /// hooks in the background, so one can arrive after the turn's end; it
    /// may end a wait for you (needing you → working) or keep a turn
    /// working, and never wakes an idle one.
    pub fn carries_on(&self, event: &str) -> bool {
        self.states(event).any(|s| s == State::CarriesOn)
    }

    /// The event that carries the prompt you typed (`prompt` in its payload).
    pub fn prompt_event(&self) -> Option<&str> {
        self.events
            .iter()
            .find(|(_, said)| matches!(said, Event::Says(s) if s.prompt))
            .map(|(e, _)| e.as_str())
    }

    /// Whether `event` is worth finding the agent's process for: a start,
    /// or a prompt (the first word `rl hook` may hear of a session it did
    /// not see start).
    pub fn asks_pid(&self, event: &str) -> bool {
        self.starts(event) || self.prompt_event() == Some(event)
    }

    /// The word from a payload that `rl hook` keeps with its event: the field
    /// it is narrowed by, else its `detail`.
    pub fn detail(&self, event: &str, payload: &Value) -> Option<String> {
        let field = match self.event(event)? {
            Event::By { field, .. } => field,
            Event::Says(s) => s.detail.as_ref()?,
        };
        field_of(payload, field)
    }

    /// What `event` (narrowed by `detail`, as `detail` gives it) says about
    /// one of the agent's own sessions.
    pub fn told(&self, event: &str, detail: Option<&str>) -> Told<'_> {
        let Some(s) = self.says(event, |_| detail.map(str::to_string)) else {
            return Told::Nothing;
        };
        let stands = |status| Told::Stands {
            status,
            waiting_for: None,
        };
        match s.state {
            State::Started | State::Idle => stands(Status::Idle),
            State::Working | State::CarriesOn => stands(Status::Busy),
            State::Needs => Told::Stands {
                status: Status::Waiting,
                waiting_for: s.waiting_for.as_deref(),
            },
            State::Ended => Told::Ended,
            State::Nothing => Told::Nothing,
        }
    }

    /// The agent's hooks file, as the agent finds it from this environment
    /// (`env` reads a variable): the first of `file` whose variables are
    /// all set, and not empty. None where none is.
    pub fn file(&self, env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
        self.file.iter().find_map(|f| expand(f, &env))
    }
}

/// `path` with `~` and each `$NAME` in it as `env` has them; none where one
/// is not set, or is empty.
pub(crate) fn expand(path: &str, env: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let var = |k: &str| {
        env(k)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string_lossy().into_owned())
    };
    let mut out = String::new();
    let mut rest = path;
    if let Some(after) = rest.strip_prefix('~')
        && (after.is_empty() || after.starts_with('/'))
    {
        out.push_str(&var("HOME")?);
        rest = after;
    }
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let name_len = rest[i + 1..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len() - i - 1);
        out.push_str(&var(&rest[i + 1..i + 1 + name_len])?);
        rest = &rest[i + 1 + name_len..];
    }
    out.push_str(rest);
    Some(PathBuf::from(out))
}

/// A payload field's first line, trimmed and clipped; none where it is not
/// there, or says nothing: an empty or blank one is none too.
fn field_of(payload: &Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(|v| clip(v.trim()))
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Registry;
    use serde_json::json;
    use std::path::Path;

    fn hooks(agent: &str) -> UserHooks {
        Registry::builtin().user_hooks(agent).unwrap().clone()
    }

    #[test]
    fn the_files_are_where_each_agent_looks() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
        };
        let (claude, codex, gemini) = (hooks("claude"), hooks("codex"), hooks("gemini"));
        let home = env(&[("HOME", "/h")]);
        assert_eq!(codex.file(home).unwrap(), Path::new("/h/.codex/hooks.json"));
        assert_eq!(gemini.file(home).unwrap(), Path::new("/h/.gemini/settings.json"));
        let moved = env(&[("HOME", "/h"), ("CODEX_HOME", "/c"), ("GEMINI_CLI_HOME", "/g")]);
        assert_eq!(codex.file(moved).unwrap(), Path::new("/c/hooks.json"));
        assert_eq!(gemini.file(moved).unwrap(), Path::new("/g/.gemini/settings.json"));
        assert_eq!(codex.file(env(&[("CODEX_HOME", "")])), None, "empty is unset");
        assert_eq!(claude.file(home).unwrap(), Path::new("/h/.claude/settings.json"));
        let config = env(&[("HOME", "/h"), ("CLAUDE_CONFIG_DIR", "/cc")]);
        assert_eq!(claude.file(config).unwrap(), Path::new("/cc/settings.json"));
        assert!(Registry::builtin().user_hooks("cursor").is_none());
        let whole = |f: &str| expand(f, &env(&[("A_1", "/a"), ("HOME", "/h")]));
        assert_eq!(whole("$A_1/x~/y.json").unwrap(), Path::new("/a/x~/y.json"));
        assert_eq!(whole("~").unwrap(), Path::new("/h"));
        assert_eq!(whole("~x/y"), Some(PathBuf::from("~x/y")), "only `~` alone is home");
        assert_eq!(whole("/$B/x"), None);
    }

    #[test]
    fn events_say_how_a_session_stands() {
        let stands = |status, waiting_for| Told::Stands { status, waiting_for };
        let (codex, gemini) = (hooks("codex"), hooks("gemini"));
        assert_eq!(
            codex.told("PermissionRequest", Some("Bash")),
            stands(Status::Waiting, Some("permission prompt"))
        );
        assert_eq!(
            gemini.told("Notification", Some("ToolPermission")),
            stands(Status::Waiting, Some("permission prompt"))
        );
        assert_eq!(gemini.told("Notification", Some("Other")), Told::Nothing);
        assert_eq!(gemini.told("Notification", None), Told::Nothing);
        assert_eq!(codex.told("UserPromptSubmit", None), stands(Status::Busy, None));
        assert_eq!(gemini.told("BeforeAgent", None), stands(Status::Busy, None));
        assert_eq!(codex.told("Stop", None), stands(Status::Idle, None));
        assert_eq!(gemini.told("AfterAgent", None), stands(Status::Idle, None));
        assert_eq!(gemini.told("SessionStart", Some("startup")), stands(Status::Idle, None));
        assert_eq!(codex.told("SessionEnd", None), Told::Ended);
        assert_eq!(
            codex.told("BeforeAgent", None),
            Told::Nothing,
            "gemini's word, not codex's"
        );
        assert_eq!(
            gemini
                .detail("Notification", &json!({"notification_type": "ToolPermission"}))
                .as_deref(),
            Some("ToolPermission")
        );
        assert_eq!(
            codex
                .detail("PermissionRequest", &json!({"tool_name": "shell"}))
                .as_deref(),
            Some("shell")
        );
        assert_eq!(codex.detail("Stop", &json!({"last_assistant_message": "long"})), None);
        assert!(codex.starts("SessionStart") && gemini.starts("SessionStart"));
        assert!(!codex.starts("Stop"));
    }

    #[test]
    fn claudes_events_say_how_its_session_stands() {
        let stands = |status, waiting_for| Told::Stands { status, waiting_for };
        let claude = hooks("claude");
        assert_eq!(claude.told("SessionStart", Some("startup")), stands(Status::Idle, None));
        assert_eq!(claude.told("UserPromptSubmit", None), stands(Status::Busy, None));
        assert_eq!(
            claude.told("Notification", Some("permission_prompt")),
            stands(Status::Waiting, Some("permission prompt"))
        );
        assert_eq!(
            claude.told("Notification", Some("idle_prompt")),
            stands(Status::Idle, None)
        );
        assert_eq!(claude.told("Notification", Some("auth_success")), Told::Nothing);
        assert_eq!(claude.told("PostToolUse", None), stands(Status::Busy, None));
        assert!(claude.carries_on("PostToolUse"));
        assert_eq!(claude.told("Stop", None), stands(Status::Idle, None));
        assert_eq!(claude.told("SessionEnd", Some("exit")), Told::Ended);
        assert_eq!(claude.told("Interrupt", None), Told::Nothing, "codex's word");
        assert_eq!(
            claude
                .detail("Notification", &json!({"notification_type": "permission_prompt"}))
                .as_deref(),
            Some("permission_prompt")
        );
        assert_eq!(claude.prompt_event(), Some("UserPromptSubmit"));
    }

    #[test]
    fn a_tool_that_ran_ends_the_wait_and_an_interrupt_ends_the_turn() {
        // Approve, then the tool runs: working again.
        for (agent, ran) in [("codex", "PostToolUse"), ("gemini", "AfterTool")] {
            let h = hooks(agent);
            assert!(h.event_names().any(|e| e == ran), "{agent} hooks {ran}");
            assert_eq!(
                h.told(ran, None),
                Told::Stands {
                    status: Status::Busy,
                    waiting_for: None
                }
            );
            assert!(h.carries_on(ran));
        }
        let codex = hooks("codex");
        assert!(!codex.carries_on("AfterTool") && !codex.carries_on("UserPromptSubmit"));
        assert_eq!(
            codex.told("Interrupt", None),
            Told::Stands {
                status: Status::Idle,
                waiting_for: None
            }
        );
    }

    #[test]
    fn only_a_start_or_a_prompt_asks_ps() {
        for (agent, e) in [
            ("codex", "SessionStart"),
            ("codex", "UserPromptSubmit"),
            ("gemini", "BeforeAgent"),
            ("claude", "UserPromptSubmit"),
        ] {
            assert!(hooks(agent).asks_pid(e), "{agent} {e}");
        }
        for (agent, e) in [
            ("gemini", "AfterTool"),
            ("codex", "PostToolUse"),
            ("claude", "Notification"),
            ("codex", "PermissionRequest"),
            ("codex", "Stop"),
            ("gemini", "AfterAgent"),
            ("codex", "SessionEnd"),
        ] {
            assert!(!hooks(agent).asks_pid(e), "{agent} {e}");
        }
    }

    #[test]
    fn each_agent_hooks_its_own_events_and_says_what_it_asks_after() {
        let names = |a: &str| hooks(a).event_names().map(String::from).collect::<Vec<_>>();
        assert_eq!(
            names("claude"),
            [
                "SessionStart",
                "UserPromptSubmit",
                "Notification",
                "PostToolUse",
                "Stop",
                "SessionEnd"
            ]
        );
        assert_eq!(
            names("codex"),
            [
                "SessionStart",
                "UserPromptSubmit",
                "PermissionRequest",
                "PostToolUse",
                "Interrupt",
                "Stop",
                "SessionEnd"
            ]
        );
        assert_eq!(
            names("gemini"),
            [
                "SessionStart",
                "BeforeAgent",
                "Notification",
                "AfterTool",
                "AfterAgent",
                "SessionEnd"
            ]
        );
        assert_eq!(
            hooks("codex").after_install.as_deref(),
            Some("codex asks you to trust it once: open codex, /hooks")
        );
        assert_eq!(
            hooks("claude").after_install.as_deref(),
            Some(
                "Claude picks it up in sessions started from now on; ones already open may ask you to review it in /hooks"
            )
        );
        assert_eq!(hooks("gemini").after_install, None);
        assert_eq!(
            hooks("gemini").settings,
            [("ui.useAlternateBuffer".to_string(), json!(true))]
        );
        assert!(hooks("claude").settings.is_empty() && hooks("codex").settings.is_empty());
        let timing = |a: &str| {
            let h = hooks(a);
            (h.timeout, h.unit, h.background)
        };
        assert_eq!(
            timing("claude"),
            (crate::claude::HOOK_TIMEOUT_SECS as u64, Unit::Seconds, false)
        );
        assert_eq!(timing("codex"), (3, Unit::Seconds, true));
        assert_eq!(timing("gemini"), (2000, Unit::Millis, false));
        assert_eq!(hooks("claude").marker.as_deref(), Some(MARKER));
        assert_eq!(hooks("codex").marker, None);
        assert_eq!(
            hooks("gemini").from_env,
            FromEnv {
                session_id: Some("GEMINI_SESSION_ID".into()),
                cwd: Some("GEMINI_CWD".into())
            }
        );
        let r = Registry::builtin();
        let claude = r.profile("claude").unwrap().sessions.as_ref().unwrap();
        assert_eq!(
            claude.list,
            Some(List {
                args: vec!["agents".into(), "--json".into(), "--all".into()],
                reader: "claude-agents".into()
            })
        );
        assert_eq!(claude.attach, ["attach", ID]);
        assert_eq!(claude.start, ["--bg"]);
        assert_eq!(claude.name, ["-n", NAME]);
        assert_eq!(claude.stop, ["stop", ID]);
        let codex = r.profile("codex").unwrap().sessions.as_ref().unwrap();
        assert_eq!(
            codex.list,
            Some(List {
                args: vec!["app-server".into(), "proxy".into()],
                reader: "codex-threads".into()
            })
        );
        assert_eq!(codex.attach, ["resume", ID], "attaching to a thread is resuming it");
        assert!(codex.start.is_empty(), "no background start: Relay holds it");
        assert!(codex.stop.is_empty(), "nothing stops a thread from outside it");
        assert_eq!(
            codex.ping,
            ["queue", "--thread", ID, "--message", TEXT, "--resume"],
            "mail is queued on the thread"
        );
        assert!(claude.ping.is_empty(), "nothing tells a Claude session from outside");
        assert!(r.profile("gemini").unwrap().sessions.as_ref().unwrap().list.is_none());
        assert!(r.profile("cursor").unwrap().sessions.is_none());
    }

    #[test]
    fn a_bad_sessions_block_does_not_load_and_names_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let ok =
            "sessions:\n  hooks:\n    file: [\"~/.x/hooks.json\"]\n    timeout: 3s\n    events:\n      Stop: idle\n";
        std::fs::write(dir.path().join("xagent.yaml"), ok).unwrap();
        crate::Registry::load_from(dir.path()).unwrap();
        for (bad, says) in [
            (ok.replace("Stop: idle", "Stop: snoozing"), "snoozing"),
            (
                ok.replace("Stop: idle", "Stop: { state: needs, colour: red }"),
                "colour",
            ),
            (ok.replace("Stop: idle", "Note: { by: kind, a: asleep }"), "asleep"),
            (ok.replace("timeout: 3s", "timeout: 3s\n    unit: min"), "min"),
            (ok.replace("timeout: 3s", "timeout: 1500ms"), "whole seconds"),
            (ok.replace("timeout: 3s", "timeout: soon"), "soon"),
            (ok.replace("timeout: 3s", "timeout: 3s\n    marker: --mine"), "--mine"),
            (
                format!("{ok}  list: {{ args: [ls], reader: tea-leaves }}\n"),
                "tea-leaves",
            ),
            (format!("{ok}  attach: [attach]\n"), "{id}"),
            (format!("{ok}  stop: [stop]\n"), "sessions.stop"),
            (format!("{ok}  ping: [queue, \"{{id}}\"]\n"), "{text}"),
            (format!("{ok}  ping: [queue, \"{{text}}\"]\n"), "sessions.ping"),
            (format!("{ok}    settings: {{ \"ui..x\": true }}\n"), "ui..x"),
            (format!("{ok}    settings: {{ hooks.Stop: [] }}\n"), "hooks.Stop"),
            (ok.replace("    events:\n      Stop: idle\n", ""), "events"),
            (ok.replace("Stop: idle", "Pre Tool: idle"), "Pre Tool"),
            (ok.replace("Stop: idle", "\"Stop;rm\": idle"), "Stop;rm"),
            (
                ok.replace(
                    "Stop: idle",
                    "Stop: { state: working, prompt: true }\n      Go: { state: working, prompt: true }",
                ),
                "Stop and Go",
            ),
            (
                ok.replace("Stop: idle", "Note: { by: kind, a: { state: working, prompt: true } }"),
                "a case's",
            ),
        ] {
            std::fs::write(dir.path().join("xagent.yaml"), &bad).unwrap();
            let err = format!("{:#}", crate::Registry::load_from(dir.path()).unwrap_err());
            assert!(err.contains("xagent.yaml") && err.contains(says), "{bad}: {err}");
        }
        // The agent's own name goes in the command as well.
        std::fs::remove_file(dir.path().join("xagent.yaml")).unwrap();
        std::fs::write(dir.path().join("x agent.yaml"), ok).unwrap();
        let err = format!("{:#}", crate::Registry::load_from(dir.path()).unwrap_err());
        assert!(err.contains("x agent.yaml") && err.contains("named only"), "{err}");
    }

    #[test]
    fn a_users_file_moves_one_fact_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("codex.yaml"),
            "sessions:\n  hooks:\n    file: [/elsewhere/hooks.json]\n",
        )
        .unwrap();
        let r = Registry::load_from(dir.path()).unwrap();
        let h = r.user_hooks("codex").unwrap();
        assert_eq!(h.file(|_| None).unwrap(), Path::new("/elsewhere/hooks.json"));
        assert_eq!(h.events, hooks("codex").events, "the events stay the built-in's");
    }
}
