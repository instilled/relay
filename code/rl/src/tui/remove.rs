//! `^x` twice, in words: what the second press does to the line under the
//! cursor, said by the first. A session Relay holds that is still going is
//! stopped; one that has ended is removed. One of a harness's own that its
//! harness can close (`claude rm`, `codex archive`) is asked about: the
//! second press puts a choice in the footer — stop it, where it is still
//! going (`claude stop`; a Codex thread's running turn interrupted), or close
//! it — and `enter` takes the first. One of a harness's own it cannot close
//! but can stop is stopped. Anything else — a session an agent's hooks told
//! of, one of a harness's own it can do nothing with, a project — is hidden.
//! One already hidden and shown only because hidden is on (or because it
//! came through) is brought back. Pure, so what you are told can be tested
//! on its own.

use crate::config::ListPrefs;
use crate::rows::{self, Line, Names};
use crate::sources::{self, Attach, Choice, Source};
use crate::store::sessions::Status;
use std::path::PathBuf;

/// What a second `^x` will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// A Relay-held session still going: closed.
    Stop { name: String, title: String },
    /// One of a harness's own still going: its harness stops it by its id,
    /// in its folder (`Harness::stop`).
    StopNative {
        harness: String,
        id: String,
        cwd: PathBuf,
        title: String,
    },
    /// One of a harness's own it can close: what to do is asked, in the
    /// footer.
    Ask(Ask),
    /// One of a harness's own closed by its harness, by its id, in its folder
    /// (`Harness::close`); `done` says so after.
    CloseNative {
        harness: String,
        id: String,
        cwd: PathBuf,
        title: String,
        done: String,
    },
    /// A Relay-held session that has ended (or whose holder is lost): its
    /// folder removed.
    Remove { name: String, title: String },
    /// Off the list, by key, until hidden is on.
    Hide { key: String, title: String },
    /// Back on the list.
    Unhide { key: String, title: String },
}

impl Removal {
    /// The armed hint's words, after "press ^x again to".
    pub fn hint(&self) -> String {
        match self {
            Removal::Stop { title, .. } | Removal::StopNative { title, .. } => format!("stop {title}"),
            Removal::Ask(a) => match &a.stop {
                Some(_) => format!("stop or {} {}", a.close.words, a.title),
                None => format!("{} {}", a.close.words, a.title),
            },
            Removal::CloseNative { title, .. } => format!("close {title}"),
            Removal::Remove { title, .. } => format!("remove {title}"),
            Removal::Hide { title, .. } => format!("hide {title}"),
            Removal::Unhide { title, .. } => format!("bring back {title}"),
        }
    }
}

/// What the second `^x` asks of one of a harness's own sessions: stop it,
/// where it is still going, or close it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub harness: String,
    pub id: String,
    pub cwd: PathBuf,
    pub title: String,
    /// Offered first, where its harness can stop it as it stands.
    pub stop: Option<Choice>,
    pub close: Choice,
}

impl Ask {
    /// The footer's words while it asks: each choice and how to cancel, then
    /// its name and harness — last, so a narrow footer keeps the keys.
    pub fn offer(&self) -> String {
        let choices: Vec<String> = self
            .stop
            .iter()
            .chain([&self.close])
            .map(Choice::offer)
            .chain(["esc cancel".to_string()])
            .collect();
        format!("{}   — {} · {}", choices.join("   "), self.title, self.harness)
    }

    /// The same, the stop left out: only the close is offered (`/rm`).
    pub fn close_only(self) -> Ask {
        Ask { stop: None, ..self }
    }

    /// What `key` takes: `s` the stop, the close's own key the close, and
    /// `enter` the first offered. `None`: not one of the choices.
    pub fn take(&self, key: char) -> Option<Removal> {
        let stop = self
            .stop
            .as_ref()
            .filter(|s| s.key == key)
            .map(|_| Removal::StopNative {
                harness: self.harness.clone(),
                id: self.id.clone(),
                cwd: self.cwd.clone(),
                title: self.title.clone(),
            });
        stop.or_else(|| {
            (self.close.key == key).then(|| Removal::CloseNative {
                harness: self.harness.clone(),
                id: self.id.clone(),
                cwd: self.cwd.clone(),
                title: self.title.clone(),
                done: self.close.done.clone(),
            })
        })
    }

    /// What `enter` takes: the first offered, the stop where there is one.
    pub fn first(&self) -> Removal {
        let key = self.stop.as_ref().map_or(self.close.key, |s| s.key);
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
            if r.source == Source::Held
                && let Attach::Held(name) = &r.attach
            {
                return match status(name) {
                    Some(Status::Live) => Ok(Removal::Stop {
                        name: name.clone(),
                        title,
                    }),
                    Some(_) => Ok(Removal::Remove {
                        name: name.clone(),
                        title,
                    }),
                    None => Err(format!("{title} is gone already")),
                };
            }
            // One hidden and shown is brought back first.
            if r.source == Source::Native
                && !hidden(&r.key)
                && let Some(close) = &r.close
                && let Some(id) = sources::native_id(r)
            {
                return Ok(Removal::Ask(Ask {
                    harness: r.harness.clone(),
                    id: id.to_string(),
                    cwd: r.cwd.clone(),
                    title,
                    stop: r.stop.as_ref().and(r.stop_choice.clone()),
                    close: close.clone(),
                }));
            }
            if r.source == Source::Native
                && !hidden(&r.key)
                && let Some(id) = &r.stop
            {
                return Ok(Removal::StopNative {
                    harness: r.harness.clone(),
                    id: id.clone(),
                    cwd: r.cwd.clone(),
                    title,
                });
            }
        }
    }
    hide_plan(line, prefs)
}

/// `line` hidden, or brought back if it is hidden: a session of any kind,
/// or a project. `Err` says why not.
pub fn hide_plan(line: &Line, prefs: &ListPrefs) -> Result<Removal, String> {
    let names = Names::new(&prefs.names);
    let hidden = |key: &str| prefs.hidden.iter().any(|h| h.key == key);
    let (key, title) = match line {
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
    use crate::sources::Row;
    use jiff::Timestamp;
    use rl_core::state::{RunKind, RunState};
    use rl_session::exit::Exit;
    use std::path::PathBuf;

    fn row(key: &str, source: Source) -> Row {
        let name = key.rsplit(':').next().unwrap().to_string();
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
                Source::Held => Attach::Held(name),
                _ => Attach::None,
            },
            stop: None,
            stop_choice: None,
            close: None,
            kind: RunKind::Agent,
            repo: None,
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

    #[test]
    fn a_held_session_is_stopped_while_it_goes_and_removed_once_it_has_ended() {
        let prefs = ListPrefs::default();
        let r = row("r:api-sh", Source::Held);
        let live = plan(&Line::Session(&r), &prefs, |_| Some(Status::Live)).unwrap();
        assert_eq!(live.hint(), "stop api-sh");
        assert!(matches!(live, Removal::Stop { ref name, .. } if name == "api-sh"));
        let over = plan(&Line::Session(&r), &prefs, |_| Some(ended())).unwrap();
        assert_eq!(over.hint(), "remove api-sh");
        let lost = plan(&Line::Session(&r), &prefs, |_| Some(Status::Lost)).unwrap();
        assert!(matches!(lost, Removal::Remove { .. }));
        assert_eq!(
            plan(&Line::Session(&r), &prefs, |_| None).unwrap_err(),
            "api-sh is gone already"
        );
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
    fn a_harnesss_own_session_still_going_is_stopped_by_it_and_one_ended_hidden() {
        let prefs = ListPrefs::default();
        let mut n = row("n:claude:a1", Source::Native);
        n.stop = Some("a1".into());
        let stop = plan(&Line::Session(&n), &prefs, |_| None).unwrap();
        assert_eq!(stop.hint(), "stop a1");
        assert_eq!(
            stop,
            Removal::StopNative {
                harness: n.harness.clone(),
                id: "a1".into(),
                cwd: "/w/api".into(),
                title: "a1".into()
            }
        );
        n.stop = None;
        assert!(matches!(
            plan(&Line::Session(&n), &prefs, |_| None).unwrap(),
            Removal::Hide { .. }
        ));
    }

    fn choice(key: char, words: &str, says: Option<&str>, done: &str) -> Choice {
        Choice {
            key,
            words: words.into(),
            says: says.map(String::from),
            done: done.into(),
        }
    }

    /// One of Claude's own, as its harness offers it: a stop while it goes,
    /// and its remove always.
    fn claudes(live: bool) -> Row {
        let mut n = row("n:claude:a1", Source::Native);
        if live {
            n.stop = Some("a1".into());
            n.stop_choice = Some(choice('s', "stop", Some("keeps the conversation"), "stopped"));
        }
        n.close = Some(choice('r', "remove", Some("deletes it and its worktree"), "removed"));
        n
    }

    #[test]
    fn a_harnesss_own_session_it_can_close_is_asked_about() {
        let prefs = ListPrefs::default();
        let n = claudes(true);
        let Removal::Ask(ask) = plan(&Line::Session(&n), &prefs, |_| None).unwrap() else {
            panic!("asked");
        };
        assert_eq!(Removal::Ask(ask.clone()).hint(), "stop or remove a1");
        assert_eq!(
            ask.offer(),
            "s stop · keeps the conversation   r remove · deletes it and its worktree   esc cancel   — a1 · claude"
        );
        assert!(matches!(ask.take('s'), Some(Removal::StopNative { ref id, .. }) if id == "a1"));
        assert_eq!(
            ask.take('r'),
            Some(Removal::CloseNative {
                harness: "claude".into(),
                id: "a1".into(),
                cwd: "/w/api".into(),
                title: "a1".into(),
                done: "removed".into()
            })
        );
        assert_eq!(ask.take('x'), None);
        assert!(matches!(ask.first(), Removal::StopNative { .. }), "enter: the safe one");
        // Ended: only the close, and enter takes it.
        let Removal::Ask(ended) = plan(&Line::Session(&claudes(false)), &prefs, |_| None).unwrap() else {
            panic!("asked");
        };
        assert_eq!(Removal::Ask(ended.clone()).hint(), "remove a1");
        assert_eq!(
            ended.offer(),
            "r remove · deletes it and its worktree   esc cancel   — a1 · claude"
        );
        assert_eq!(ended.take('s'), None, "nothing to stop");
        assert!(matches!(ended.first(), Removal::CloseNative { .. }));
        assert_eq!(ask.close_only(), ended, "/rm: the close alone");
        // Hidden and shown: brought back first.
        let mut prefs = ListPrefs::default();
        prefs.hidden.push(Hidden {
            key: "n:claude:a1".into(),
            at: Timestamp::UNIX_EPOCH,
        });
        assert!(matches!(
            plan(&Line::Session(&claudes(true)), &prefs, |_| None).unwrap(),
            Removal::Unhide { .. }
        ));
    }

    #[test]
    fn a_state_heading_has_nothing_to_remove() {
        let prefs = ListPrefs::default();
        let h = Head::heading(crate::rows::Heading::Working, 0);
        assert!(plan(&Line::Group(h), &prefs, |_| None).is_err());
    }
}
