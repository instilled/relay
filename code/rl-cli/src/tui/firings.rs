//! A flow's firings in its pane: the steps of the one shown — the latest, unless another was
//! picked — the last few under them, the files the shown one's steps wrote, and a caret that
//! walks the steps and then the firings. `enter` on a step joins its run; on a firing, shows its
//! steps. Pure but for reading the firing's folder.

use super::flows::state_words;
use super::rows::fit;
use super::theme;
use crossterm::event::KeyCode;
use jiff::Timestamp;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use rl_core::time::fmt_when;
use rl_proto::flow::Firing;
use std::path::Path;

/// How many of a flow's last firings its pane lists.
pub const LAST: u32 = 8;

/// A flow's pane: its last firings as fetched, which one it shows, where the caret is.
#[derive(Debug, Default, Clone)]
pub struct FlowPane {
    /// The flow these are of.
    pub flow: String,
    /// Its last firings, newest first, as `flow.firings` gave them.
    pub firings: Vec<Firing>,
    /// The firing picked to be shown; none: the latest.
    pub picked: Option<String>,
    /// The caret, while the pane is walked: an index into `stops`.
    pub at: usize,
    /// The files in the shown firing's folder, by name, with their sizes.
    pub files: Vec<(String, u64)>,
}

/// Where the caret can stand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// A step of the firing shown, and its run when it has one.
    Step { name: String, run: Option<String> },
    /// One of the last firings, by id.
    Firing(String),
}

/// What a key did to a walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Walked {
    /// The caret moved, a firing was shown, or nothing happened: still walking.
    On,
    /// `esc`, or nothing to walk: the walk is over.
    Done,
    /// `enter` on a step with a run: that run, to join.
    Join(String),
    /// `enter` on a step with no run: its name.
    NoRun(String),
}

impl FlowPane {
    /// `flow`'s last firings, newest first. What was picked and the caret stay while it is the
    /// same flow and they still exist; another flow starts afresh.
    pub fn load(&mut self, flow: &str, firings: Vec<Firing>) {
        if self.flow != flow {
            *self = FlowPane {
                flow: flow.to_string(),
                ..Default::default()
            };
        }
        self.firings = firings;
        if self
            .picked
            .as_ref()
            .is_some_and(|id| !self.firings.iter().any(|f| &f.id == id))
        {
            self.picked = None;
        }
        self.at = self.at.min(self.stops().len().saturating_sub(1));
    }

    /// The firing shown: the one picked, else the latest.
    pub fn shown(&self) -> Option<&Firing> {
        match &self.picked {
            Some(id) => self.firings.iter().find(|f| &f.id == id),
            None => self.firings.first(),
        }
    }

    /// The firing picked, when it is not the latest: what `/summary` on the flow's row tells.
    pub fn picked_firing(&self) -> Option<&Firing> {
        self.picked.as_ref().and(self.shown())
    }

    fn steps_len(&self) -> usize {
        self.shown().map_or(0, |f| f.steps.len())
    }

    /// Where the caret can stand: the shown firing's steps, then each of the last firings.
    pub fn stops(&self) -> Vec<Stop> {
        let steps = self.shown().into_iter().flat_map(|f| &f.steps).map(|s| Stop::Step {
            name: s.name.clone(),
            run: s.run.clone(),
        });
        steps
            .chain(self.firings.iter().map(|f| Stop::Firing(f.id.clone())))
            .collect()
    }

    /// The step the caret is on, by its place among the shown firing's steps.
    pub fn step_at(&self) -> Option<usize> {
        (self.at < self.steps_len()).then_some(self.at)
    }

    /// A key while the pane is walked.
    pub fn key(&mut self, code: KeyCode) -> Walked {
        let stops = self.stops();
        let last = stops.len().saturating_sub(1);
        match code {
            KeyCode::Up => {
                self.at = self.at.saturating_sub(1);
                Walked::On
            }
            KeyCode::Down => {
                self.at = (self.at + 1).min(last);
                Walked::On
            }
            KeyCode::Esc => Walked::Done,
            KeyCode::Enter => match stops.get(self.at) {
                Some(Stop::Step { run: Some(r), .. }) => Walked::Join(r.clone()),
                Some(Stop::Step { name, run: None }) => Walked::NoRun(name.clone()),
                Some(Stop::Firing(id)) => {
                    let latest = self.firings.first().map(|f| &f.id) == Some(id);
                    self.picked = (!latest).then(|| id.clone());
                    self.at = 0;
                    Walked::On
                }
                None => Walked::Done,
            },
            _ => Walked::On,
        }
    }

    /// The last firings, newest first, a line each — when, where it stands, what it came to —
    /// the one shown marked `▸`, and while walking the caret's lit.
    pub fn firing_lines(&self, walking: bool, w: usize, now: Timestamp) -> Vec<Line<'static>> {
        let steps = self.steps_len();
        let shown = self.shown().map(|f| f.id.clone());
        let mut lines = vec![Line::raw(""), Line::from(Span::raw(" firings").dim())];
        for (n, f) in self.firings.iter().enumerate() {
            let lit = walking && self.at == steps + n;
            let caret = match (lit, shown.as_deref() == Some(f.id.as_str())) {
                (true, _) => "❯",
                (false, true) => "▸",
                _ => " ",
            };
            let row = format!(
                " {caret} {:<12} {:<18} {}",
                fmt_when(f.started_at, now),
                state_words(f),
                f.outcome.as_deref().unwrap_or("")
            );
            let line = Line::raw(fit(&row, w).trim_end().to_string());
            lines.push(if lit { line.style(theme::selected()) } else { line });
        }
        lines
    }
}

/// The files a firing's folder holds — what its steps wrote to `$RELAY_WORK` — by name, with
/// their sizes; Relay's own `.relay/`, anything hidden and any folder left out. None when the
/// folder is not there.
pub fn files_in(dir: &Path) -> Vec<(String, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut out: Vec<(String, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let meta = e.metadata().ok()?;
            (!name.starts_with('.') && meta.is_file()).then_some((name, meta.len()))
        })
        .collect();
    out.sort();
    out
}

/// `note.txt 12 B · report.md 2 kB`.
pub fn files_words(files: &[(String, u64)]) -> String {
    files
        .iter()
        .map(|(name, len)| format!("{name} {}", size(*len)))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn size(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} kB", n >> 10),
        n => format!("{n} B"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::flow::{FiringStatus, StepState, StepStatus};

    fn firing(
        id: &str,
        status: FiringStatus,
        steps: &[(&str, StepStatus, Option<&str>)],
        outcome: Option<&str>,
    ) -> Firing {
        Firing {
            id: id.into(),
            flow: "prod-logs".into(),
            trigger: "when".into(),
            event: None,
            status,
            started_at: "2026-09-10T11:00:00Z".parse().unwrap(),
            ended_at: None,
            steps: steps
                .iter()
                .map(|(n, s, r)| StepState {
                    name: n.to_string(),
                    status: *s,
                    run: r.map(String::from),
                    decision: None,
                })
                .collect(),
            outcome: outcome.map(String::from),
        }
    }

    fn two() -> Vec<Firing> {
        use StepStatus::*;
        vec![
            firing(
                "fi2",
                FiringStatus::Waiting,
                &[
                    ("probe", Done, Some("r21")),
                    ("triage", Done, Some("r22")),
                    ("review", Waiting, None),
                ],
                Some("restart the api pod"),
            ),
            firing(
                "fi1",
                FiringStatus::Done,
                &[
                    ("probe", Done, Some("r11")),
                    ("triage", Skipped, None),
                    ("review", Skipped, None),
                ],
                None,
            ),
        ]
    }

    #[test]
    fn the_caret_walks_the_shown_firings_steps_then_the_last_firings() {
        let mut p = FlowPane::default();
        p.load("prod-logs", two());
        let step = |n: &str, r: Option<&str>| Stop::Step {
            name: n.into(),
            run: r.map(String::from),
        };
        assert_eq!(
            p.stops(),
            [
                step("probe", Some("r21")),
                step("triage", Some("r22")),
                step("review", None),
                Stop::Firing("fi2".into()),
                Stop::Firing("fi1".into()),
            ]
        );
        assert_eq!(
            p.key(KeyCode::Enter),
            Walked::Join("r21".into()),
            "enter on a step joins its run"
        );
        p.key(KeyCode::Down);
        p.key(KeyCode::Down);
        assert_eq!(p.key(KeyCode::Enter), Walked::NoRun("review".into()));
        for _ in 0..10 {
            p.key(KeyCode::Down);
        }
        assert_eq!(p.at, 4, "the caret stops at the last firing");
        assert_eq!(p.step_at(), None);
        assert_eq!(p.key(KeyCode::Enter), Walked::On);
        assert_eq!(
            p.shown().map(|f| f.id.as_str()),
            Some("fi1"),
            "enter on a firing shows its steps"
        );
        assert_eq!(p.picked_firing().map(|f| f.id.as_str()), Some("fi1"));
        assert_eq!((p.at, p.step_at()), (0, Some(0)), "the caret on its first step");
        assert_eq!(p.key(KeyCode::Enter), Walked::Join("r11".into()));
        p.at = 3;
        assert_eq!(p.key(KeyCode::Enter), Walked::On, "the latest, again");
        assert_eq!(p.picked_firing(), None);
        assert_eq!(p.shown().map(|f| f.id.as_str()), Some("fi2"));
        assert_eq!(p.key(KeyCode::Esc), Walked::Done);
    }

    #[test]
    fn fewer_firings_on_a_refresh_keep_the_caret_in_reach() {
        let mut p = FlowPane::default();
        p.load("prod-logs", two());
        p.at = 4;
        p.key(KeyCode::Enter); // fi1 picked
        p.at = 4;
        let latest = two().remove(0);
        p.load("prod-logs", vec![latest]);
        assert_eq!(p.picked, None, "the picked one is gone: the latest is shown");
        assert!(p.at < p.stops().len(), "{} of {}", p.at, p.stops().len());
        p.load("prod-logs", vec![]);
        assert_eq!((p.at, p.stops().len()), (0, 0));
        assert_eq!(p.key(KeyCode::Enter), Walked::Done, "nothing to walk");
        p.load("prod-logs", two());
        p.at = 2;
        p.load("morning-brief", vec![]);
        assert_eq!(
            (p.flow.as_str(), p.at),
            ("morning-brief", 0),
            "another flow starts afresh"
        );
    }

    #[test]
    fn a_firings_folder_shows_what_its_steps_wrote() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("note.txt"), "twelve bytes").unwrap();
        std::fs::write(tmp.path().join("report.md"), vec![b'x'; 2048]).unwrap();
        std::fs::write(tmp.path().join(".hidden"), "x").unwrap();
        std::fs::create_dir_all(tmp.path().join(".relay/probe")).unwrap();
        std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
        let files = files_in(tmp.path());
        assert_eq!(files, [("note.txt".to_string(), 12), ("report.md".to_string(), 2048)]);
        assert_eq!(files_words(&files), "note.txt 12 B · report.md 2 kB");
        assert!(files_in(&tmp.path().join("gone")).is_empty());
    }

    #[test]
    fn the_last_firings_say_where_each_stands_and_what_it_came_to() {
        let mut p = FlowPane::default();
        p.load("prod-logs", two());
        let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
        let text: Vec<String> = p.firing_lines(false, 100, now).iter().map(|l| l.to_string()).collect();
        assert_eq!(text[1], " firings");
        assert!(
            text[2].starts_with(" ▸ ")
                && text[2].contains("waiting at review")
                && text[2].ends_with("restart the api pod"),
            "{text:#?}"
        );
        assert!(
            text[3].starts_with("   ") && text[3].trim_end().ends_with("done"),
            "{text:#?}"
        );
        p.at = 4;
        let text: Vec<String> = p.firing_lines(true, 100, now).iter().map(|l| l.to_string()).collect();
        assert!(text[3].starts_with(" ❯ "), "the caret: {text:#?}");
    }
}
