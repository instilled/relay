//! Starting a session: resolve what it runs, claim its folder, spawn the
//! `rl-session` holder straight into it, and record `meta.json`.

use crate::store::sessions::{self, Meta};
use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rl_core::Home;
use rl_core::state::RunKind;
use rl_detect::registry::{Profile, find_bin};
use rl_detect::{Registry, claude};
use rl_session::client::Session;
use rl_session::detect::spec::DetectSpec;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Names the holder binary's override, as the old daemon did.
pub const SESSION_BIN_ENV: &str = "RELAY_SESSION_BIN";

pub struct Start {
    /// A registry profile name; its binary leads the argv.
    pub harness: Option<String>,
    pub cwd: PathBuf,
    /// The command, or arguments for `harness`. Empty without `harness`: a shell.
    pub argv: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

/// What a session runs, worked out from the request.
struct Plan<'a> {
    harness: String,
    kind: RunKind,
    profile: &'a Profile,
    argv: Vec<String>,
}

fn plan<'a>(registry: &'a Registry, start: &Start) -> Result<Plan<'a>> {
    if let Some(name) = &start.harness {
        let Some(profile) = registry.profile(name) else {
            bail!("no harness profile named {name}");
        };
        let Some(first) = profile.bins.first() else {
            bail!("harness {name} has no binary to start");
        };
        let bin = find_bin(&profile.bins).map_or_else(|| first.clone(), |p| p.to_string_lossy().into_owned());
        let argv = std::iter::once(bin).chain(start.argv.iter().cloned()).collect();
        return Ok(Plan {
            harness: profile.name.clone(),
            kind: profile.kind,
            profile,
            argv,
        });
    }
    if start.argv.is_empty() {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".into());
        return Ok(Plan {
            harness: "sh".into(),
            kind: RunKind::Shell,
            profile: registry.profile("sh").unwrap_or_else(|| registry.generic()),
            argv: vec![shell],
        });
    }
    let argv = start.argv.clone();
    Ok(match registry.detect(&argv) {
        Some(p) => Plan {
            harness: p.name.clone(),
            kind: p.kind,
            profile: p,
            argv,
        },
        None => Plan {
            harness: Path::new(&argv[0])
                .file_name()
                .map_or_else(|| argv[0].clone(), |n| n.to_string_lossy().into_owned()),
            kind: RunKind::Command,
            profile: registry.generic(),
            argv,
        },
    })
}

/// Starts a session and returns its record. On failure nothing is left behind.
pub async fn start(home: &Home, start: Start) -> Result<Meta> {
    let home = &Home::at(std::path::absolute(home.path())?);
    let cwd = std::fs::canonicalize(&start.cwd).with_context(|| format!("cwd {}", start.cwd.display()))?;
    let registry = Registry::load(home)?;
    let mut plan = plan(&registry, &start)?;

    // Claude started by Relay reports through hooks of its own.
    if let Some(hooks) = &plan.profile.hooks
        && hooks.dialect == claude::DIALECT
    {
        let rl = std::env::current_exe().context("locating this executable for hooks")?;
        let file = claude::write_settings(home, &rl)?;
        let extra = hooks
            .args
            .iter()
            .map(|a| a.replace("{hooks_file}", &file.to_string_lossy()));
        plan.argv.splice(1..1, extra);
    }

    home.ensure_store()?;
    let folder = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (name, dir) = sessions::claim(home, &sessions::base_name(&folder, &plan.harness))?;
    let socket = home.session_socket_for(&name);
    match launch(home, &name, &dir, &socket, &cwd, &plan, &start).await {
        Ok(meta) => Ok(meta),
        Err(e) => {
            let _ = std::fs::remove_file(&socket);
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

async fn launch(
    home: &Home,
    name: &str,
    dir: &Path,
    socket: &Path,
    cwd: &Path,
    plan: &Plan<'_>,
    start: &Start,
) -> Result<Meta> {
    let detect = dir.join("detect.json");
    let spec = DetectSpec::from_parts(plan.kind, &plan.profile.patterns, plan.profile.timing);
    rl_core::atomic::write_json(&detect, &spec)?;

    let log = dir.join("log");
    let log_file = std::fs::OpenOptions::new().create(true).append(true).open(&log)?;
    let bin = rl_core::locate_bin("rl-session", SESSION_BIN_ENV);
    let status = tokio::process::Command::new(&bin)
        .arg("--id")
        .arg(name)
        .arg("--socket")
        .arg(socket)
        .arg("--exit-file")
        .arg(dir.join("exit"))
        .arg("--state-file")
        .arg(dir.join("state.json"))
        .arg("--detect")
        .arg(&detect)
        .arg("--cwd")
        .arg(cwd)
        .arg("--cols")
        .arg(start.cols.to_string())
        .arg("--rows")
        .arg(start.rows.to_string())
        .arg("--env")
        .arg(format!("RELAY_SESSION={name}"))
        .arg("--env")
        .arg(format!("{}={}", rl_core::HOME_ENV, home.path().display()))
        .arg("--")
        .args(&plan.argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file))
        .status()
        .await
        .with_context(|| format!("spawning {}", bin.display()))?;
    if !status.success() {
        let tail = std::fs::read_to_string(&log).unwrap_or_default();
        bail!("{}", tail.lines().last().unwrap_or("rl-session failed to start"));
    }

    // One look at the holder, for its pid; then let go.
    let (hello, session) = Session::attach(socket, false).await?;
    drop(session);
    let meta = Meta {
        name: name.to_string(),
        harness: plan.harness.clone(),
        kind: plan.kind,
        cwd: cwd.to_path_buf(),
        argv: plan.argv.clone(),
        started: Timestamp::now(),
        socket: socket.to_path_buf(),
        holder_pid: Some(hello.holder_pid),
        native_id: None,
    };
    sessions::write_meta(dir, &meta)?;
    Ok(meta)
}
