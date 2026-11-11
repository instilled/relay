//! Setup from the list: as it starts it asks `setup::check` (files only,
//! off the UI thread, never a harness's CLI), and while something needs
//! setting up the footer's hints lead with a notice of it. `/setup` hands
//! the terminal to the setup wizard (`wizard`), as an attach would; back on
//! the list, the footer says what it did and setup is checked again.

use super::app::{App, Checked, Done};
use crate::setup::{Host, Need};

/// The footer's notice: which harnesses need setting up, or that the check
/// could not tell; none when all is set up (or not checked yet).
pub fn notice(setup: Option<&Result<Vec<Need>, String>>) -> Option<String> {
    match setup? {
        Ok(needs) if needs.is_empty() => None,
        Ok(needs) => {
            let names: Vec<&str> = needs.iter().map(|n| n.harness.as_str()).collect();
            Some(format!("setup needed: {} · /setup", names.join(", ")))
        }
        Err(_) => Some("setup check failed · /setup says why".into()),
    }
}

/// `setup::check` for the harnesses found, as the list keeps it.
fn checked(host: &Host, home: &rl_core::Home) -> Result<Vec<Need>, String> {
    host.check(home).map_err(|e| format!("{e:#}"))
}

impl App {
    /// The start's check, in the background.
    pub fn check_setup(&mut self) {
        let Some(host) = self.setup_host.clone() else { return };
        let home = self.home.clone();
        self.work.spawn(async move {
            let needs = tokio::task::spawn_blocking(move || checked(&host, &home))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            Done {
                setup: Some(Checked { needs }),
                ..Done::said("")
            }
        });
    }

    /// `/setup`: the wizard, once the list has let go of the terminal.
    pub(super) fn setup_now(&mut self) {
        if self.setup_host.is_none() {
            return self.say("setup: where this rl is is not known");
        }
        self.wizard = true;
    }

    /// What a check came to: kept for the notice.
    pub(super) fn checked(&mut self, c: Checked) {
        self.setup = Some(c.needs);
    }

    /// The footer's notice, while setup is needed.
    pub fn setup_notice(&self) -> Option<String> {
        notice(self.setup.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::{Part, State, What};

    fn need(h: &str) -> Need {
        Need {
            harness: h.into(),
            items: vec![Part {
                what: What::Mcp,
                state: State::Missing,
                file: "~/x".into(),
            }],
        }
    }

    #[test]
    fn the_notice_names_the_harnesses_and_is_quiet_when_all_is_set_up() {
        assert_eq!(notice(None), None, "not checked yet");
        assert_eq!(notice(Some(&Ok(vec![]))), None);
        assert_eq!(
            notice(Some(&Ok(vec![need("codex"), need("antigravity")]))).as_deref(),
            Some("setup needed: codex, antigravity · /setup")
        );
        assert_eq!(
            notice(Some(&Err("x".into()))).as_deref(),
            Some("setup check failed · /setup says why")
        );
    }
}
