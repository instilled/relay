//! `config.yaml` as the daemon sees it: read at start and on every
//! `config.get` / reload, so what `rl config` prints is what is on disk. A
//! file that does not parse is reported and left aside — the daemon runs on
//! defaults, and nothing that needs the config runs on a guess.

use anyhow::{Context, Result};
use rl_core::Home;
use rl_detect::Registry;
use rl_proto::config::{Assistant, Config};
use rl_proto::methods::{AgentInfo, ConfigReport, ResolvedAssistant};
use std::path::PathBuf;

/// What reading the file gave.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub file: PathBuf,
    pub present: bool,
    pub config: Config,
    /// The parse error, when the file could not be used.
    pub error: Option<String>,
}

/// What a fresh home gets: the shape, all of it commented out, so the file
/// is there to find and `rl config edit` has something to show.
pub const SEED: &str = "\
# Your Relay defaults. Absent keys mean defaults; a key Relay does not know is refused.
# `rl config` shows what this resolves to; `rl config edit` opens this file.
# `rl setup` walks through the main ones and writes them here.
#
# agents: [claude, codex] # the agent CLIs the panel offers; unset: all of them
#
# assistants:            # the agents Relay itself calls on, by job
#   interpret: claude    # what you type at a / rl interpret in plain words -> a draft for your enter
#   summary: claude      # a recap where the agent keeps none (/summary in the panel, rl summary)
#   # or, with a model and extra arguments:
#   # interpret: { agent: claude, model: claude-haiku-4-5-20251001, args: [\"--max-turns\", \"1\"] }
#
# retention: 30d         # the only thing Relay deletes: a run's terminal bytes and log, once it
#                        # ended this long ago. The run and its events stay. Unset: 30d.
#                        # `never` keeps every byte for good.
#
# runs:
#   reap_after: 6h       # a run outlives rld, but nothing journals it while no daemon is there:
#                        # with nobody attached at all this long, it stops itself. `never` keeps it.
#   settle: 5ms          # caught up with its child, a holder waits this long before reading again,
#                        # so a burst arrives as one chunk. Costs the next chunk that much; `off`
#                        # reads as fast as it can.
#
# workspaces:
#   badges: [gh-pr]      # beside a git folder's branch: the open PR, by `gh` (~/.relay/plugins/gh-pr).
#                        # Any command whose first line says something; asked when the branch
#                        # changes, then at most every 5 minutes, only while the list is shown.
#
# catch_up:              # what happened while you were away
#   after: 8h            # away longer than this, the panel opens on it; off: only /since
#   summary: auto        # auto: the top is written when you come back after `after`, by assistants.summary
#                        # ask: only on /summary
";

/// Writes the seed when there is no file yet. Never touches one that exists.
pub fn seed(home: &Home) -> Result<bool> {
    let file = home.config_file();
    if file.exists() {
        return Ok(false);
    }
    std::fs::write(&file, SEED).with_context(|| format!("writing {}", file.display()))?;
    Ok(true)
}

pub fn load(home: &Home) -> Loaded {
    let file = home.config_file();
    match read(&file) {
        Ok(Some(config)) => Loaded {
            file,
            present: true,
            config,
            error: None,
        },
        Ok(None) => Loaded {
            file,
            ..Default::default()
        },
        Err(e) => Loaded {
            file,
            present: true,
            config: Config::default(),
            error: Some(format!("{e:#}")),
        },
    }
}

fn read(file: &std::path::Path) -> Result<Option<Config>> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    if text.trim().is_empty() {
        return Ok(Some(Config::default()));
    }
    serde_norway::from_str(&text)
        .map(Some)
        .with_context(|| format!("{}", file.display()))
}

/// How long a holder waits for a Relay that may never come back; `None` is
/// forever. A value that is not a duration falls back to the default, and
/// `report` says so.
pub fn reap_after(config: &Config) -> Option<std::time::Duration> {
    let said = config.runs.reap_after();
    if rl_proto::config::is_never(said) {
        return None;
    }
    let fallback = || rl_core::time::parse_duration(rl_proto::config::DEFAULT_REAP).expect("the default is a duration");
    Some(
        rl_core::time::parse_duration(said)
            .unwrap_or_else(fallback)
            .unsigned_abs(),
    )
}

/// How long a holder lets a burst of its child's output gather before it
/// reads again; `None` reads as fast as the child writes. A value that is not
/// a duration falls back to the default, and `report` says so.
pub fn settle(config: &Config) -> Option<std::time::Duration> {
    let said = config.runs.settle();
    if rl_proto::config::is_never(said) {
        return None;
    }
    let fallback =
        || rl_core::time::parse_duration(rl_proto::config::DEFAULT_SETTLE).expect("the default is a duration");
    Some(
        rl_core::time::parse_duration(said)
            .unwrap_or_else(fallback)
            .unsigned_abs(),
    )
}

/// Every agent the registry knows, whether it is on this machine, and
/// whether the config has the panel offer it.
pub fn agents(registry: &Registry, config: &Config) -> Vec<AgentInfo> {
    registry
        .agents()
        .map(|p| AgentInfo {
            name: p.name.clone(),
            bin: rl_detect::registry::find_bin(&p.bins),
            bins: p.bins.clone(),
            headless: p.headless.clone(),
            model_flag: p.model_flag.clone(),
            small_model: p.small_model.clone(),
            offered: config.agents.is_empty() || config.agents.contains(&p.name),
        })
        .collect()
}

/// An assistant as it would be invoked: the one the config names, else the
/// first available agent. An error says why it cannot be used as written.
pub fn resolve(
    job: &str,
    named: Option<&Assistant>,
    agents: &[AgentInfo],
) -> Result<Option<ResolvedAssistant>, String> {
    let (a, source) = match named {
        Some(a) => (a.clone(), "config"),
        None => match agents.iter().find(|x| x.available()) {
            Some(x) => (
                Assistant {
                    agent: x.name.clone(),
                    model: None,
                    args: vec![],
                },
                "default",
            ),
            None => return Ok(None),
        },
    };
    let Some(info) = agents.iter().find(|x| x.name == a.agent) else {
        let known: Vec<&str> = agents.iter().map(|x| x.name.as_str()).collect();
        return Err(format!(
            "assistants.{job}: `{}` is not an agent the registry knows ({}); a detect/{}.yaml would make it one",
            a.agent,
            known.join(", "),
            a.agent
        ));
    };
    let mut argv = vec![
        info.bin
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| info.bins.first().cloned().unwrap_or_else(|| a.agent.clone())),
    ];
    argv.extend(info.headless.iter().cloned());
    if let Some(m) = &a.model {
        if info.model_flag.is_empty() {
            return Err(format!(
                "assistants.{job}: `{}` has no model_flag in its profile, so `model: {m}` cannot be passed",
                a.agent
            ));
        }
        argv.extend(info.model_flag.iter().cloned());
        argv.push(m.clone());
    }
    argv.extend(a.args.iter().cloned());
    Ok(Some(ResolvedAssistant {
        agent: a.agent,
        source: source.into(),
        argv,
        available: info.available(),
    }))
}

pub fn report(loaded: &Loaded, registry: &Registry) -> ConfigReport {
    let agents = agents(registry, &loaded.config);
    let mut errors = Vec::new();
    if let Some(e) = &loaded.error {
        errors.push(e.clone());
    }
    for a in &loaded.config.agents {
        if !agents.iter().any(|x| &x.name == a) {
            errors.push(format!(
                "agents: `{a}` is not an agent the registry knows; a detect/{a}.yaml would make it one"
            ));
        }
    }
    if let Some(r) = &loaded.config.retention
        && let Some(j) = &r.journal
        && !rl_proto::config::is_never(j)
        && rl_core::time::parse_duration(j).is_none()
    {
        errors.push(format!("retention: `{j}` is not a duration (30d, 12h) or `never`"));
    }
    if let Some(s) = &loaded.config.runs.reap_after
        && !rl_proto::config::is_never(s)
        && rl_core::time::parse_duration(s).is_none()
    {
        errors.push(format!("runs.reap_after: `{s}` is not a duration (6h, 30m) or `never`"));
    }
    if let Some(s) = &loaded.config.runs.settle
        && !rl_proto::config::is_never(s)
        && rl_core::time::parse_duration(s).is_none()
    {
        errors.push(format!("runs.settle: `{s}` is not a duration (5ms, 20ms) or `off`"));
    }
    if let Some(s) = &loaded.config.catch_up.after
        && !rl_proto::config::is_never(s)
        && rl_core::time::parse_duration(s).is_none()
    {
        errors.push(format!("catch_up.after: `{s}` is not a duration (8h, 2d) or `off`"));
    }
    if let Some(s) = &loaded.config.catch_up.summary
        && !s.eq_ignore_ascii_case("auto")
        && !s.eq_ignore_ascii_case("ask")
    {
        errors.push(format!("catch_up.summary: `{s}` is not `auto` or `ask`"));
    }
    let mut each = |job: &str| match resolve(job, loaded.config.assistants.for_job(job).flatten(), &agents) {
        Ok(r) => r,
        Err(e) => {
            errors.push(e);
            None
        }
    };
    let interpret = each("interpret");
    let summary = each("summary");
    ConfigReport {
        file: loaded.file.clone(),
        present: loaded.present,
        config: loaded.config.clone(),
        interpret,
        summary,
        catch_up_after: loaded
            .config
            .catch_up
            .after
            .clone()
            .unwrap_or_else(|| rl_proto::config::CatchUp::DEFAULT_AFTER.into()),
        catch_up_summary: if loaded.config.catch_up.auto_summary() {
            "auto"
        } else {
            "ask"
        }
        .into(),
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_runs(yaml: &str) -> Config {
        serde_json::from_str(yaml).expect("config")
    }

    #[test]
    fn settling_falls_back_to_the_default_and_off_means_no_waiting() {
        use std::time::Duration;
        assert_eq!(settle(&with_runs("{}")), Some(Duration::from_millis(5)));
        assert_eq!(
            settle(&with_runs(r#"{"runs":{"settle":"20ms"}}"#)),
            Some(Duration::from_millis(20))
        );
        assert_eq!(settle(&with_runs(r#"{"runs":{"settle":"off"}}"#)), None);
        // Not a duration: the default stands and `rl config` reports it.
        assert_eq!(
            settle(&with_runs(r#"{"runs":{"settle":"soon"}}"#)),
            Some(Duration::from_millis(5))
        );
    }

    #[test]
    fn the_seed_says_when_the_top_writes_itself_and_by_whom() {
        let line = SEED.lines().find(|l| l.starts_with("#   summary: auto")).unwrap();
        assert!(
            line.contains("when you come back after `after`, by assistants.summary"),
            "{line}"
        );
    }

    #[test]
    fn the_report_says_what_catch_up_resolves_to_and_what_is_wrong() {
        let said = |json: &str| {
            let loaded = Loaded {
                config: with_runs(json),
                ..Loaded::default()
            };
            report(&loaded, &Registry::builtin())
        };
        let r = said("{}");
        assert_eq!((r.catch_up_after.as_str(), r.catch_up_summary.as_str()), ("8h", "auto"));
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let r = said(r#"{"catch_up":{"after":"off","summary":"ask"}}"#);
        assert_eq!((r.catch_up_after.as_str(), r.catch_up_summary.as_str()), ("off", "ask"));
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let r = said(r#"{"catch_up":{"after":"soon","summary":"always"}}"#);
        assert!(
            r.errors.iter().any(|e| e.starts_with("catch_up.after: `soon`")),
            "{:?}",
            r.errors
        );
        assert!(
            r.errors.iter().any(|e| e.starts_with("catch_up.summary: `always`")),
            "{:?}",
            r.errors
        );
    }

    fn agent(name: &str, bin: Option<&str>, model_flag: &[&str]) -> AgentInfo {
        AgentInfo {
            name: name.into(),
            bins: vec![name.into()],
            bin: bin.map(PathBuf::from),
            headless: vec!["-p".into()],
            model_flag: model_flag.iter().map(|s| s.to_string()).collect(),
            small_model: None,
            offered: true,
        }
    }

    #[test]
    fn the_assistant_is_the_named_agent_or_the_first_available() {
        let agents = vec![
            agent("claude", Some("/usr/bin/claude"), &["--model"]),
            agent("codex", None, &["-m"]),
            agent("cursor", Some("/usr/bin/cursor"), &[]),
        ];
        let r = resolve("interpret", None, &agents).unwrap().unwrap();
        assert_eq!(
            (r.agent.as_str(), r.source.as_str(), r.available),
            ("claude", "default", true)
        );
        assert_eq!(r.argv, ["/usr/bin/claude", "-p"]);

        let named = Assistant {
            agent: "codex".into(),
            model: Some("o3".into()),
            args: vec!["--full-auto".into()],
        };
        let r = resolve("interpret", Some(&named), &agents).unwrap().unwrap();
        assert_eq!(
            (r.source.as_str(), r.available),
            ("config", false),
            "named but not installed: still resolved"
        );
        assert_eq!(r.argv, ["codex", "-p", "-m", "o3", "--full-auto"]);

        let e = resolve(
            "interpret",
            Some(&Assistant {
                agent: "cursor".into(),
                model: Some("x".into()),
                args: vec![],
            }),
            &agents,
        )
        .unwrap_err();
        assert!(e.contains("no model_flag"), "{e}");
        let e = resolve(
            "interpret",
            Some(&Assistant {
                agent: "gemini".into(),
                model: None,
                args: vec![],
            }),
            &agents,
        )
        .unwrap_err();
        assert!(
            e.contains("not an agent the registry knows") && e.contains("detect/gemini.yaml"),
            "{e}"
        );
        assert_eq!(
            resolve("interpret", None, &[agent("codex", None, &[])]).unwrap(),
            None,
            "nothing installed: no default"
        );
    }

    #[test]
    fn the_seed_is_all_comments_and_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path());
        assert!(seed(&home).unwrap());
        let l = load(&home);
        assert!(l.present && l.error.is_none() && l.config == Config::default(), "{l:?}");
        std::fs::write(home.config_file(), "assistants:\n  interpret: codex\n").unwrap();
        assert!(!seed(&home).unwrap());
        assert_eq!(load(&home).config.assistants.interpret.unwrap().agent, "codex");
    }

    #[test]
    fn a_file_that_does_not_parse_is_reported_not_used() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::at(tmp.path());
        let l = load(&home);
        assert!(!l.present && l.error.is_none() && l.config == Config::default());
        std::fs::write(home.config_file(), "assistants:\n  interpret: claude\n").unwrap();
        let l = load(&home);
        assert!(l.present && l.error.is_none());
        assert_eq!(l.config.assistants.interpret.as_ref().unwrap().agent, "claude");
        std::fs::write(home.config_file(), "assistant:\n  interpret: claude\n").unwrap();
        let l = load(&home);
        let e = l.error.unwrap();
        assert!(
            e.contains("config.yaml") && e.contains("unknown field `assistant`"),
            "{e}"
        );
        assert_eq!(l.config, Config::default());
    }
}
