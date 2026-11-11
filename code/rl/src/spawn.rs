//! Starting a session: resolve what it runs, claim its folder, spawn the
//! `rl-session` holder straight into it, and record `meta.json`.

use crate::store::sessions::{self, Meta};
use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use rl_core::Home;
use rl_core::state::RunKind;
use rl_detect::Registry;
use rl_detect::registry::{Profile, find_bin};
use rl_session::client::Session;
use rl_session::detect::spec::DetectSpec;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Names the holder binary's override.
pub const SESSION_BIN_ENV: &str = "RELAY_SESSION_BIN";

/// The harness a session runs, in its holder's environment: so a hook can
/// tell the session's own agent from one run inside it (`rl hook`).
pub use rl_core::AGENT_ENV;

pub struct Start {
    /// A registry profile name; its binary leads the argv.
    pub harness: Option<String>,
    pub cwd: PathBuf,
    /// The command, or arguments for `harness`. Empty without `harness`: a shell.
    pub argv: Vec<String>,
    /// `argv` is a shell's, whatever it runs first (the list's command line:
    /// `$SHELL -ic '<line>…'`): the session reads as a shell, harness `sh`.
    pub shell: bool,
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
    if start.shell && !start.argv.is_empty() {
        return Ok(Plan {
            harness: "sh".into(),
            kind: RunKind::Shell,
            profile: registry.profile("sh").unwrap_or_else(|| registry.generic()),
            argv: start.argv.clone(),
        });
    }
    if start.argv.is_empty() {
        let shell = login_shell();
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

/// `$SHELL`, else `/bin/sh`.
pub fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into())
}

/// What runs `line` in a shell that stays once it is done:
/// `$SHELL -ic '<line>` then `exec $SHELL'`, on a line of its own so a
/// comment or a trailing `&` in `line` leaves the `exec` be.
pub fn shell_line_argv(line: &str) -> Vec<String> {
    let shell = login_shell();
    let quoted = format!("'{}'", shell.replace('\'', r"'\''"));
    vec![shell, "-ic".into(), format!("{line}\nexec {quoted}")]
}

/// Starts a session and returns its record. On failure nothing is left behind.
pub async fn start(home: &Home, start: Start) -> Result<Meta> {
    let home = &Home::at(std::path::absolute(home.path())?);
    let cwd = std::fs::canonicalize(&start.cwd).with_context(|| format!("cwd {}", start.cwd.display()))?;
    let registry = Registry::load(home)?;
    let mut plan = plan(&registry, &start)?;

    // A harness that reports through hooks of its own when held (Claude)
    // is passed them.
    let rl = || std::env::current_exe().context("locating this executable for hooks");
    plan.argv = rl_detect::harness::of(plan.profile, None).held_argv(home, std::mem::take(&mut plan.argv), &rl)?;

    home.ensure_store()?;
    let folder = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (name, dir) = sessions::claim(home, &sessions::base_name(&folder, &plan.harness))?;
    let socket = home.session_socket_for(&name);
    // Written before the holder starts, so a session mid-spawn is a Lost one
    // with a start time (`sweep` leaves it a minute), never an unknown folder.
    let meta = Meta {
        name: name.clone(),
        harness: plan.harness.clone(),
        kind: plan.kind,
        cwd: cwd.clone(),
        argv: plan.argv.clone(),
        started: Timestamp::now(),
        socket: socket.clone(),
        holder_pid: None,
        native_id: None,
    };
    let mut holder_pid = None;
    let launched = match sessions::write_meta(&dir, &meta) {
        Ok(()) => launch(home, meta, &dir, &plan, &start, &mut holder_pid).await,
        Err(e) => Err(e.into()),
    };
    match launched {
        Ok(meta) => Ok(meta),
        Err(e) => {
            stop_holder(&socket, holder_pid).await;
            let _ = std::fs::remove_file(&socket);
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// A holder that may have started when its session did not: asked to close if
/// its socket answers, else sent SIGTERM if its pid is known.
async fn stop_holder(socket: &Path, pid: Option<u32>) {
    if let Ok((_, mut session)) = Session::attach(socket, false).await
        && session.close().await.is_ok()
    {
        return;
    }
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGTERM);
    }
}

/// How long `rl-session` may take to start its holder and say so.
const START_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// The exit of `child`, which must come within `wait`; else it is killed.
async fn wait_started(
    child: &mut tokio::process::Child,
    wait: std::time::Duration,
) -> Result<std::process::ExitStatus> {
    match tokio::time::timeout(wait, child.wait()).await {
        Ok(status) => Ok(status?),
        Err(_) => {
            let _ = child.kill().await;
            bail!("did not start in {}s", wait.as_secs_f32());
        }
    }
}

async fn launch(
    home: &Home,
    mut meta: Meta,
    dir: &Path,
    plan: &Plan<'_>,
    start: &Start,
    holder_pid: &mut Option<u32>,
) -> Result<Meta> {
    let (name, socket, cwd) = (meta.name.clone(), meta.socket.clone(), meta.cwd.clone());
    let detect = dir.join("detect.json");
    let spec = DetectSpec::from_parts(plan.kind, &plan.profile.patterns, plan.profile.timing);
    rl_core::atomic::write_json(&detect, &spec)?;

    let log = dir.join("log");
    let log_file = std::fs::OpenOptions::new().create(true).append(true).open(&log)?;
    let bin = rl_core::locate_bin("rl-session", SESSION_BIN_ENV);
    let mut child = tokio::process::Command::new(&bin)
        .arg("--id")
        .arg(&name)
        .arg("--socket")
        .arg(&socket)
        .arg("--exit-file")
        .arg(dir.join("exit"))
        .arg("--state-file")
        .arg(dir.join("state.json"))
        .arg("--detect")
        .arg(&detect)
        .arg("--cwd")
        .arg(&cwd)
        .arg("--cols")
        .arg(start.cols.to_string())
        .arg("--rows")
        .arg(start.rows.to_string())
        .arg("--env")
        .arg(format!("RL_SESSION={name}"))
        .arg("--env")
        .arg(format!("{AGENT_ENV}={}", plan.harness))
        .arg("--env")
        .arg(format!("{}={}", rl_core::HOME_ENV, home.path().display()))
        .arg("--")
        .args(&plan.argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file))
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawning {}", bin.display()))?;
    // It forks the holder and exits: a start that hangs is a failed one.
    let status = wait_started(&mut child, START_WAIT)
        .await
        .with_context(|| format!("running {}", bin.display()))?;
    if !status.success() {
        let tail = std::fs::read_to_string(&log).unwrap_or_default();
        bail!("{}", tail.lines().last().unwrap_or("rl-session failed to start"));
    }

    // One look at the holder, for its pid; then let go.
    let (hello, session) = Session::attach(&socket, false).await?;
    drop(session);
    *holder_pid = Some(hello.holder_pid);
    meta.holder_pid = *holder_pid;
    sessions::write_meta(dir, &meta)?;
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_start_that_hangs_is_killed_at_the_wait() {
        let mut child = tokio::process::Command::new("sleep").arg("60").spawn().unwrap();
        let at = std::time::Instant::now();
        let e = wait_started(&mut child, std::time::Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("did not start"), "{e}");
        assert!(at.elapsed() < std::time::Duration::from_secs(5));
        assert!(child.try_wait().unwrap().is_some(), "killed");
        let mut quick = tokio::process::Command::new("true").spawn().unwrap();
        assert!(
            wait_started(&mut quick, std::time::Duration::from_secs(60))
                .await
                .unwrap()
                .success()
        );
    }

    #[tokio::test]
    async fn a_holder_that_never_answered_is_sent_sigterm_by_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("sleep").arg("60").spawn().unwrap();
        stop_holder(&tmp.path().join("no-socket"), Some(child.id())).await;
        let status = child.wait().unwrap();
        assert!(!status.success(), "ended by the signal: {status}");
    }
}
