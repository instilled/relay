//! The detection registry: one YAML profile per CLI, compiled into the
//! binary from `detect/*.yaml` and merged with `$RELAY_HOME/detect/*.yaml`
//! field by field (a user file named like a built-in overrides those fields;
//! a new name adds a CLI). `generic` is the floor every profile extends.

use crate::pty::{Patterns, Timing};
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
    patterns: Option<PatternsFile>,
    timing: Option<TimingFile>,
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
        if let Some(p) = over.patterns {
            let mine = self.patterns.get_or_insert_with(Default::default);
            if p.needs_input.is_some() {
                mine.needs_input = p.needs_input;
            }
            if p.idle.is_some() {
                mine.idle = p.idle;
            }
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
    /// scheduled runs.
    pub headless: Vec<String>,
    /// The flag that names a model (`--model`); empty when the CLI has none.
    pub model_flag: Vec<String>,
    /// Generic patterns first, then the profile's own.
    pub patterns: Patterns,
    pub timing: Timing,
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
                    patterns,
                    timing,
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
            ["claude", "codex", "cursor", "generic", "sh"]
        );
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
