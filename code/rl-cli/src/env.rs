//! The environment a run started from here gets.

use anyhow::Result;

/// The run gets this shell's environment, as `nohup` would give it: PATH,
/// HOME, the keys and settings the CLI expects. `extra` (`KEY=VALUE`)
/// overrides. `RELAY_RUN` is dropped so a run started from inside another is
/// not mistaken for it.
pub fn child_env(extra: &[String]) -> Result<Vec<(String, String)>> {
    let mut env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .filter(|(k, _)| k != "RELAY_RUN")
        .collect();
    for kv in extra {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--env expects KEY=VALUE"))?;
        env.retain(|(e, _)| e != k);
        env.push((k.to_string(), v.to_string()));
    }
    Ok(env)
}

/// `$VISUAL` or `$EDITOR` (else `vi`) on `file` at `line`, on this terminal,
/// until it is closed.
pub fn open_in_editor(file: &std::path::Path, line: usize) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "vi".into());
    let mut words = editor.split_whitespace();
    let bin = words.next().unwrap_or("vi");
    let mut cmd = std::process::Command::new(bin);
    cmd.args(words);
    let base = std::path::Path::new(bin)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned());
    match base.as_deref() {
        Some("code" | "codium" | "cursor") => {
            cmd.arg("--wait")
                .arg("--goto")
                .arg(format!("{}:{line}", file.display()));
        }
        Some("hx" | "subl" | "zed") => {
            cmd.arg(format!("{}:{line}", file.display()));
        }
        _ => {
            cmd.arg(format!("+{line}")).arg(file);
        }
    }
    let status = cmd.status().map_err(|e| anyhow::anyhow!("{editor}: {e}"))?;
    if !status.success() {
        anyhow::bail!("{editor} ended with {status}");
    }
    Ok(())
}
