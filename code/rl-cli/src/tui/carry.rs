//! Enter on one of an agent's own sessions: one in its agent's background
//! (Claude's) is opened here, the real terminal handed to what its profile's
//! `sessions.attach` says (`claude attach <id>`) the way the editor gets
//! it, and nothing is made of it in Relay; one open in another terminal is
//! carried on here as a Relay run (`import.move`), as the agent's profile
//! resumes a conversation. What is run, and what is said after, are
//! worked out here, apart from the handover, so they read without a terminal.

use super::rows::registry;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use rl_proto::methods::{ImportMoveParams, Outside};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What enter does with a session.
#[derive(Debug, PartialEq, Eq)]
pub enum Enter {
    /// Hands the terminal to this command line: `claude attach <id>`.
    Attach(Vec<OsString>),
    /// Asks `rld` to carry it on as a run.
    CarryOn,
}

/// What enter does with `s`: attaches where it has an attach id (Claude's
/// background), by `bin`, its agent's binary where that was found, with
/// `attach`, its profile's arguments for it; else carries it on. Why not,
/// where it cannot be attached to.
pub fn enter(s: &Outside, bin: Option<&Path>, attach: &[String]) -> Result<Enter, String> {
    let Some(id) = &s.id else {
        return Ok(Enter::CarryOn);
    };
    if attach.is_empty() {
        return Err(format!("Relay does not know how {} opens one of its sessions", s.agent));
    }
    let bin = bin.ok_or_else(|| format!("{} {}: `{}` is not on this machine", s.agent, attach[0], s.agent))?;
    Ok(Enter::Attach(attach_argv(bin, attach, id)))
}

/// `<bin>` and `attach`, with `id` in place of `{id}`: `<bin> attach <id>`.
pub fn attach_argv(bin: &Path, attach: &[String], id: &str) -> Vec<OsString> {
    std::iter::once(bin.as_os_str().to_owned())
        .chain(attach.iter().map(|a| a.replace(rl_detect::sessions::ID, id).into()))
        .collect()
}

/// `agent`'s arguments that open one of its sessions here, after its
/// binary (`[attach, "{id}"]`): its profile's `sessions.attach`.
pub fn attach_args(agent: &str) -> Vec<String> {
    registry()
        .profile(agent)
        .and_then(|p| p.sessions.as_ref())
        .map(|s| s.attach.clone())
        .unwrap_or_default()
}

/// How `agent` opens one of its sessions here, in words: `claude attach`.
/// None where its profile does not say.
pub fn attach_words(agent: &str) -> Option<String> {
    words(agent, &attach_args(agent), rl_detect::sessions::ID)
}

/// Runs `argv` on the terminal as it is, until it lets go; why, where it
/// would not start or ended badly. Its output is the terminal's, so what is
/// said is how it ended, named by its first two words: `claude attach: exit
/// code 1`.
///
/// It shares the panel's process group, and the terminal is cooked until it
/// sets it raw, and again once it gives it back: a ^C or ^\ then is for it,
/// not for the panel. So, as `system(3)` does, the panel ignores SIGINT and
/// SIGQUIT while it waits, and the child has them back as they come.
pub fn attach(argv: &[OsString]) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let (bin, args) = argv.split_first().ok_or("nothing to run")?;
    let program = Path::new(bin).file_name().unwrap_or(bin.as_os_str()).to_string_lossy();
    let what = match args.first() {
        Some(a) => format!("{program} {}", a.to_string_lossy()),
        None => program.into_owned(),
    };
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    // SAFETY: between fork and exec only `sigaction` runs, which is
    // async-signal-safe, and nothing is allocated.
    unsafe {
        cmd.pre_exec(|| {
            for sig in QUIET {
                set(sig, SigHandler::SigDfl).map_err(std::io::Error::from)?;
            }
            Ok(())
        });
    }
    let was: Vec<_> = QUIET
        .iter()
        .map(|&sig| (sig, set(sig, SigHandler::SigIgn).ok()))
        .collect();
    let status = cmd.status();
    for (sig, old) in was {
        if let Some(old) = old {
            // SAFETY: puts back the disposition that was there before.
            let _ = unsafe { sigaction(sig, &old) };
        }
    }
    let status = status.map_err(|e| format!("{what}: {e}"))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(format!("{what}: exit code {code}")),
        None => Err(format!("{what}: ended by a signal")),
    }
}

/// The signals a keyboard sends that the panel leaves to the child.
const QUIET: [Signal; 2] = [Signal::SIGINT, Signal::SIGQUIT];

/// Sets `sig`'s handler to `h`, plainly; what it was.
fn set(sig: Signal, h: SigHandler) -> nix::Result<SigAction> {
    let action = SigAction::new(h, SaFlags::empty(), SigSet::empty());
    // SAFETY: only ignore or default: no handler of ours runs.
    unsafe { sigaction(sig, &action) }
}

/// What `import.move` is asked, to carry `s` on: with the caller's
/// environment, on a terminal `size` (columns, rows).
pub fn move_params(s: &Outside, env: Vec<(String, String)>, size: (u16, u16)) -> ImportMoveParams {
    ImportMoveParams {
        agent: s.agent.clone(),
        session_id: s.session_id.clone(),
        env,
        cols: Some(size.0),
        rows: Some(size.1),
    }
}

/// What is said once one in another terminal carries on here.
pub fn carried_on(s: &Outside) -> String {
    format!(
        "{} carries on here; it is still open in its own terminal — leave that one",
        s.title()
    )
}

/// The same, once the cursor has moved on from it meanwhile: the run it now
/// is, by its title, without taking the cursor there.
pub fn carried_on_there(s: &Outside, run: &str) -> String {
    format!("{} carries on here · run {run}", s.title())
}

/// `agent`'s binary on this machine, as its profile names it.
pub fn bin(agent: &str) -> Option<PathBuf> {
    rl_detect::registry::find_bin(&registry().profile(agent)?.bins)
}

/// How `agent` resumes a conversation, in words: `claude --resume`, `codex
/// resume`. None where its profile does not say.
pub fn resume_words(agent: &str) -> Option<String> {
    words(agent, &registry().profile(agent)?.resume, rl_detect::registry::SESSION)
}

/// `agent` and `args` but the one that holds `slot`, as words; none where
/// there are no `args`.
fn words(agent: &str, args: &[String], slot: &str) -> Option<String> {
    if args.is_empty() {
        return None;
    }
    let args = args.iter().filter(|a| !a.contains(slot));
    Some(
        std::iter::once(agent)
            .chain(args.map(String::as_str))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::outside::session;

    #[test]
    fn a_background_one_is_attached_to_by_its_id() {
        let bg = session("a1", Some("working"), "busy", None);
        let attach = attach_args("claude");
        assert_eq!(
            enter(&bg, Some(Path::new("/opt/claude")), &attach).unwrap(),
            Enter::Attach(vec!["/opt/claude".into(), "attach".into(), "a1".into()])
        );
        assert_eq!(
            enter(&bg, None, &attach).unwrap_err(),
            "claude attach: `claude` is not on this machine"
        );
        assert_eq!(
            enter(&bg, Some(Path::new("/opt/claude")), &[]).unwrap_err(),
            "Relay does not know how claude opens one of its sessions"
        );
        let term = session("i", None, "idle", None);
        assert_eq!(enter(&term, None, &[]).unwrap(), Enter::CarryOn);
        assert_eq!(
            carried_on(&term),
            "name i carries on here; it is still open in its own terminal — leave that one"
        );
        assert_eq!(carried_on_there(&term, "name i"), "name i carries on here · run name i");
        let p = move_params(&term, vec![("A".into(), "1".into())], (100, 30));
        assert_eq!(
            (p.agent.as_str(), p.session_id.as_str(), p.cols, p.rows),
            ("claude", term.session_id.as_str(), Some(100), Some(30))
        );
        assert_eq!(p.env, [("A".to_string(), "1".to_string())]);
    }

    #[test]
    fn each_agent_resumes_in_its_own_words() {
        assert_eq!(resume_words("claude").as_deref(), Some("claude --resume"));
        assert_eq!(resume_words("codex").as_deref(), Some("codex resume"));
        assert_eq!(resume_words("gemini").as_deref(), Some("gemini --resume"));
        assert_eq!(resume_words("nobody"), None);
        assert_eq!(attach_words("claude").as_deref(), Some("claude attach"));
        assert_eq!(attach_words("codex"), None);
    }

    /// The handover sets the process's signals: its tests take turns.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A stand-in `claude` that writes down how it was called, and ends as
    /// told: the handover runs it as it would the real one.
    fn fake(dir: &Path, exit: u8) -> PathBuf {
        let bin = dir.join("claude");
        let said = dir.join("argv");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\necho \"$@\" > '{}'\nexit {exit}\n", said.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[test]
    fn the_handover_runs_claude_attach_and_says_how_it_ended() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let ok = fake(tmp.path(), 0);
        attach(&attach_argv(&ok, &attach_args("claude"), "a1")).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path().join("argv")).unwrap(), "attach a1\n");
        let bad = tempfile::tempdir().unwrap();
        let failing = fake(bad.path(), 3);
        assert_eq!(
            attach(&attach_argv(&failing, &attach_args("claude"), "a1")).unwrap_err(),
            "claude attach: exit code 3"
        );
        let gone = attach(&attach_argv(
            &tmp.path().join("gone/claude"),
            &attach_args("claude"),
            "a1",
        ))
        .unwrap_err();
        assert!(gone.starts_with("claude attach: "), "{gone}");
    }

    /// The panel ignores ^C while the child has the terminal; the child does
    /// not: a shell started with SIGINT ignored could not take it back, so
    /// one that dies of its own ^C had it as it comes. The panel's own is as
    /// it was after.
    #[test]
    fn a_keyboard_signal_is_the_childs_while_it_has_the_terminal() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let handler = |sig| {
            let was = set(sig, SigHandler::SigIgn).unwrap();
            unsafe { sigaction(sig, &was) }.unwrap();
            format!("{:?}", was.handler())
        };
        let before = (handler(Signal::SIGINT), handler(Signal::SIGQUIT));
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("claude");
        std::fs::write(&bin, "#!/bin/sh\nkill -INT $$\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            attach(&attach_argv(&bin, &attach_args("claude"), "a1")).unwrap_err(),
            "claude attach: ended by a signal"
        );
        assert_eq!((handler(Signal::SIGINT), handler(Signal::SIGQUIT)), before);
    }
}
