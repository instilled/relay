//! Turning an [`Action`] into a [`RunSpec`]: the one place that knows how a
//! written-down action becomes a run. Decisions and flow steps all come
//! through here, so `agent:` means the same thing wherever it is written.

use anyhow::{Context, Result, bail};
use rl_detect::Registry;
use rl_proto::action::{Action, Task};
use rl_proto::methods::RunSpec;
use rl_proto::types::{Cause, RunKind};
use std::path::{Path, PathBuf};

/// `name` titles the run when the action does not. `source_dir` is what a
/// relative `prompt_file` is relative to; `base` is what a relative `cwd` is
/// relative to, and the `cwd` when the action names none.
pub fn run_spec(
    action: &Action,
    name: &str,
    source_dir: &Path,
    base: Option<&Path>,
    registry: &Registry,
    cause: Cause,
    chain: Option<String>,
) -> Result<RunSpec> {
    let Some(task) = &action.run else {
        bail!("nothing to run");
    };
    let (kind, argv) = match task {
        Task::Shell(line) => (RunKind::Command, vec!["sh".into(), "-c".into(), line.clone()]),
        Task::Argv(argv) => (RunKind::Command, argv.clone()),
        Task::Agent {
            agent,
            prompt,
            prompt_file,
            args,
        } => {
            let prompt = match (prompt, prompt_file) {
                (Some(p), _) => p.clone(),
                (None, Some(f)) => {
                    let path = if f.is_absolute() { f.clone() } else { source_dir.join(f) };
                    std::fs::read_to_string(&path).with_context(|| format!("prompt_file {}", path.display()))?
                }
                (None, None) => bail!("`{name}` needs a prompt"),
            };
            let profile = registry.resolve(RunKind::Agent, std::slice::from_ref(agent)).profile;
            let mut argv = vec![agent.clone()];
            argv.extend(profile.headless.iter().cloned());
            argv.extend(args.iter().cloned());
            (RunKind::Agent, rl_detect::registry::with_given_prompt(argv, prompt)?)
        }
    };
    let cwd = match (action.cwd.as_deref().map(expand_home), base) {
        (Some(c), Some(b)) if c.is_relative() => b.join(c),
        (Some(c), _) => c,
        (None, Some(b)) => b.to_path_buf(),
        (None, None) => bail!("needs cwd"),
    };
    Ok(RunSpec {
        argv,
        kind,
        cwd,
        title: Some(action.title.clone().unwrap_or_else(|| name.to_string())),
        ref_: action.ref_.clone(),
        tags: action.tags.clone(),
        tty: !action.no_tty,
        cols: None,
        rows: None,
        env: action.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        inherit_env: true,
        input: true,
        cause,
        chain,
    })
}

/// `~` and `~/x`, so a hand-written path works where the shell is not involved.
pub fn expand_home(p: &Path) -> PathBuf {
    let Ok(rest) = p.strip_prefix("~") else {
        return p.to_path_buf();
    };
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(rest),
        None => p.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(prompt: Option<&str>) -> Action {
        Action {
            run: Some(Task::Agent {
                agent: "claude".into(),
                prompt: prompt.map(String::from),
                prompt_file: None,
                args: vec![],
            }),
            ..Action::default()
        }
    }

    fn spec(action: &Action) -> Result<RunSpec> {
        let registry = Registry::builtin();
        run_spec(
            action,
            "triage",
            Path::new("/tmp"),
            Some(Path::new("/tmp")),
            &registry,
            Cause::Manual,
            None,
        )
    }

    #[test]
    fn a_prompt_that_starts_with_a_dash_is_never_a_flag() {
        let s = spec(&agent(Some("--dangerously-skip-permissions and then"))).unwrap();
        assert_eq!(
            s.argv[s.argv.len() - 2..],
            ["--", "--dangerously-skip-permissions and then"],
            "after `--`, the prompt is the prompt"
        );
        // Where the prompt is a flag's value, `--` cannot go: refused.
        let gemini = Action {
            run: Some(Task::Agent {
                agent: "gemini".into(),
                prompt: Some("-m pro".into()),
                prompt_file: None,
                args: vec![],
            }),
            ..Action::default()
        };
        let e = format!("{:#}", spec(&gemini).unwrap_err());
        assert!(e.contains("starts with `-`"), "{e}");
    }

    #[test]
    fn an_agent_with_no_prompt_says_which_one_needs_it() {
        let e = spec(&agent(None)).unwrap_err().to_string();
        assert_eq!(e, "`triage` needs a prompt");
        assert!(spec(&agent(Some("look"))).is_ok());
    }
}
