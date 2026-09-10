//! Turning an [`Action`] into a [`RunSpec`]: the one place that knows how a
//! written-down action becomes a run. Schedules, decisions and (later)
//! workflow steps all come through here, so `agent:` means the same thing
//! wherever it is written.

use anyhow::{Context, Result, bail};
use relay_detect::Registry;
use relay_proto::action::Action;
use relay_proto::methods::RunSpec;
use relay_proto::types::{Cause, RunKind};
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
    flow: Option<String>,
) -> Result<RunSpec> {
    let (kind, argv) = match &action.agent {
        Some(cli) => {
            let prompt = match (&action.prompt, &action.prompt_file) {
                (Some(p), _) => p.clone(),
                (None, Some(f)) => {
                    let path = if f.is_absolute() { f.clone() } else { source_dir.join(f) };
                    std::fs::read_to_string(&path).with_context(|| format!("prompt_file {}", path.display()))?
                }
                (None, None) => bail!("an agent needs a prompt"),
            };
            let profile = registry.resolve(RunKind::Agent, std::slice::from_ref(cli)).profile;
            let mut argv = vec![cli.clone()];
            argv.extend(profile.headless.iter().cloned());
            argv.extend(action.args.iter().cloned());
            argv.push(prompt);
            (RunKind::Agent, argv)
        }
        None if action.run.is_empty() => bail!("nothing to run"),
        None => (RunKind::Command, action.run.clone()),
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
        cause,
        flow,
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
