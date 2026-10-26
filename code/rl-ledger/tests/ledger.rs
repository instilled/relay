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
fn a_rename_by_hand_is_undone_back_to_relays_own_name() {
    let l = Ledger::open_in_memory().unwrap();
    for id in ["asked001", "early001", "given001"] {
        let r = run(id, "2026-09-08T10:00:00Z");
        l.call_blocking(move |c| runs::insert(c, &r)).unwrap();
    }
    for id in ["asked001", "early001"] {
        l.call_blocking(move |c| runs::set_title_auto(c, id)).unwrap();
    }
    let title = |id: &'static str| l.call_blocking(move |c| runs::get(c, id)).unwrap().unwrap().title;
    let rename = |id: &'static str, t: &'static str| l.call_blocking(move |c| runs::set_title(c, id, t)).unwrap();
    let restore = |id: &'static str| l.call_blocking(move |c| runs::restore_title(c, id)).unwrap();
    let name_once = |id: &'static str, t: &'static str| l.call_blocking(move |c| runs::name_once(c, id, t)).unwrap();

    // Named for its first prompt, then by hand: back to the prompt's name.
    assert!(name_once("asked001", "claude: one"));
    rename("asked001", "mine");
    assert!(restore("asked001"));
    assert_eq!(title("asked001"), "claude: one");
    assert!(!name_once("asked001", "claude: two"), "named once is once");

    // Named by hand before it was asked anything: back to what it started
    // as, and the first prompt still names it.
    rename("early001", "mine");
    assert!(!name_once("early001", "claude: one"), "the rename stands");
    assert!(restore("early001"));
    assert_eq!(title("early001"), "make test");
    assert!(name_once("early001", "claude: two"));
    assert_eq!(title("early001"), "claude: two");

    // Titled by whoever started it: that is the name it goes back to.
    rename("given001", "mine");
    assert!(restore("given001"));
    assert_eq!(title("given001"), "make test");
    assert!(!name_once("given001", "claude: one"));

    assert!(!restore("nope0000"));
}

#[test]
fn a_hidden_run_is_kept_as_it_was_and_hiding_again_moves_the_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.db");
    let l = Ledger::open(&path).unwrap();
    let r = run("hidden01", "2026-09-08T10:00:00Z");
    l.call_blocking(move |c| runs::insert(c, &r)).unwrap();
    l.call_blocking(|c| runs::set_hidden(c, "hidden01", Some(ts("2026-09-08T10:05:00Z"))))
        .unwrap();
    // Unlike a pin, hiding again takes the new time: whatever came through
    // since, because it needed you, goes back.
    l.call_blocking(|c| runs::set_hidden(c, "hidden01", Some(ts("2026-09-08T11:00:00Z"))))
        .unwrap();
    drop(l);
    // Kept across a reopen, and nothing else about the run moved.
    let l = Ledger::open(&path).unwrap();
    let got = l.call_blocking(|c| runs::get(c, "hidden01")).unwrap().unwrap();
    assert_eq!(got.hidden_at, Some(ts("2026-09-08T11:00:00Z")));
    assert_eq!(got.state, RunState::Starting);
    assert_eq!(got.archived_at, None);
    l.call_blocking(|c| runs::set_hidden(c, "hidden01", None)).unwrap();
    let got = l.call_blocking(|c| runs::get(c, "hidden01")).unwrap().unwrap();
    assert_eq!(got.hidden_at, None);
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

fn recap_of(run: &str, by: &str, at: &str, text: &str) -> rl_proto::types::Summary {
    rl_proto::types::Summary {
        run: run.into(),
        agent: "claude".into(),
        by: by.into(),
        at: ts(at),
        text: text.into(),
        since: None,
        source: if by.is_empty() { "claude recap" } else { "summarised" }.into(),
        reach: "t:10".into(),
        stale: false,
        target: String::new(),
        deep: false,
        from: None,
        calls: 0,
        runs: 1,
    }
}

#[test]
fn a_later_seed_never_replaces_a_written_recap() {
    use rl_ledger::{seeds, summaries};
    let l = Ledger::open_in_memory().unwrap();
    l.call_blocking(|c| {
        runs::insert(c, &run("r9", "2026-09-08T10:00:00Z"))?;
        summaries::put(
            c,
            &recap_of("r9", "r9", "2026-09-08T11:00:00Z", "Written by an assistant."),
        )?;
        assert!(seeds::put_newer(
            c,
            &recap_of("r9", "", "2026-09-08T11:01:00Z", "The agent's own words.")
        )?);
        assert_eq!(summaries::get(c, "r9")?.unwrap().text, "Written by an assistant.");
        let ids = ["r9".to_string()];
        assert_eq!(summaries::lines(c, &ids)?["r9"], "Written by an assistant.");
        assert_eq!(seeds::get(c, "r9")?.unwrap().text, "The agent's own words.");
        // An older seed does not replace a newer one.
        assert!(!seeds::put_newer(
            c,
            &recap_of("r9", "", "2026-09-08T10:30:00Z", "Older.")
        )?);
        Ok(())
    })
    .unwrap();
}

#[test]
fn lines_fall_back_to_the_seed_and_strip_the_label() {
    use rl_ledger::{seeds, summaries};
    let l = Ledger::open_in_memory().unwrap();
    l.call_blocking(|c| {
        for id in ["r1", "r2"] {
            runs::insert(c, &run(id, "2026-09-08T10:00:00Z"))?;
        }
        seeds::put_newer(
            c,
            &recap_of("r1", "", "2026-09-08T11:00:00Z", "\nFixed the search.\nMore."),
        )?;
        summaries::put(
            c,
            &recap_of(
                "r2",
                "a1",
                "2026-09-08T11:00:00Z",
                "Worked on: the parser\nOpen questions: none\nNext: tests",
            ),
        )?;
        let ids = ["r1".to_string(), "r2".to_string()];
        let got = summaries::lines(c, &ids)?;
        assert_eq!(got["r1"], "Fixed the search.");
        assert_eq!(got["r2"], "the parser");
        Ok(())
    })
    .unwrap();
}
