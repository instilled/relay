use jiff::{SignedDuration, Timestamp};
use rl_ledger::events::{self, NewEvent};
use rl_ledger::{Ledger, presence, runs};
use rl_proto::methods::{EventFilter, RunFilter};
use rl_proto::types::{Confidence, Run, RunKind, RunState, Source, event_kind};

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn run(id: &str, started: &str) -> Run {
    let mut r = Run::new(
        id,
        RunKind::Command,
        "make test",
        vec!["make".into(), "test".into()],
        "/w/repo",
        "/w/repo",
        ts(started),
    );
    r.tags = vec!["nightly".into()];
    r
}

#[test]
fn set_title_renames_a_run_and_moves_nothing_else() {
    let l = Ledger::open_in_memory().unwrap();
    let a = run("aaaaaaaa", "2026-09-08T10:00:00Z");
    l.call_blocking({
        let a = a.clone();
        move |c| runs::insert(c, &a)
    })
    .unwrap();

    l.call_blocking(|c| runs::set_title(c, "aaaaaaaa", "the nightly build"))
        .unwrap();

    let got = l.call_blocking(|c| runs::get(c, "aaaaaaaa")).unwrap().unwrap();
    assert_eq!(got.title, "the nightly build");
    // The same run under a new name: put the old title back and it matches.
    let mut back = got.clone();
    back.title = a.title.clone();
    assert_eq!(back, a, "only the title changed");

    // A run that is not there is not an error, as with the other setters.
    l.call_blocking(|c| runs::set_title(c, "nope", "x")).unwrap();
}

#[test]
fn a_run_relay_titled_is_named_once_and_a_rename_by_hand_wins() {
    let l = Ledger::open_in_memory().unwrap();
    for id in ["auto0001", "hand0001", "late0001"] {
        let r = run(id, "2026-09-08T10:00:00Z");
        l.call_blocking(move |c| runs::insert(c, &r)).unwrap();
    }
    let title = |id: &'static str| l.call_blocking(move |c| runs::get(c, id)).unwrap().unwrap().title;
    for id in ["auto0001", "late0001"] {
        l.call_blocking(move |c| runs::set_title_auto(c, id)).unwrap();
    }
    // Titled by Relay: named once, and never again.
    assert!(
        l.call_blocking(|c| runs::name_once(c, "auto0001", "claude: one"))
            .unwrap()
    );
    assert!(
        !l.call_blocking(|c| runs::name_once(c, "auto0001", "claude: two"))
            .unwrap()
    );
    assert_eq!(title("auto0001"), "claude: one");
    // Titled by whoever started it: left alone.
    assert!(
        !l.call_blocking(|c| runs::name_once(c, "hand0001", "claude: one"))
            .unwrap()
    );
    assert_eq!(title("hand0001"), run("hand0001", "2026-09-08T10:00:00Z").title);
    // Renamed by hand before it was asked anything: the rename stands.
    l.call_blocking(|c| runs::set_title(c, "late0001", "mine")).unwrap();
    assert!(
        !l.call_blocking(|c| runs::name_once(c, "late0001", "claude: one"))
            .unwrap()
    );
    assert_eq!(title("late0001"), "mine");
    assert!(!l.call_blocking(|c| runs::name_once(c, "nope0000", "x")).unwrap());
}

#[test]
fn a_pinned_run_stays_in_view_until_let_go_by_name() {
    let l = Ledger::open_in_memory().unwrap();
    let long_ago = ts("2026-09-01T10:00:00Z");
    for id in ["pinned01", "loose001"] {
        let r = run(id, "2026-09-01T09:00:00Z");
        l.call_blocking(move |c| {
            runs::insert(c, &r)?;
            runs::finish(c, &r.id, RunState::Finished, Source::Pty, Some(0), long_ago)?;
            runs::set_seen(c, std::slice::from_ref(&r.id), long_ago)?;
            Ok(())
        })
        .unwrap();
    }

    let first = ts("2026-09-02T10:00:00Z");
    l.call_blocking(move |c| runs::set_pinned(c, "pinned01", Some(first)))
        .unwrap();
    // Pinning again keeps the first time, so the band's order holds.
    l.call_blocking(|c| runs::set_pinned(c, "pinned01", Some(ts("2026-09-03T10:00:00Z"))))
        .unwrap();
    let got = l.call_blocking(|c| runs::get(c, "pinned01")).unwrap().unwrap();
    assert_eq!(got.pinned_at, Some(first));

    // Nothing has happened to either for weeks; only the pinned one is news.
    let now = ts("2026-09-16T10:00:00Z");
    let ids = |l: &Ledger| {
        l.call_blocking(move |c| runs::list_for_digest(c, now))
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&l), vec!["pinned01"]);

    // `/archive seen` leaves what was pinned alone.
    assert_eq!(l.call_blocking(move |c| runs::archive_seen(c, now)).unwrap(), 1);
    assert_eq!(ids(&l), vec!["pinned01"]);

    // Archived by name, it goes, and the pin with it.
    let n = l
        .call_blocking(move |c| runs::archive(c, &["pinned01".into()], now))
        .unwrap();
    assert_eq!(n, 1);
    let got = l.call_blocking(|c| runs::get(c, "pinned01")).unwrap().unwrap();
    assert_eq!(got.pinned_at, None);
    assert!(ids(&l).is_empty());
}

#[test]
fn removing_a_folder_archives_its_ended_runs_and_nothing_else() {
    let l = Ledger::open_in_memory().unwrap();
    let at = ts("2026-09-01T10:00:00Z");
    let mk = |id: &str, ws: &str, state: RunState| {
        let mut r = run(id, "2026-09-01T09:00:00Z");
        r.workspace = ws.into();
        r.cwd = ws.into();
        r.state = state;
        if state.is_terminal() {
            r.ended_at = Some(at);
        }
        r
    };
    let rows = [
        mk("ended001", "/w/repo", RunState::Finished),
        mk("pinned01", "/w/repo", RunState::Failed),
        mk("going001", "/w/repo", RunState::Working),
        mk("nested01", "/w/repo/inner", RunState::Finished),
        mk("other001", "/w/other", RunState::Stopped),
    ];
    l.call_blocking(move |c| {
        for r in &rows {
            runs::insert(c, r)?;
        }
        runs::set_pinned(c, "pinned01", Some(at))
    })
    .unwrap();

    let live = l
        .call_blocking(|c| runs::live_in(c, std::path::Path::new("/w/repo")))
        .unwrap();
    assert_eq!(live.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["going001"]);

    let now = ts("2026-09-02T10:00:00Z");
    let n = l
        .call_blocking(move |c| runs::archive_workspace(c, std::path::Path::new("/w/repo"), now))
        .unwrap();
    assert_eq!(n, 2, "the folder's two ended runs, pinned or not");
    let get = |id: &'static str| l.call_blocking(move |c| runs::get(c, id)).unwrap().unwrap();
    assert_eq!(get("ended001").archived_at, Some(now));
    let pinned = get("pinned01");
    assert_eq!(pinned.archived_at, Some(now));
    assert_eq!(pinned.pinned_at, None, "the pin goes with it");
    // Still going, a folder of its own inside, another folder: untouched.
    for id in ["going001", "nested01", "other001"] {
        assert_eq!(get(id).archived_at, None, "{id}");
    }
    // Twice is nothing more.
    let n = l
        .call_blocking(move |c| runs::archive_workspace(c, std::path::Path::new("/w/repo"), now))
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn runs_insert_get_list_and_transition() {
    let l = Ledger::open_in_memory().unwrap();
    let a = run("aaaaaaaa", "2026-09-08T10:00:00Z");
    let mut b = run("bbbbbbbb", "2026-09-08T11:00:00Z");
    b.branch = Some("fl-1-x".into());
    b.tags = vec![];
    l.call_blocking({
        let (a, b) = (a.clone(), b.clone());
        move |c| {
            runs::insert(c, &a)?;
            runs::insert(c, &b)
        }
    })
    .unwrap();

    let got = l.call_blocking(|c| runs::get(c, "aaaaaaaa")).unwrap().unwrap();
    assert_eq!(got, a);
    assert!(l.call_blocking(|c| runs::get(c, "nope")).unwrap().is_none());

    // Newest first.
    let all = l.call_blocking(|c| runs::list(c, &RunFilter::default())).unwrap();
    assert_eq!(
        all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["bbbbbbbb", "aaaaaaaa"]
    );

    // Tag and group filters.
    let f = RunFilter {
        tag: Some("nightly".into()),
        ..Default::default()
    };
    assert_eq!(l.call_blocking(move |c| runs::list(c, &f)).unwrap().len(), 1);
    let f = RunFilter {
        group: Some("fl-1-x".into()),
        ..Default::default()
    };
    assert_eq!(l.call_blocking(move |c| runs::list(c, &f)).unwrap()[0].id, "bbbbbbbb");
    let f = RunFilter {
        group: Some("/w/repo".into()),
        ..Default::default()
    };
    assert_eq!(l.call_blocking(move |c| runs::list(c, &f)).unwrap()[0].id, "aaaaaaaa");

    // Transitions.
    let at = ts("2026-09-08T10:05:00Z");
    assert!(
        l.call_blocking(move |c| runs::set_state(
            c,
            "aaaaaaaa",
            RunState::Working,
            Source::Pty,
            Confidence::Likely,
            at
        ))
        .unwrap()
    );
    assert!(
        !l.call_blocking(move |c| runs::set_state(c, "nope", RunState::Working, Source::Pty, Confidence::Likely, at))
            .unwrap()
    );
    let got = l.call_blocking(|c| runs::get(c, "aaaaaaaa")).unwrap().unwrap();
    assert_eq!(
        (got.state, got.source, got.confidence, got.state_at),
        (RunState::Working, Source::Pty, Confidence::Likely, at)
    );

    let end = at + SignedDuration::from_mins(41);
    l.call_blocking(move |c| runs::finish(c, "aaaaaaaa", RunState::Failed, Source::Pty, Some(1), end))
        .unwrap();
    let got = l.call_blocking(|c| runs::get(c, "aaaaaaaa")).unwrap().unwrap();
    assert_eq!(
        (got.state, got.exit_code, got.ended_at, got.confidence),
        (RunState::Failed, Some(1), Some(end), Confidence::Exact)
    );

    let f = RunFilter {
        active_only: true,
        ..Default::default()
    };
    let active = l.call_blocking(move |c| runs::list(c, &f)).unwrap();
    assert_eq!(active.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["bbbbbbbb"]);
    let f = RunFilter {
        states: vec![RunState::Failed],
        ..Default::default()
    };
    assert_eq!(l.call_blocking(move |c| runs::list(c, &f)).unwrap().len(), 1);
}

#[test]
fn events_append_and_filter() {
    let l = Ledger::open_in_memory().unwrap();
    let mut r = run("aaaaaaaa", "2026-09-08T10:00:00Z");
    r.ref_ = Some("notion:FL-1".into());
    let e1 = NewEvent::for_run(&r, ts("2026-09-08T10:00:00Z"), event_kind::RUN_STARTED, "started");
    let e2 = NewEvent::for_run(&r, ts("2026-09-08T10:30:00Z"), event_kind::RUN_FAILED, "exit 1")
        .with_data(serde_json::json!({"exit": 1}));
    let e3 = NewEvent {
        run: None,
        group_key: "other".into(),
        ..e1.clone()
    };
    let ids = l
        .call_blocking(move |c| {
            Ok((
                events::append(c, &e1)?,
                events::append(c, &e2)?,
                events::append(c, &e3)?,
            ))
        })
        .unwrap();
    assert_eq!(ids, (1, 2, 3));

    let all = l.call_blocking(|c| events::list(c, &EventFilter::default())).unwrap();
    // Oldest first, id breaks ties: e1 (10:00), e3 (10:00), e2 (10:30).
    assert_eq!(all.iter().map(|e| e.id).collect::<Vec<_>>(), [1, 3, 2]);
    assert_eq!(all[0].group_key, "notion:FL-1");
    assert_eq!(all[2].data, Some(serde_json::json!({"exit": 1})));

    let f = EventFilter {
        since: Some(ts("2026-09-08T10:15:00Z")),
        ..Default::default()
    };
    assert_eq!(l.call_blocking(move |c| events::list(c, &f)).unwrap().len(), 1);
    let f = EventFilter {
        run: Some("aaaaaaaa".into()),
        ..Default::default()
    };
    assert_eq!(l.call_blocking(move |c| events::list(c, &f)).unwrap().len(), 2);
    let f = EventFilter {
        group: Some("other".into()),
        ..Default::default()
    };
    assert_eq!(l.call_blocking(move |c| events::list(c, &f)).unwrap().len(), 1);
    let f = EventFilter {
        kinds: vec![event_kind::RUN_FAILED.into()],
        ..Default::default()
    };
    assert_eq!(l.call_blocking(move |c| events::list(c, &f)).unwrap()[0].id, 2);
    let f = EventFilter {
        limit: Some(1),
        ..Default::default()
    };
    assert_eq!(l.call_blocking(move |c| events::list(c, &f)).unwrap().len(), 1);
}

#[test]
fn presence_touch_returns_previous() {
    let l = Ledger::open_in_memory().unwrap();
    let t1 = ts("2026-09-08T10:00:00Z");
    let t2 = ts("2026-09-08T18:00:00Z");
    assert_eq!(l.call_blocking(move |c| presence::touch(c, "tui", t1)).unwrap(), None);
    assert_eq!(
        l.call_blocking(move |c| presence::touch(c, "tui", t2)).unwrap(),
        Some(t1)
    );
    assert_eq!(l.call_blocking(|c| presence::get(c, "tui")).unwrap(), Some(t2));
    assert_eq!(l.call_blocking(|c| presence::get(c, "phone")).unwrap(), None);
    assert_eq!(l.call_blocking(|c| presence::latest(c)).unwrap(), Some(t2));
}

#[test]
fn file_ledger_survives_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("ledger.db");
    {
        let l = Ledger::open(&path).unwrap();
        let r = run("aaaaaaaa", "2026-09-08T10:00:00Z");
        l.call_blocking(move |c| runs::insert(c, &r)).unwrap();
    }
    let l = Ledger::open(&path).unwrap();
    assert!(l.call_blocking(|c| runs::get(c, "aaaaaaaa")).unwrap().is_some());
    // Two handles on one file (daemon + a CLI test) coexist under WAL.
    let l2 = Ledger::open(&path).unwrap();
    let r = run("bbbbbbbb", "2026-09-08T11:00:00Z");
    l2.call_blocking(move |c| runs::insert(c, &r)).unwrap();
    assert_eq!(
        l.call_blocking(|c| runs::list(c, &RunFilter::default())).unwrap().len(),
        2
    );
}
