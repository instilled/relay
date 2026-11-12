//! Drift from the list: while a harness has changed under Relay and that is
//! not yet reported or dismissed, the footer's hints lead with a notice of
//! it (`drift::notice`), read again whenever its file changes. `/report`
//! hands the terminal to `rl report`'s questions, as `/setup` does to the
//! wizard; `/report dismiss` dismisses it, posting nothing.

use super::app::App;
use crate::drift;
use std::io::Write;
use std::path::Path;

/// `rl report` on this terminal, then a key to go back to the list.
pub fn run(dir: &Path) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    drift::report(dir, false, &mut input, &mut out)?;
    write!(out, "enter to go back to the list ")?;
    out.flush()?;
    let mut line = String::new();
    std::io::BufRead::read_line(&mut input, &mut line)?;
    Ok(())
}

impl App {
    /// `/report`: the report, once the list has let go of the terminal.
    pub(super) fn report_now(&mut self) {
        if drift::load(&drift::dir(&self.home)).iter().any(drift::Event::pending) {
            self.reporting = true;
        } else {
            self.say("nothing to report: no harness has changed under Relay");
        }
    }

    /// `/report dismiss`: what there is to report, dismissed.
    pub(super) fn dismiss_drift(&mut self) {
        match drift::dismiss(&drift::dir(&self.home)) {
            Ok(0) => self.say("nothing to dismiss"),
            Ok(n) => self.say(format!("dismissed {n} · rl doctor still lists it")),
            Err(e) => self.say(format!("report: {e:#}")),
        }
    }

    /// The footer's notice of drift, read again only when its file
    /// changed.
    pub fn drift_notice(&self) -> Option<String> {
        let file = drift::dir(&self.home).join("events.json");
        let at = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
        let mut seen = self.drift.borrow_mut();
        if seen.0 != at || (at.is_none() && seen.1.is_some()) {
            *seen = (at, drift::notice(&drift::load(&drift::dir(&self.home))));
        }
        seen.1.clone()
    }
}
