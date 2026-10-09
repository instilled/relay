//! What heads the catch-up: for each flow that says `catch_up: top`, its latest firing when that
//! started or ended since you left — the morning brief above Relay's own bullets, under its
//! flow's name, what it came to under that. The panel and `rl since` say it alike.

use crate::client::Client;
use crate::tui::flows::state_words;
use crate::tui::rows::FLOW;
use jiff::Timestamp;
use rl_core::time::fmt_when;
use rl_proto::flow::{Firing, FiringRef, FiringStatus, Flow, FlowOutcome, Outcome};
use std::collections::HashMap;

/// How many lines of what a lead came to the catch-up shows; the pane shows the rest.
pub const LINES: usize = 8;

/// A firing heading the catch-up, and its flow's name.
#[derive(Debug, Clone, Copy)]
pub struct Lead<'a> {
    pub flow: &'a str,
    pub firing: &'a Firing,
}

/// The firings heading a catch-up from `since`, newest first: for each flow that says
/// `catch_up: top`, its latest firing (`firings`: the digest's, each flow's latest) when that
/// started or ended since — one still going too.
pub fn leads<'a>(flows: &'a [Flow], firings: &'a [Firing], since: Timestamp) -> Vec<Lead<'a>> {
    let mut out: Vec<Lead> = flows
        .iter()
        .filter(|f| f.removed_at.is_none() && f.heads_catch_up())
        .filter_map(|f| {
            let firing = firings.iter().find(|x| x.flow == f.name)?;
            let news = firing.started_at >= since || firing.ended_at.is_some_and(|e| e >= since);
            news.then_some(Lead { flow: &f.name, firing })
        })
        .collect();
    out.sort_by(|a, b| {
        b.firing
            .started_at
            .cmp(&a.firing.started_at)
            .then_with(|| a.flow.cmp(b.flow))
    });
    out
}

/// A lead's key in the catch-up, for the cursor: `h:<flow>`.
pub fn key(flow: &str) -> String {
    format!("h:{flow}")
}

/// The flow a lead's key is of.
pub fn flow_of(key: &str) -> Option<&str> {
    key.strip_prefix("h:")
}

/// A lead's title: its flow, when it fired, where it stands — `morning-brief · 07:30 · done`.
pub fn title(l: &Lead, now: Timestamp) -> String {
    format!(
        "{} · {} · {}",
        l.flow,
        fmt_when(l.firing.started_at, now),
        state_words(l.firing)
    )
}

/// What a lead came to, a line each, blank lines left out, links said by their text
/// (`links_as_text`); nothing while nothing has.
pub fn body(o: Option<&Outcome>) -> Vec<String> {
    o.map(|o| {
        o.text
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.trim().is_empty())
            .map(links_as_text)
            .collect()
    })
    .unwrap_or_default()
}

/// `line` with each Google Chat link — `<https://…|PR 123>` — said by its text, `PR 123`; the
/// rest, `*bold*` too, as it is.
pub fn links_as_text(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let link = after.find('>').and_then(|close| {
            let (url, text) = after[..close].split_once('|')?;
            let is_url = !url.contains(char::is_whitespace) && (url.contains("://") || url.starts_with("mailto:"));
            (is_url && !text.is_empty() && !text.contains('<')).then_some((text, close))
        });
        match link {
            Some((text, close)) => {
                out.push_str(text);
                rest = &after[close + 1..];
            }
            None => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The leads as `rl since` prints them over the top: `↻` and the title, what it came to under
/// it, indented, then a blank line.
pub fn text(leads: &[Lead], outcomes: &Outcomes, now: Timestamp) -> Vec<String> {
    let mut out = Vec::new();
    for l in leads {
        out.push(format!("{FLOW} {}", title(l, now)));
        out.extend(body(outcomes.of(&l.firing.id)).into_iter().map(|b| format!("  {b}")));
        out.push(String::new());
    }
    out
}

/// What a firing is, as far as asking again goes: how it stands and the first line it came to.
type Seen = (FiringStatus, Option<String>);

fn seen(f: &Firing) -> Seen {
    (f.status, f.outcome.clone())
}

/// What each lead came to, all of it, by firing: asked for once, and again when the firing has
/// moved on since.
#[derive(Debug, Default)]
pub struct Outcomes(HashMap<String, (Seen, Option<Outcome>)>);

impl Outcomes {
    pub fn put(&mut self, f: &Firing, o: Option<Outcome>) {
        self.0.insert(f.id.clone(), (seen(f), o));
    }

    /// Asks `flow.outcome` of each of `firings` not asked yet, or moved on since; one that fails
    /// is asked again next time.
    pub async fn fetch(&mut self, rpc: &mut Client, firings: Vec<Firing>) {
        for f in firings {
            if self.0.get(&f.id).is_some_and(|(s, _)| *s == seen(&f)) {
                continue;
            }
            if let Ok(o) = rpc.call::<FlowOutcome>(FiringRef { id: f.id.clone() }).await {
                self.put(&f, o);
            }
        }
    }

    pub fn of(&self, firing: &str) -> Option<&Outcome> {
        self.0.get(firing)?.1.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rl_proto::flow::{StepState, StepStatus};
    use serde_json::json;

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn flow(name: &str, top: bool) -> Flow {
        Flow {
            name: name.into(),
            hash: "h".into(),
            def: if top { json!({ "catch_up": "top" }) } else { json!({}) },
            source: format!("/h/flows/{name}").into(),
            folder: None,
            paused: false,
            approved_hash: Some("h".into()),
            last_fired_at: None,
            next_at: None,
            removed_at: None,
        }
    }

    fn firing(id: &str, flow: &str, status: FiringStatus, started: &str) -> Firing {
        Firing {
            id: id.into(),
            flow: flow.into(),
            trigger: "when".into(),
            event: None,
            status,
            started_at: at(started),
            ended_at: (status != FiringStatus::Running).then(|| at(started)),
            steps: vec![StepState {
                name: "brief".into(),
                status: if status == FiringStatus::Running {
                    StepStatus::Running
                } else {
                    StepStatus::Done
                },
                run: Some("r1".into()),
                decision: None,
            }],
            outcome: None,
        }
    }

    #[test]
    fn a_catch_up_top_flows_latest_firing_since_you_left_heads_it() {
        let since = at("2026-10-09T18:00:00Z");
        let flows = [
            flow("morning-brief", true),
            flow("prod-logs", false),
            flow("weekly", true),
            flow("standup", true),
        ];
        let firings = [
            firing("fi2", "morning-brief", FiringStatus::Done, "2026-10-10T05:30:00Z"),
            firing("fi3", "prod-logs", FiringStatus::Done, "2026-10-10T06:00:00Z"),
            firing("fi4", "weekly", FiringStatus::Done, "2026-10-09T09:00:00Z"),
            firing("fi5", "standup", FiringStatus::Running, "2026-10-10T06:30:00Z"),
        ];
        let got: Vec<(&str, &str)> = leads(&flows, &firings, since)
            .iter()
            .map(|l| (l.flow, l.firing.id.as_str()))
            .collect();
        assert_eq!(
            got,
            [("standup", "fi5"), ("morning-brief", "fi2")],
            "newest first, a running one too; prod-logs is a line under its project; \
             weekly fired before you left"
        );
    }

    #[test]
    fn a_lead_in_words_is_its_title_then_what_it_printed() {
        let now = at("2026-10-10T08:00:00Z");
        let f = firing("fi2", "morning-brief", FiringStatus::Done, "2026-10-10T05:30:00Z");
        let mut outcomes = Outcomes::default();
        outcomes.put(
            &f,
            Some(Outcome {
                firing: "fi2".into(),
                step: "brief".into(),
                text: "*Morning brief · Fri*\n\n*Anna*\n• the parser\n".into(),
            }),
        );
        let t = text(
            &[Lead {
                flow: "morning-brief",
                firing: &f,
            }],
            &outcomes,
            now,
        );
        assert!(
            t[0].starts_with("↻ morning-brief · ") && t[0].ends_with(" · done"),
            "{t:?}"
        );
        assert_eq!(
            &t[1..],
            ["  *Morning brief · Fri*", "  *Anna*", "  • the parser", ""],
            "blank lines left out"
        );

        // A source failed: the title says where, the body why.
        let mut failed = firing("fi6", "morning-brief", FiringStatus::Failed, "2026-10-10T05:30:00Z");
        failed.steps[0] = StepState {
            name: "gchat".into(),
            status: StepStatus::Failed,
            run: Some("r2".into()),
            decision: None,
        };
        outcomes.put(
            &failed,
            Some(Outcome {
                firing: "fi6".into(),
                step: "gchat".into(),
                text: "token expired".into(),
            }),
        );
        let t = text(
            &[Lead {
                flow: "morning-brief",
                firing: &failed,
            }],
            &outcomes,
            now,
        );
        assert!(t[0].ends_with(" · failed at gchat"), "{t:?}");
        assert_eq!(t[1], "  token expired");

        // Still running: the title alone.
        let going = firing("fi7", "morning-brief", FiringStatus::Running, "2026-10-10T07:30:00Z");
        let t = text(
            &[Lead {
                flow: "morning-brief",
                firing: &going,
            }],
            &Outcomes::default(),
            now,
        );
        assert!(t[0].ends_with(" · running · brief"), "{t:?}");
        assert_eq!(&t[1..], [""]);
    }

    #[test]
    fn a_link_is_said_by_its_text() {
        assert_eq!(
            links_as_text("• <https://github.com/o/r/pull/123|PR 123> by *Anna*"),
            "• PR 123 by *Anna*"
        );
        assert_eq!(
            links_as_text("<http://a.example/x?y=1|one> and <mailto:b@c.d|mail Bea>"),
            "one and mail Bea"
        );
        // Not links: left as they are.
        for kept in [
            "a <b> c",
            "x < y | z > w",
            "<not a url|text>",
            "<https://a.b|",
            "*bold*",
            "",
        ] {
            assert_eq!(links_as_text(kept), kept);
        }
        // What a lead came to says links by their text.
        let o = Outcome {
            firing: "fi1".into(),
            step: "brief".into(),
            text: "*Brief*\n\n• <https://x.y/1|PR 1> merged\n".into(),
        };
        assert_eq!(body(Some(&o)), ["*Brief*", "• PR 1 merged"]);
    }

    #[test]
    fn a_leads_key_names_its_flow() {
        assert_eq!(key("morning-brief"), "h:morning-brief");
        assert_eq!(flow_of("h:morning-brief"), Some("morning-brief"));
        assert_eq!(flow_of("f:morning-brief"), None);
    }
}
