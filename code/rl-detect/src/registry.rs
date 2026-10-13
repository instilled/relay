//! The detection registry: one YAML profile per CLI, compiled into the
//! binary from `detect/*.yaml` and merged with `$RELAY_HOME/detect/*.yaml`
//! field by field (a user file named like a built-in overrides those fields;
//! a new name adds a CLI). `generic` is the floor every profile extends.

use crate::pty::{Patterns, Timing};
use crate::sessions::{Sessions, SessionsFile, UserHooks};
use anyhow::{Context, Result, bail};
use regex::Regex;
use rl_core::Home;
use rl_core::time::parse_duration;
use rl_proto::types::RunKind;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

pub const GENERIC: &str = "generic";

const BUILTIN: &[(&str, &str)] = &[
    (GENERIC, include_str!("../../../detect/generic.yaml")),
    ("claude", include_str!("../../../detect/claude.yaml")),
    ("codex", include_str!("../../../detect/codex.yaml")),
    ("cursor", include_str!("../../../detect/cursor.yaml")),
    ("gemini", include_str!("../../../detect/gemini.yaml")),
    ("opencode", include_str!("../../../detect/opencode.yaml")),
    ("sh", include_str!("../../../detect/sh.yaml")),
];

/// A profile file as written. Everything optional so files can be merged.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    kind: Option<RunKind>,
    bins: Option<Vec<String>>,
    interactive_only: Option<bool>,
    hooks: Option<HooksFile>,
    headless: Option<Vec<String>>,
    model_flag: Option<Vec<String>>,
    small_model: Option<String>,
    resume: Option<Vec<String>>,
    icon: Option<String>,
    color: Option<String>,
    patterns: Option<PatternsFile>,
    timing: Option<TimingFile>,
    sessions: Option<SessionsFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct HooksFile {
    dialect: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatternsFile {
    needs_input: Option<Vec<String>>,
    idle: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimingFile {
    idle_after: Option<String>,
    stale_after: Option<String>,
}

impl ProfileFile {
    fn merge(&mut self, over: ProfileFile) {
        if over.kind.is_some() {
            self.kind = over.kind;
        }
        if over.bins.is_some() {
            self.bins = over.bins;
        }
        if over.interactive_only.is_some() {
            self.interactive_only = over.interactive_only;
        }
        if over.hooks.is_some() {
            self.hooks = over.hooks;
        }
        if over.headless.is_some() {
            self.headless = over.headless;
        }
        if over.model_flag.is_some() {
            self.model_flag = over.model_flag;
        }
        if over.small_model.is_some() {
            self.small_model = over.small_model;
        }
        if over.resume.is_some() {
            self.resume = over.resume;
        }
        if over.icon.is_some() {
            self.icon = over.icon;
        }
        if over.color.is_some() {
            self.color = over.color;
        }
        if let Some(p) = over.patterns {
            let mine = self.patterns.get_or_insert_with(Default::default);
            if p.needs_input.is_some() {
                mine.needs_input = p.needs_input;
            }
            if p.idle.is_some() {
                mine.idle = p.idle;
            }
        }
        if let Some(s) = over.sessions {
            self.sessions.get_or_insert_with(Default::default).merge(s);
        }
        if let Some(t) = over.timing {
            let mine = self.timing.get_or_insert_with(Default::default);
            if t.idle_after.is_some() {
                mine.idle_after = t.idle_after;
            }
            if t.stale_after.is_some() {
                mine.stale_after = t.stale_after;
            }
        }
    }
}

/// How a CLI reports back, and what to pass it so it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hooks {
    /// Only `claude` exists today.
    pub dialect: String,
    /// Inserted after `argv[0]`; `{hooks_file}` is replaced with the settings
    /// file `rld` writes for that dialect.
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub kind: RunKind,
    pub bins: Vec<String>,
    /// Only match when argv looks like an interactive session (no `-c`, no
    /// script). Shells.
    pub interactive_only: bool,
    pub hooks: Option<Hooks>,
    /// Arguments that make the CLI run one prompt and exit (`-p`), for
    /// headless runs: a flow's agent steps, Relay's own assistants. The prompt
    /// goes last, or in place of `{prompt}`.
    pub headless: Vec<String>,
    /// The flag that names a model (`--model`); empty when the CLI has none.
    pub model_flag: Vec<String>,
    /// The quick, cheap model setup offers for summaries, when there is one.
    pub small_model: Option<String>,
    /// The arguments that carry one of its conversations on, after its
    /// binary: `{session}` is the conversation's id (`["--resume",
    /// "{session}"]`). Empty when Relay does not know how it resumes.
    pub resume: Vec<String>,
    /// The glyph its rows wear in place of the plain agent one: one column
    /// wide under every width table, so the titles after it stay in line.
    pub icon: Option<String>,
    /// The colour that glyph is drawn in (`#rrggbb`); none, the foreground.
    pub color: Option<(u8, u8, u8)>,
    /// Generic patterns first, then the profile's own.
    pub patterns: Patterns,
    pub timing: Timing,
    /// Its own sessions, outside Relay: how its hooks tell of them, how it
    /// lists them, how one is opened. None, and it has none on the list.
    pub sessions: Option<Sessions>,
}

/// Where a headless prompt goes when it does not simply go last: a CLI whose
/// prompt is the value of a flag (`gemini -p <prompt>`).
pub const PROMPT: &str = "{prompt}";

/// A headless argv with its prompt: in place of `{prompt}`, else last.
pub fn with_prompt(mut argv: Vec<String>, prompt: String) -> Vec<String> {
    match argv.iter().position(|a| a == PROMPT) {
        Some(i) => argv[i] = prompt,
        None => argv.push(prompt),
    }
    argv
}

/// The same for a prompt Relay did not write — a flow's, with what its steps left spliced in, or
/// an option's: last, after `--`, so one that starts with `-` is never read as a flag. In place
/// of `{prompt}` it is a flag's value, where `--` cannot go, and one that starts with `-` is
/// refused.
pub fn with_given_prompt(mut argv: Vec<String>, prompt: String) -> Result<Vec<String>> {
    if !argv.iter().any(|a| a == PROMPT) {
        argv.push("--".into());
    } else if prompt.starts_with('-') {
        bail!("the prompt starts with `-`, and this agent would read it as a flag");
    }
    Ok(with_prompt(argv, prompt))
}

/// Where a conversation's id goes in a profile's `resume`.
pub const SESSION: &str = "{session}";

impl Profile {
    /// What carries `session` on: `bin`, then the profile's `resume` with
    /// the id in place of `{session}`. None when it has no `resume`.
    pub fn resume_argv(&self, bin: String, session: &str) -> Option<Vec<String>> {
        if self.resume.is_empty() {
            return None;
        }
        let args = self.resume.iter().map(|a| a.replace(SESSION, session));
        Some(std::iter::once(bin).chain(args).collect())
    }
}

/// The outcome of looking at an argv.
#[derive(Debug, Clone)]
pub struct Resolved<'a> {
    pub kind: RunKind,
    pub cli: Option<String>,
    pub profile: &'a Profile,
}

#[derive(Debug)]
pub struct Registry {
    profiles: BTreeMap<String, Profile>,
}

/// Parses the compiled-in `BUILTIN` profiles, keyed by name.
fn builtin_raw() -> BTreeMap<String, ProfileFile> {
    let mut raw = BTreeMap::new();
    for (name, text) in BUILTIN {
        let file: ProfileFile = serde_norway::from_str(text).unwrap_or_else(|e| panic!("built-in {name}.yaml: {e}"));
        raw.insert(name.to_string(), file);
    }
    raw
}

impl Registry {
    /// Compiled-in profiles only.
    pub fn builtin() -> Self {
        Self::compile(builtin_raw()).expect("built-in profiles compile")
    }

    /// Built-in profiles merged with `$RELAY_HOME/detect/*.yaml`.
    pub fn load(home: &Home) -> Result<Self> {
        Self::load_from(&home.detect_dir())
    }

    pub fn load_from(dir: &Path) -> Result<Self> {
        let mut raw = builtin_raw();
        if dir.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .with_context(|| format!("reading {}", dir.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "yaml" || x == "yml"))
                .collect();
            entries.sort();
            for path in entries {
                let name = path.file_stem().unwrap().to_string_lossy().into_owned();
                let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
                let file: ProfileFile =
                    serde_norway::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
                raw.entry(name).or_default().merge(file);
            }
        }
        Self::compile(raw)
    }

    fn compile(raw: BTreeMap<String, ProfileFile>) -> Result<Self> {
        let generic = raw.get(GENERIC).cloned().unwrap_or_default();
        let base_patterns = compile_patterns(GENERIC, &Patterns::default(), generic.patterns.as_ref())?;
        let base_timing = compile_timing(
            GENERIC,
            Timing {
                idle_after: Duration::from_secs(3),
                stale_after: Duration::from_secs(600),
            },
            generic.timing.as_ref(),
        )?;
        let mut profiles = BTreeMap::new();
        for (name, file) in raw {
            let (patterns, timing) = if name == GENERIC {
                (base_patterns.clone(), base_timing)
            } else {
                (
                    compile_patterns(&name, &base_patterns, file.patterns.as_ref())?,
                    compile_timing(&name, base_timing, file.timing.as_ref())?,
                )
            };
            profiles.insert(
                name.clone(),
                Profile {
                    kind: file.kind.unwrap_or(RunKind::Agent),
                    bins: file.bins.unwrap_or_default(),
                    interactive_only: file.interactive_only.unwrap_or(false),
                    hooks: file.hooks.map(|h| Hooks {
                        dialect: h.dialect,
                        args: h.args,
                    }),
                    headless: file.headless.unwrap_or_default(),
                    model_flag: file.model_flag.unwrap_or_default(),
                    small_model: file.small_model,
                    resume: file.resume.unwrap_or_default(),
                    icon: file.icon.map(|i| check_icon(&name, i)).transpose()?,
                    color: file.color.map(|c| parse_color(&name, &c)).transpose()?,
                    patterns,
                    timing,
                    sessions: file.sessions.map(|s| crate::sessions::compile(&name, s)).transpose()?,
                    name,
                },
            );
        }
        Ok(Self { profiles })
    }

    pub fn generic(&self) -> &Profile {
        self.profiles.get(GENERIC).expect("generic profile")
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.profiles.keys().map(String::as_str)
    }

    /// The profiles that are agents, in name order.
    pub fn agents(&self) -> impl Iterator<Item = &Profile> {
        self.profiles
            .values()
            .filter(|p| p.kind == RunKind::Agent && p.name != GENERIC)
    }

    /// The hooks of the agent `name` that tell of its own sessions, where
    /// its profile has them (`sessions.hooks`).
    pub fn user_hooks(&self, name: &str) -> Option<&UserHooks> {
        self.profiles.get(name)?.sessions.as_ref()?.hooks.as_ref()
    }

    /// Every agent's hooks that tell of its own sessions, in name order.
    pub fn all_user_hooks(&self) -> impl Iterator<Item = &UserHooks> {
        self.profiles
            .values()
            .filter_map(|p| p.sessions.as_ref()?.hooks.as_ref())
    }

    /// The profiles of the agents that list their own sessions themselves
    /// (`sessions.list`), in name order.
    pub fn listing(&self) -> impl Iterator<Item = &Profile> {
        self.profiles
            .values()
            .filter(|p| p.sessions.as_ref().is_some_and(|s| s.list.is_some()))
    }

    /// Which known CLI `argv` starts, if any.
    pub fn detect(&self, argv: &[String]) -> Option<&Profile> {
        let bin = basename(argv.first()?);
        self.profiles.values().find(|p| {
            p.name != GENERIC && p.bins.iter().any(|b| b == bin) && (!p.interactive_only || is_interactive(argv))
        })
    }

    /// Kind and cli for a run: the caller's hint (`--agent`) wins over what
    /// the registry sees; a plain command that starts a known CLI becomes
    /// that CLI's kind. Unknown argv gets the generic profile.
    pub fn resolve(&self, hint: RunKind, argv: &[String]) -> Resolved<'_> {
        match self.detect(argv) {
            Some(p) => Resolved {
                kind: if hint == RunKind::Command { p.kind } else { hint },
                cli: Some(p.name.clone()),
                profile: p,
            },
            None => Resolved {
                kind: hint,
                cli: (hint == RunKind::Agent)
                    .then(|| argv.first().map(|a| basename(a).to_string()))
                    .flatten(),
                profile: self.generic(),
            },
        }
    }
}

fn basename(arg: &str) -> &str {
    arg.rsplit('/').next().unwrap_or(arg)
}

/// A shell started with nothing but flags, and not `-c`.
fn is_interactive(argv: &[String]) -> bool {
    argv.iter().skip(1).all(|a| a.starts_with('-') && a != "-c")
}

/// One character one column wide in the CJK width table too: an ambiguous
/// one (`◆`, `◎`) is two there, and the title after it would move over.
fn check_icon(name: &str, icon: String) -> Result<String> {
    use unicode_width::UnicodeWidthChar;
    let mut chars = icon.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.width() == Some(1) && c.width_cjk() == Some(1) => Ok(icon),
        _ => bail!("{name}.yaml: icon `{icon}` is not one character one column wide (✦, ✻, ⬡)"),
    }
}

fn parse_color(name: &str, s: &str) -> Result<(u8, u8, u8)> {
    let hex = s
        .strip_prefix('#')
        .filter(|h| h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()));
    let Some(h) = hex else {
        bail!("{name}.yaml: color `{s}` is not #rrggbb")
    };
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap();
    Ok((byte(0), byte(2), byte(4)))
}

fn compile_patterns(name: &str, base: &Patterns, file: Option<&PatternsFile>) -> Result<Patterns> {
    let mut out = base.clone();
    let Some(f) = file else { return Ok(out) };
    let compile = |list: &Option<Vec<String>>, what: &str| -> Result<Vec<Regex>> {
        list.iter()
            .flatten()
            .map(|s| Regex::new(s).with_context(|| format!("{name}.yaml: {what} pattern `{s}`")))
            .collect()
    };
    out.needs_input.extend(compile(&f.needs_input, "needs_input")?);
    out.idle.extend(compile(&f.idle, "idle")?);
    Ok(out)
}

fn compile_timing(name: &str, base: Timing, file: Option<&TimingFile>) -> Result<Timing> {
    let mut out = base;
    let Some(f) = file else { return Ok(out) };
    let dur = |s: &Option<String>, what: &str| -> Result<Option<Duration>> {
        match s {
            None => Ok(None),
            Some(s) => match parse_duration(s) {
                Some(d) => Ok(Some(Duration::from_millis(d.as_millis().max(0) as u64))),
                None => bail!("{name}.yaml: timing.{what} `{s}` is not a duration (500ms, 3s, 10m)"),
            },
        }
    };
    if let Some(d) = dur(&f.idle_after, "idle_after")? {
        out.idle_after = d;
    }
    if let Some(d) = dur(&f.stale_after, "stale_after")? {
        out.stale_after = d;
    }
    Ok(out)
}

/// The first of `bins` on `PATH` (an absolute one is taken as it is).
pub fn find_bin(bins: &[String]) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for b in bins {
        let p = Path::new(b);
        if p.is_absolute() {
            if p.is_file() {
                return Some(p.to_path_buf());
            }
            continue;
        }
        for dir in std::env::split_paths(&path) {
            let cand = dir.join(b);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn builtin_profiles_detect_the_usual_clis() {
        let r = Registry::builtin();
        assert_eq!(
            r.names().collect::<Vec<_>>(),
            ["claude", "codex", "cursor", "gemini", "generic", "opencode", "sh"]
        );
        assert_eq!(r.profile("claude").unwrap().small_model.as_deref(), Some("haiku"));
        assert_eq!(r.profile("opencode").unwrap().small_model, None);
        let resume = |n: &str| r.profile(n).unwrap().resume_argv(n.into(), "s1");
        assert_eq!(resume("claude").unwrap(), ["claude", "--resume", "s1"]);
        assert_eq!(resume("codex").unwrap(), ["codex", "resume", "s1"]);
        assert_eq!(resume("gemini").unwrap(), ["gemini", "--resume", "s1"]);
        assert_eq!(resume("opencode"), None);
        let c = r.detect(&argv("/Users/me/.local/bin/claude -p hi")).unwrap();
        assert_eq!((c.name.as_str(), c.kind), ("claude", RunKind::Agent));
        assert_eq!(c.hooks.as_ref().unwrap().dialect, "claude");
        assert!(c.patterns.needs_input.len() > r.generic().patterns.needs_input.len());
        assert_eq!(r.detect(&argv("codex")).unwrap().name, "codex");
        assert!(r.detect(&argv("make e2e")).is_none());
        assert_eq!(r.generic().timing.idle_after, Duration::from_secs(3));
        assert_eq!(r.profile("sh").unwrap().timing.idle_after, Duration::from_secs(2));
    }

    #[test]
    fn agents_wear_their_own_glyph_and_colour() {
        let r = Registry::builtin();
        let look = |n: &str| {
            let p = r.profile(n).unwrap();
            (p.icon.as_deref(), p.color)
        };
        assert_eq!(look("claude"), (Some("✻"), Some((0xD9, 0x77, 0x57))));
        assert_eq!(look("codex"), (Some("⬡"), None));
        assert_eq!(look("gemini"), (Some("✧"), Some((0x4C, 0x8D, 0xF6))));
        assert_eq!(look("cursor"), (Some("❖"), None));
        assert_eq!(look("opencode"), (Some("⊡"), None));
        assert_eq!(look("generic"), (None, None));
    }

    #[test]
    fn a_user_file_recolours_a_cli_and_a_bad_look_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codex.yaml"), "color: '#10a37f'\n").unwrap();
        let r = Registry::load_from(dir.path()).unwrap();
        let c = r.profile("codex").unwrap();
        assert_eq!((c.icon.as_deref(), c.color), (Some("⬡"), Some((0x10, 0xA3, 0x7F))));
        for (bad, says) in [
            ("icon: ◆", "◆"),
            ("icon: ab", "ab"),
            ("icon: 🤖", "🤖"),
            ("color: teal", "teal"),
            ("color: '#12345'", "#12345"),
        ] {
            std::fs::write(dir.path().join("codex.yaml"), bad).unwrap();
            let err = format!("{:#}", Registry::load_from(dir.path()).unwrap_err());
            assert!(err.contains("codex.yaml") && err.contains(says), "{bad}: {err}");
        }
    }

    #[test]
    fn the_prompt_goes_where_the_profile_says() {
        let r = Registry::builtin();
        let mut argv = vec!["gemini".to_string()];
        argv.extend(r.profile("gemini").unwrap().headless.iter().cloned());
        argv.extend(["-m".into(), "flash-lite".into()]);
        assert_eq!(
            with_prompt(argv, "say hi".into()),
            ["gemini", "-p", "say hi", "-m", "flash-lite"]
        );
        assert_eq!(
            with_prompt(vec!["claude".into(), "-p".into()], "say hi".into()),
            ["claude", "-p", "say hi"]
        );
    }

    #[test]
    fn a_prompt_relay_did_not_write_goes_after_the_flags_end() {
        let claude = vec!["claude".to_string(), "-p".into()];
        assert_eq!(
            with_given_prompt(claude, "-v please".into()).unwrap(),
            ["claude", "-p", "--", "-v please"]
        );
        let gemini = vec!["gemini".to_string(), "-p".into(), PROMPT.into()];
        assert_eq!(
            with_given_prompt(gemini.clone(), "say hi".into()).unwrap(),
            ["gemini", "-p", "say hi"]
        );
        assert!(with_given_prompt(gemini, "-m pro".into()).is_err());
    }

    #[test]
    fn shells_only_when_interactive() {
        let r = Registry::builtin();
        assert_eq!(r.detect(&argv("bash")).unwrap().kind, RunKind::Shell);
        assert_eq!(r.detect(&argv("zsh -l")).unwrap().kind, RunKind::Shell);
        assert!(r.detect(&argv("sh -c echo")).is_none());
        assert!(r.detect(&argv("bash script.sh")).is_none());
    }

    #[test]
    fn resolve_respects_the_hint_and_names_the_cli() {
        let r = Registry::builtin();
        let x = r.resolve(RunKind::Command, &argv("claude"));
        assert_eq!((x.kind, x.cli.as_deref()), (RunKind::Agent, Some("claude")));
        let x = r.resolve(RunKind::Command, &argv("make test"));
        assert_eq!(
            (x.kind, x.cli.as_deref(), x.profile.name.as_str()),
            (RunKind::Command, None, "generic")
        );
        let x = r.resolve(RunKind::Agent, &argv("/opt/bin/fake-cli hi"));
        assert_eq!((x.kind, x.cli.as_deref()), (RunKind::Agent, Some("fake-cli")));
        let x = r.resolve(RunKind::Agent, &argv("sh -c x"));
        assert_eq!(x.kind, RunKind::Agent, "--agent wins");
        // A warm shell is started with a `Command` hint and `$SHELL -i` for
        // argv, so the profile's kind is what it ends up with. It matters:
        // `Heuristics` turns prompt and idle detection on for everything that
        // is not a `Command`.
        let x = r.resolve(RunKind::Command, &argv("zsh -i"));
        assert_eq!((x.kind, x.profile.name.as_str()), (RunKind::Shell, "sh"));
    }

    #[test]
    fn user_files_merge_over_builtins_and_add_clis() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("generic.yaml"), "timing:\n  idle_after: 250ms\n").unwrap();
        std::fs::write(
            dir.path().join("claude.yaml"),
            "bins: [claude, claude-nightly]\npatterns:\n  idle: ['^ready$']\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("aider.yaml"),
            "kind: agent\nbins: [aider]\npatterns:\n  needs_input: ['\\(Y\\)es/\\(N\\)o']\n",
        )
        .unwrap();
        let r = Registry::load_from(dir.path()).unwrap();
        assert_eq!(r.generic().timing.idle_after, Duration::from_millis(250));
        let c = r.profile("claude").unwrap();
        assert_eq!(
            c.timing.idle_after,
            Duration::from_millis(250),
            "inherits merged generic"
        );
        assert_eq!(c.timing.stale_after, Duration::from_secs(600));
        assert_eq!(c.bins, ["claude", "claude-nightly"]);
        assert!(c.hooks.is_some(), "untouched fields survive");
        assert!(c.patterns.idle.iter().any(|re| re.as_str() == "^ready$"));
        assert!(
            c.patterns
                .needs_input
                .iter()
                .any(|re| re.as_str().contains("do you want to")),
            "generic still applies"
        );
        assert_eq!(r.detect(&argv("aider --model x")).unwrap().kind, RunKind::Agent);

        std::fs::write(dir.path().join("broken.yaml"), "timing:\n  idle_after: soon\n").unwrap();
        let Err(err) = Registry::load_from(dir.path()) else {
            panic!("broken file accepted")
        };
        let err = format!("{err:#}");
        assert!(err.contains("broken.yaml") && err.contains("soon"), "{err}");
    }
}
