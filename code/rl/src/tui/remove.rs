//! `^x` twice, in words: what the second press does to the line under the
//! cursor, said by the first. A session — one Relay holds, or one of a
//! harness's own that its harness can remove — is asked about, the same
//! question for every harness: the second press puts a choice in the
//! footer, `s stop` where it is still going and `r remove`, which deletes it
//! for good, and `enter` takes the first. What each does is its harness's:
//! `stop` is `claude stop`, a Codex turn interrupted, a held session's
//! holder closed; `remove` is `claude rm`, Codex's `thread/delete`, a held
//! session stopped if need be and its folder removed (`Harness::stop`,
//! `Harness::remove`: the list asks the session's harness, whoever runs
//! it). One of a harness's own
//! it cannot remove but can stop is stopped. Anything else — a session an
//! agent's hooks told of, one of a harness's own it can do nothing with, a
//! project — is hidden, but a project you added (`/project`) with nothing in
//! it is taken off the list. One already hidden and shown only because hidden is
//! on is brought back. Pure, so what you are
//! told can be tested on its own.

use crate::config::ListPrefs;
use crate::rows::{self, Line, Names};
use crate::sources::{Owner, Session, Source};
use crate::store::sessions::Status;
use std::path::PathBuf;

/// How the footer offers the stop, for every harness.
pub const STOP: &str = "s stop · keeps the session";
/// How the footer offers the remove, for every harness.
pub const REMOVE: &str = "r remove · deletes the session for good";

/// Why the list only hides a session open in a terminal of its own.
pub const ELSEWHERE: &str = "it runs in another terminal";

/// Why the list can do nothing with `s` but hide it, as whose it is says:
/// one open in a terminal of its own is that terminal's to stop. None:
/// what the list can do with it is its harness's to say.
pub fn only_hidden(s: &Session) -> Option<&'static str> {
    matches!(s.owner, Owner::Elsewhere { .. }).then_some(ELSEWHERE)
}

/// What a second `^x` will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// A session still going: stopped by its harness (`Harness::stop`).
    Stop { session: Session, title: String },
    /// A session: what to do is asked, in the footer.
    Ask(Ask),
    /// A session deleted for good by its harness (`Harness::remove`; one
    /// Relay holds is stopped first where it still goes), and what was kept
    /// for its row (`key`) with it.
    Remove {
        session: Session,
        key: String,
        title: String,
    },
    /// Off the list, by key, until hidden is on.
    Hide { key: String, title: String },
    /// Back on the list.
    Unhide { key: String, title: String },
    /// A project you added (`/project`) with nothing in it: off the list,
    /// and no longer one you added.
    Forget {
        key: String,
        folder: PathBuf,
        title: String,
    },
}

impl Removal {
    /// The armed hint's words, after "press ^x again to".
    pub fn hint(&self) -> String {
        match self {
            Removal::Stop { title, .. } => format!("stop {title}"),
            Removal::Ask(a) if a.stop => format!("stop or remove {}", a.title),
            Removal::Ask(a) => format!("remove {}", a.title),
            Removal::Remove { title, .. } => format!("remove {title}"),
            Removal::Hide { title, .. } => format!("hide {title}"),
            Removal::Unhide { title, .. } => format!("bring back {title}"),
            Removal::Forget { title, .. } => format!("take {title} off the list"),
        }
    }
}

/// What the second `^x` asks of a session: stop it, where it is still
/// going, or remove it. The same words for every harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub harness: String,
    pub title: String,
    /// What its harness stops and removes.
    pub session: Session,
    /// Its row's key: what was kept for it goes with a remove.
    pub key: String,
    /// Whether the stop is offered (first): it is still going.
    pub stop: bool,
}

impl Ask {
    /// The footer's words while it asks: each choice and how to cancel, then
    /// its name and harness — last, so a narrow footer keeps the keys.
    pub fn offer(&self) -> String {
        let choices: Vec<&str> = self
            .stop
            .then_some(STOP)
            .into_iter()
            .chain([REMOVE, "esc cancel"])
            .collect();
        format!("{}   — {} · {}", choices.join("   "), self.title, self.harness)
    }

    /// The same, the stop left out: only the remove is offered (`/rm`).
    pub fn remove_only(self) -> Ask {
        Ask { stop: false, ..self }
    }

    /// What `key` takes: `s` the stop, where it is offered, `r` the remove.
    /// `None`: not one of the choices.
    pub fn take(&self, key: char) -> Option<Removal> {
        let (session, title) = (self.session.clone(), self.title.clone());
        match key {
            's' if self.stop => Some(Removal::Stop { session, title }),
            'r' => Some(Removal::Remove {
                session,
                key: self.key.clone(),
                title,
            }),
            _ => None,
        }
    }

    /// What `enter` takes: the first offered, the stop where there is one.
    pub fn first(&self) -> Removal {
        let key = if self.stop { 's' } else { 'r' };
        self.take(key).expect("the first is one of its choices")
    }
}

/// What `^x` does to `line`. `status` is how a Relay-held session stands now,
/// by name, from its files; `None` when it is gone. `Err` says why there is
/// nothing to do.
pub fn plan(line: &Line, prefs: &ListPrefs, status: impl Fn(&str) -> Option<Status>) -> Result<Removal, String> {
    let hidden = |key: &str| prefs.hidden.iter().any(|h| h.key == key);
    match line {
        Line::Group(h) if !h.is_project() => return Err("ctrl+x stops, removes or hides what the cursor is on".into()),
        Line::Group(_) => {}
        Line::Session(r) => {
            let title = Names::new(&prefs.names).title(r).to_string();
            if let Owner::Relay { dir, .. } = &r.session.owner {
                // As it stands now, not as last read.
                let live = match status(&r.session.id) {
                    Some(Status::Live) => true,
                    Some(_) => false,
                    None => return Err(format!("{title} is gone already")),
                };
                let session = Session {
                    owner: Owner::Relay { live, dir: dir.clone() },
                    ..r.session.clone()
                };
                return Ok(Removal::Ask(Ask {
                    harness: r.harness.clone(),
                    title,
                    session,
                    key: r.key.clone(),
                    stop: live,
                }));
            }
            // One hidden and shown is brought back first.
            if r.source == Source::Native && !hidden(&r.key) && r.removes {
                return Ok(Removal::Ask(Ask {
                    harness: r.harness.clone(),
                    title,
                    session: r.session.clone(),
                    key: r.key.clone(),
                    stop: r.stop.is_some(),
                }));
            }
            if r.source == Source::Native && !hidden(&r.key) && r.stop.is_some() {
                return Ok(Removal::Stop {
                    session: r.session.clone(),
                    title,
                });
            }
        }
    }
    hide_plan(line, prefs)
}

/// `line` hidden, or brought back if it is hidden: a session of any kind,
/// or a project — but a project you added with nothing in it is taken off
/// the list, not hidden. `Err` says why not.
pub fn hide_plan(line: &Line, prefs: &ListPrefs) -> Result<Removal, String> {
    let names = Names::new(&prefs.names);
    let hidden = |key: &str| prefs.hidden.iter().any(|h| h.key == key);
    let (key, title) = match line {
        Line::Group(h)
            if h.is_project() && h.empty && !hidden(&h.key) && rows::added(prefs).any(|f| f == h.workspace) =>
        {
            return Ok(Removal::Forget {
                key: h.key.clone(),
                folder: h.workspace.clone(),
                title: h.name.clone(),
            });
        }
        Line::Group(h) if h.is_project() => (h.key.clone(), h.name.clone()),
        Line::Group(_) => return Err("a heading has nothing to hide".into()),
        Line::Session(r) => {
            let title = names.title(r).to_string();
            if !hidden(&r.key) {
                let project = rows::project_of(r, &names);
                if rows::hidden_with(&project, &prefs.hidden).is_some() {
                    return Err(format!(
                        "{title} is hidden with its project · ctrl+x on its header brings it back"
                    ));
                }
            }
            (r.key.clone(), title)
        }
    };
    Ok(if hidden(&key) {
        Removal::Unhide { key, title }
    } else {
        Removal::Hide { key, title }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Hidden;
    use crate::rows::Head;
    use crate::sources::{Attach, Row};
    use jiff::Timestamp;
    use rl_core::state::{RunKind, RunState};
    use rl_session::exit::Exit;
    use std::path::PathBuf;

    fn row(key: &str, source: Source) -> Row {
        let name = key.rsplit(':').next().unwrap().to_string();
        let owner = match source {
            Source::Held => Owner::Relay {
                live: true,
                dir: format!("/h/sessions/{name}").into(),
            },
            Source::Native => Owner::Own,
            Source::Hooked => Owner::Elsewhere {
                listed: true,
                open: false,
            },
        };
        Row {
            key: key.into(),
            source,
            harness: "claude".into(),
            name: name.clone(),
            cwd: "/w/api".into(),
            state: RunState::Working,
            words: "working".into(),
            summary: None,
            started: None,
            ended: None,
            state_at: None,
            pr: None,
            color: None,
            attach: match source {
                Source::Held => Attach::Held(name.clone()),
                _ => Attach::None,
            },
            stop: None,
            removes: false,
            kind: RunKind::Agent,
            repo: None,
            session: Session {
                harness: "claude".into(),
                owner,
                id: name.clone(),
                session_id: String::new(),
                cwd: "/w/api".into(),
                state: RunState::Working,
                words: "working".into(),
                name,
                relay_name: None,
                attached: false,
            },
        }
    }

    fn head(folder: &str) -> Head {
        Head {
            key: format!("g:{folder}"),
            place: folder.into(),
            workspace: PathBuf::from(folder),
            name: "api".into(),
            own: None,
            attention: 0,
            empty: false,
            heading: None,
            checkout: None,
        }
    }

    fn ended() -> Status {
        Status::Ended(Exit {
            code: 0,
            signal: None,
            at: Timestamp::UNIX_EPOCH,
        })
    }

    /// One of `harness`'s own, as its harness offers it: a stop while it
    /// goes, and its remove always.
    fn natives(harness: &str, live: bool) -> Row {
        let mut n = row(&format!("n:{harness}:a1"), Source::Native);
        n.harness = harness.into();
        n.session.harness = harness.into();
        if live {
            n.stop = Some("a1".into());
        }
        n.removes = true;
        n
    }

    /// The question `^x` twice asks of `r`.
    fn asked(r: &Row, status: Option<Status>) -> Ask {
        match plan(&Line::Session(r), &ListPrefs::default(), |_| status.clone()).unwrap() {
            Removal::Ask(a) => a,
            other => panic!("asked, not {other:?}"),
        }
    }

    #[test]
    fn every_harness_is_asked_the_same_question() {
        // Running, then at rest: Claude's, Codex's, and one Relay holds.
        for live in [true, false] {
            let status = Some(if live { Status::Live } else { ended() });
            let mut held = row("r:a1", Source::Held);
            held.harness = "gemini".into();
            let asks: Vec<(String, String, String)> = [natives("claude", live), natives("codex", live), held]
                .iter()
                .map(|r| {
                    let a = asked(r, status.clone());
                    let offer = a.offer();
                    let (choices, whose) = offer.split_once("   — ").unwrap();
                    (Removal::Ask(a).hint(), choices.to_string(), whose.to_string())
                })
                .collect();
            let (hint, choices) = if live {
                (
                    "stop or remove a1",
                    "s stop · keeps the session   r remove · deletes the session for good   esc cancel",
                )
            } else {
                ("remove a1", "r remove · deletes the session for good   esc cancel")
            };
            for (h, c, _) in &asks {
                assert_eq!((h.as_str(), c.as_str()), (hint, choices), "live: {live}");
            }
            let whose: Vec<&str> = asks.iter().map(|(_, _, w)| w.as_str()).collect();
            assert_eq!(
                whose,
                ["a1 · claude", "a1 · codex", "a1 · gemini"],
                "only who it is differs"
            );
        }
    }

    #[test]
    fn a_held_session_is_asked_about_stopped_by_s_and_removed_by_r() {
        let r = row("r:api-sh", Source::Held);
        let live = asked(&r, Some(Status::Live));
        let Some(Removal::Stop { session, title }) = live.take('s') else {
            panic!("s stops it");
        };
        assert_eq!((session.id.as_str(), title.as_str()), ("api-sh", "api-sh"));
        let Some(Removal::Remove { session, key, .. }) = live.take('r') else {
            panic!("r removes it");
        };
        assert_eq!(key, "r:api-sh");
        assert!(
            matches!(session.owner, Owner::Relay { live: true, .. }),
            "its harness stops it first, then removes it"
        );
        assert!(matches!(live.first(), Removal::Stop { .. }), "enter: the safe one");
        let over = asked(&r, Some(ended()));
        assert_eq!(over.take('s'), None, "nothing to stop");
        assert!(matches!(
            over.first(),
            Removal::Remove {
                session: Session {
                    owner: Owner::Relay { live: false, .. },
                    ..
                },
                ..
            }
        ));
        let lost = asked(&r, Some(Status::Lost));
        assert!(matches!(
            lost.first(),
            Removal::Remove {
                session: Session {
                    owner: Owner::Relay { live: false, .. },
                    ..
                },
                ..
            }
        ));
        assert_eq!(
            plan(&Line::Session(&r), &ListPrefs::default(), |_| None).unwrap_err(),
            "api-sh is gone already"
        );
        assert_eq!(
            live.clone().remove_only(),
            Ask { stop: false, ..live },
            "/rm: the remove alone"
        );
    }

    #[test]
    fn a_harnesss_own_session_is_stopped_or_removed_by_its_harness() {
        let n = natives("claude", true);
        let ask = asked(&n, None);
        assert!(matches!(ask.take('s'), Some(Removal::Stop { ref session, .. }) if *session == n.session));
        assert_eq!(
            ask.take('r'),
            Some(Removal::Remove {
                session: n.session.clone(),
                key: "n:claude:a1".into(),
                title: "a1".into(),
            })
        );
        assert_eq!(ask.take('x'), None);
        assert_eq!(ask.take('a'), None, "no archive");
        assert!(matches!(ask.first(), Removal::Stop { .. }), "enter: the safe one");
        let ended = asked(&natives("claude", false), None);
        assert_eq!(ended.take('s'), None, "nothing to stop");
        assert!(matches!(ended.first(), Removal::Remove { .. }));
        assert_eq!(ask.remove_only(), ended, "/rm: the remove alone");
        // Hidden and shown: brought back first.
        let mut prefs = ListPrefs::default();
        prefs.hidden.push(Hidden {
            key: "n:claude:a1".into(),
            at: Timestamp::UNIX_EPOCH,
        });
        assert!(matches!(
            plan(&Line::Session(&natives("claude", true)), &prefs, |_| None).unwrap(),
            Removal::Unhide { .. }
        ));
    }

    #[test]
    fn what_is_not_relays_is_hidden_and_brought_back() {
        let mut prefs = ListPrefs::default();
        prefs.names.insert("n:claude:a1".into(), "the fix".into());
        let n = row("n:claude:a1", Source::Native);
        let hide = plan(&Line::Session(&n), &prefs, |_| None).unwrap();
        assert_eq!(hide.hint(), "hide the fix", "by the name it goes by");
        let o = row("o:claude:s1", Source::Hooked);
        assert!(matches!(
            plan(&Line::Session(&o), &prefs, |_| None).unwrap(),
            Removal::Hide { .. }
        ));
        assert_eq!(
            only_hidden(&o.session),
            Some("it runs in another terminal"),
            "why, as whose it is says"
        );
        assert_eq!(only_hidden(&n.session), None, "its harness's to say");
        let h = head("/w/api");
        assert_eq!(
            plan(&Line::Group(h.clone()), &prefs, |_| None).unwrap().hint(),
            "hide api"
        );

        prefs.hidden.push(Hidden {
            key: "g:/w/api".into(),
            at: Timestamp::UNIX_EPOCH,
        });
        assert_eq!(
            plan(&Line::Group(h), &prefs, |_| None).unwrap().hint(),
            "bring back api"
        );
        // Hidden with its project, a row is brought back from the header.
        assert!(
            plan(&Line::Session(&o), &prefs, |_| None)
                .unwrap_err()
                .contains("hidden with its project")
        );
        prefs.hidden.push(Hidden {
            key: "o:claude:s1".into(),
            at: Timestamp::UNIX_EPOCH,
        });
        assert!(matches!(
            plan(&Line::Session(&o), &prefs, |_| None).unwrap(),
            Removal::Unhide { .. }
        ));
    }

    #[test]
    fn a_harnesss_own_session_it_cannot_remove_is_stopped_while_it_goes_and_hidden_once_ended() {
        let prefs = ListPrefs::default();
        let mut n = row("n:claude:a1", Source::Native);
        n.stop = Some("a1".into());
        let stop = plan(&Line::Session(&n), &prefs, |_| None).unwrap();
        assert_eq!(stop.hint(), "stop a1");
        assert_eq!(
            stop,
            Removal::Stop {
                session: n.session.clone(),
                title: "a1".into()
            }
        );
        n.stop = None;
        assert!(matches!(
            plan(&Line::Session(&n), &prefs, |_| None).unwrap(),
            Removal::Hide { .. }
        ));
    }

    #[test]
    fn a_state_heading_has_nothing_to_remove() {
        let prefs = ListPrefs::default();
        let h = Head::heading(crate::rows::Heading::Working, 0);
        assert!(plan(&Line::Group(h), &prefs, |_| None).is_err());
    }
}
