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
