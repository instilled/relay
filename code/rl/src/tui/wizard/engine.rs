//! Applying the changes the wizard chose: `setup::apply`, a part at a
//! time and in order (Claude's server and its setting share a file), off
//! the screen's thread so the spinner turns while `claude mcp` or `agy mcp`
//! takes its seconds. One that fails is said and the rest go on.

use super::model::Progress;
use crate::setup::{self, Host, What};
use rl_core::Home;

/// Runs `jobs`, saying how each goes; then keeps which harnesses are set
/// up (`setup::remember`), and says it is done.
pub fn work(home: &Home, host: &Host, jobs: &[(String, What)], mut say: impl FnMut(Progress)) {
    for (i, (harness, what)) in jobs.iter().enumerate() {
        say(Progress::Started(i));
        let r = setup::apply(home, harness, what, host).map_err(|e| format!("{e:#}"));
        say(Progress::Finished(i, r));
    }
    if let Err(e) = setup::remember(home, host) {
        tracing::debug!("config.json not kept: {e:#}");
    }
    say(Progress::AllDone);
}

/// `work` on a thread of its own, saying how it goes on `tx`.
pub fn spawn(home: Home, host: Host, jobs: Vec<(String, What)>, tx: std::sync::mpsc::Sender<Progress>) {
    std::thread::spawn(move || {
        work(&home, &host, &jobs, |p| {
            let _ = tx.send(p);
        })
    });
}
