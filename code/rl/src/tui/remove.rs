//! `^x` twice, in words: what the second press does to the line under the
//! cursor, said by the first. A session Relay holds that is still going is
//! stopped; one that has ended is removed. One of a harness's own still
//! going is stopped by its harness, where it has a way to (`claude stop`; a
//! Codex thread's running turn interrupted; nothing here ever removes one).
//! Anything else — a session an agent's hooks told of, one of a harness's
//! own that has ended or that it cannot stop, a project — is hidden. One already hidden and shown
//! only because hidden is on (or because it came through) is brought back.
//! Pure, so what you are told can be tested on its own.

use crate::config::ListPrefs;
use crate::rows::{self, Line, Names};
use crate::sources::{Attach, Source};
use crate::store::sessions::Status;
use std::path::PathBuf;

/// What a second `^x` will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// A Relay-held session still going: closed.
    Stop { name: String, title: String },
    /// One of a harness's own still going: its harness stops it by its id,
    /// in its folder (`Adapter::stop`).
    StopNative {
        harness: String,
        id: String,
        cwd: PathBuf,
        title: String,
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
            Removal::Remove { title, .. } => format!("remove {title}"),
            Removal::Hide { title, .. } => format!("hide {title}"),
            Removal::Unhide { title, .. } => format!("bring back {title}"),
        }
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

    #[test]
    fn a_state_heading_has_nothing_to_remove() {
        let prefs = ListPrefs::default();
        let h = Head::heading(crate::rows::Heading::Working, 0);
        assert!(plan(&Line::Group(h), &prefs, |_| None).is_err());
    }
}
