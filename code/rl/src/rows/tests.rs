use super::*;
use crate::sources::{Attach, Source};

fn t(s: &str) -> Timestamp {
    s.parse().unwrap()
}

/// A Relay-held agent session called `id`, in `cwd`, started at `at`.
fn row(id: &str, state: RunState, cwd: &str, at: &str) -> Row {
    Row {
        key: format!("r:{id}"),
        source: Source::Held,
        harness: "gemini".into(),
        name: id.into(),
        cwd: cwd.into(),
        state,
        words: state_words(state, Confidence::Exact),
        summary: None,
        started: Some(t(at)),
        ended: None,
        state_at: Some(t(at)),
        pr: None,
        attach: Attach::Held(id.into()),
        stop: None,
        kind: RunKind::Agent,
        repo: crate::git::toplevel(Path::new(cwd)),
    }
}

fn of_kind(kind: RunKind, r: Row) -> Row {
    Row { kind, ..r }
}

/// One of Claude's own background sessions.
fn native(id: &str, state: RunState, cwd: &str, at: &str) -> Row {
    Row {
        key: format!("n:claude:{id}"),
        source: Source::Native,
        harness: "claude".into(),
        attach: Attach::None,
        ..row(id, state, cwd, at)
    }
}

/// A Codex session in another terminal, as its hooks told of it.
fn hooked(sid: &str, state: RunState, cwd: &str, at: &str) -> Row {
    Row {
        key: format!("o:codex:{sid}"),
        source: Source::Hooked,
        harness: "codex".into(),
        started: None,
        attach: Attach::None,
        ..row(sid, state, cwd, at)
    }
}

/// The prefs, with these folders named.
fn prefs(named: &[(&str, &str)]) -> ListPrefs {
    ListPrefs {
        names: named.iter().map(|(p, n)| (format!("g:{p}"), n.to_string())).collect(),
        ..ListPrefs::default()
    }
}

fn hid(key: &str, at: &str) -> Hidden {
    Hidden {
        key: key.into(),
        at: t(at),
    }
}

fn keys(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(|l| match l {
            Line::Group(h) => format!("[{}]", h.name),
            Line::Session(r) => r.key.clone(),
        })
        .collect()
}

fn build_with<'a>(rows: &'a [Row], prefs: &ListPrefs, filter: &str) -> Vec<Line<'a>> {
    build_in(rows, prefs, filter, &Checkouts::new())
}

fn build_in<'a>(rows: &'a [Row], prefs: &ListPrefs, filter: &str, checkouts: &Checkouts) -> Vec<Line<'a>> {
    build(
        rows,
        &Context {
            prefs,
            filter,
            checkouts,
        },
    )
}

/// The list with the toggles `(agents, shells, hidden)`.
fn toggled<'a>(rows: &'a [Row], prefs: &ListPrefs, (a, s, h): (bool, bool, bool)) -> Vec<Line<'a>> {
    let p = ListPrefs {
        show_agents: a,
        show_shells: s,
        show_hidden: h,
        ..prefs.clone()
    };
    build_with(rows, &p, "")
}

/// The header of the group called `name`, among the built lines.
fn head_of<'a>(lines: &'a [Line], name: &str) -> &'a Head {
    lines
        .iter()
        .find_map(|l| match l {
            Line::Group(h) if h.name == name => Some(h),
            _ => None,
        })
        .expect("a header")
}

const API: &str = "/home/f/api";
const RELAY: &str = "/home/f/relay";
const WEB: &str = "/home/f/web";

#[test]
fn a_search_narrows_by_words_and_by_what_things_are() {
    let mut agent = row("agent", RunState::Working, API, "2026-09-10T11:00:00Z");
    agent.name = "fix the login".into();
    let mut failed = of_kind(
        RunKind::Command,
        row("failed", RunState::Failed, WEB, "2026-09-10T11:00:00Z"),
    );
    failed.name = "cargo test".into();
    let rows = [agent, failed];
    let p = prefs(&[(API, "api"), (WEB, "web"), ("/home/f/idle", "idle")]);
    let shown = |f: &str| keys(&build_with(&rows, &p, f));
    // Every word somewhere: the title and the project both count.
    assert_eq!(shown("login api"), ["[api]", "r:agent"]);
    assert_eq!(shown("login web"), Vec::<String>::new());
    // A project's name keeps all of it; an idle project answers to its name.
    assert_eq!(shown("in:api"), ["[api]", "r:agent"]);
    assert_eq!(shown("idle"), ["[idle]"]);
    // Tokens pick by what a thing is, and take no bare project along.
    assert_eq!(shown("is:failed"), ["[web]", "r:failed"]);
    assert_eq!(shown("kind:agent"), ["[api]", "r:agent"]);
    assert_eq!(shown("kind:command"), ["[web]", "r:failed"]);
    // A word nothing says finds nothing.
    assert_eq!(shown("global"), Vec::<String>::new());
}

#[test]
fn a_search_lands_on_the_project_it_names_not_what_mentions_it() {
    let at = "2026-09-10T11:00:00Z";
    let mut pinned = row("pinned", RunState::Working, API, at);
    pinned.summary = Some("waiting on web to deploy".into());
    let mut deploy = row("deploy", RunState::Working, API, at);
    deploy.name = "web deploy".into();
    let rows = [pinned, deploy, row("w1", RunState::Working, WEB, at)];
    let mut p = prefs(&[(API, "api"), (WEB, "web")]);
    p.pins = vec!["r:pinned".into()];
    let names = Names::new(&p.names);
    let best = |f: &str| {
        let lines = build_with(&rows, &p, f);
        let named: Vec<Named> = lines.iter().map(|l| named(l, &names)).collect();
        let list = keys(&lines);
        let order = crate::query::ranked(&Query::parse(f).words, &named);
        let order: Vec<String> = order.iter().map(|&i| list[i].clone()).collect();
        (list, order)
    };
    let (list, order) = best("web");
    assert_eq!(
        list,
        ["r:pinned", "[api]", "r:deploy", "[web]", "r:w1"],
        "the list keeps its order"
    );
    assert_eq!(order, ["[web]", "r:deploy", "r:w1", "r:pinned"]);
    assert_eq!(best("deploy").1, ["r:deploy", "r:pinned"], "a title before a summary");
}

#[test]
fn a_row_goes_by_the_name_you_gave_it() {
    let rows = [row("api-gemini", RunState::Working, API, "2026-09-10T11:00:00Z")];
    let mut p = prefs(&[]);
    p.names.insert("r:api-gemini".into(), "the big refactor".into());
    let names = Names::new(&p.names);
    assert_eq!(names.title(&rows[0]), "the big refactor");
    assert_eq!(keys(&build_with(&rows, &p, "big")), ["[api]", "r:api-gemini"]);
    let lines = build_with(&rows, &p, "");
    assert_eq!(named(&lines[1], &names).title, "the big refactor");
}

#[test]
fn the_toggles_keep_a_row_by_what_it_is() {
    let rows = [
        row("agent", RunState::Working, RELAY, "2026-09-10T11:00:00Z"),
        of_kind(
            RunKind::Shell,
            row("shell", RunState::Working, RELAY, "2026-09-10T11:01:00Z"),
        ),
        of_kind(
            RunKind::Command,
            row("cmd", RunState::Working, RELAY, "2026-09-10T11:02:00Z"),
        ),
        native("bg", RunState::Working, RELAY, "2026-09-10T11:03:00Z"),
    ];
    let p = prefs(&[(RELAY, "relay")]);
    let shown = |tg| -> Vec<String> {
        toggled(&rows, &p, tg)
            .iter()
            .filter_map(|l| l.row().map(|r| r.key.clone()))
            .collect()
    };
    assert_eq!(
        shown((true, true, false)),
        ["r:agent", "r:shell", "r:cmd", "n:claude:bg"]
    );
    assert_eq!(
        shown((false, true, false)),
        ["r:shell", "r:cmd"],
        "agents off: Relay's and the harnesses' own alike"
    );
    assert_eq!(shown((true, false, false)), ["r:agent", "r:cmd", "n:claude:bg"]);
    assert_eq!(shown((false, false, false)), ["r:cmd"], "a command is always shown");
}

#[test]
fn pinned_rows_stand_above_everything_whatever_the_toggles() {
    let rows = [
        of_kind(
            RunKind::Shell,
            row("shell", RunState::Working, RELAY, "2026-09-10T11:00:00Z"),
        ),
        row("fail1", RunState::Failed, RELAY, "2026-09-10T11:30:00Z"),
        row("old01", RunState::Finished, API, "2026-09-01T10:00:00Z"),
    ];
    let mut p = prefs(&[(RELAY, "relay"), (API, "api")]);
    p.pins = vec!["r:old01".into(), "r:shell".into()];
    // In the order they were pinned, above the projects; a week-old row
    // stays, and its folder keeps its place by name.
    assert_eq!(
        keys(&build_with(&rows, &p, "")),
        ["r:old01", "r:shell", "[api]", "[relay]", "r:fail1"]
    );
    // No toggle takes a pin away.
    assert_eq!(
        keys(&toggled(&rows, &p, (false, false, false)))[..2],
        ["r:old01", "r:shell"]
    );
    // The filter does, and matches a pin by its folder too.
    assert_eq!(keys(&build_with(&rows, &p, "api")), ["r:old01", "[api]"]);
}

#[test]
fn what_you_hid_shows_inline_when_hidden_is_on() {
    let scratch = "/home/f/scratch";
    let rows = [
        row("mine", RunState::Working, RELAY, "2026-09-10T11:00:00Z"),
        row("kept", RunState::Working, RELAY, "2026-09-10T11:05:00Z"),
        row("pinned", RunState::Working, RELAY, "2026-09-10T11:06:00Z"),
        row("inapi", RunState::Finished, API, "2026-09-10T11:00:00Z"),
        row("loose", RunState::Working, scratch, "2026-09-10T11:00:00Z"),
    ];
    let mut p = prefs(&[(API, "api"), (RELAY, "relay")]);
    p.pins = vec!["r:pinned".into()];
    p.hidden = vec![
        hid("r:mine", "2026-09-10T11:10:00Z"),
        hid("r:pinned", "2026-09-10T11:10:00Z"),
        // A named project, a folder that is not one, and a folder with
        // nothing on the list at all.
        hid(&format!("g:{API}"), "2026-09-10T11:10:00Z"),
        hid(&format!("g:{scratch}"), "2026-09-10T11:10:00Z"),
        hid("g:/home/f/gone", "2026-09-10T11:10:00Z"),
    ];
    // Hidden off, the default: what you hid is off the list — the project's
    // header too, the pin too. Nothing hidden needs you, so nothing comes
    // through. `kept` is an agent, so agents off takes it away.
    assert_eq!(keys(&toggled(&rows, &p, (true, true, false))), ["[relay]", "r:kept"]);
    assert_eq!(keys(&toggled(&rows, &p, (false, true, false))), ["[relay]"]);
    // Hidden on, the hidden rows stand inline in their places, alongside
    // the kept ones: a hidden project's header, named or not, even with
    // nothing under it; a hidden pin in the pinned band.
    assert_eq!(
        keys(&toggled(&rows, &p, (true, true, true))),
        [
            "r:pinned",
            "[api]",
            "r:inapi",
            "[gone]",
            "[relay]",
            "r:mine",
            "r:kept",
            "[scratch]",
            "r:loose",
        ]
    );
    // The search reads the word too: with hidden on, `is:hidden` keeps only
    // what is hidden, by its own hide or its project's.
    let on = ListPrefs {
        show_hidden: true,
        ..p.clone()
    };
    assert_eq!(
        keys(&build_with(&rows, &on, "is:hidden")),
        [
            "r:pinned",
            "[api]",
            "r:inapi",
            "[relay]",
            "r:mine",
            "[scratch]",
            "r:loose"
        ]
    );
}

#[test]
fn what_you_hid_comes_through_when_it_comes_to_need_you() {
    let stood = |mut r: Row, at: &str| {
        r.state_at = Some(t(at));
        r
    };
    let rows = [
        // Hidden while it worked, and failed after: it comes through.
        stood(
            row("failed", RunState::Failed, RELAY, "2026-09-10T09:00:00Z"),
            "2026-09-10T11:00:00Z",
        ),
        // Hidden once it had failed: that was you saying so; it stays away.
        stood(
            row("seen", RunState::Failed, RELAY, "2026-09-10T09:10:00Z"),
            "2026-09-10T11:00:00Z",
        ),
        // In a hidden project: one that asks after comes through, one still
        // working does not.
        stood(
            row("asks", RunState::NeedsInput, API, "2026-09-10T09:20:00Z"),
            "2026-09-10T10:30:00Z",
        ),
        row("busy", RunState::Working, API, "2026-09-10T09:30:00Z"),
    ];
    let mut p = prefs(&[(API, "api"), (RELAY, "relay")]);
    p.hidden = vec![
        hid("r:failed", "2026-09-10T10:00:00Z"),
        hid("r:seen", "2026-09-10T11:30:00Z"),
        hid(&format!("g:{API}"), "2026-09-10T10:00:00Z"),
    ];
    let all = build_with(&rows, &p, "");
    // Above the projects, what asks before what failed; its project stays
    // hidden. What stays hidden and needs you is hinted on its project.
    assert_eq!(keys(&all), ["r:asks", "r:failed", "[relay]"]);
    assert_eq!(head_of(&all, "relay").attention, 1);
    assert_eq!(counts(&all).0, 2, "the headline counts them as needing you");
    // A toggle still narrows it, as it would any row that needs you.
    let off = toggled(&rows, &p, (false, true, false));
    assert_eq!(keys(&off), ["[relay]"]);
    assert_eq!(
        head_of(&off, "relay").attention,
        2,
        "what would come through is still hinted on its project"
    );
    // With hidden on they are in their places, not above them.
    assert_eq!(
        keys(&toggled(&rows, &p, (true, true, true))),
        ["[api]", "r:asks", "r:busy", "[relay]", "r:failed", "r:seen"]
    );
    // Hiding it again is how it goes back: the time moves on.
    let mut again = p.clone();
    again.hidden[0].at = t("2026-09-10T11:45:00Z");
    let lines = build_with(&rows, &again, "");
    assert_eq!(keys(&lines), ["r:asks", "[relay]"]);
    assert_eq!(head_of(&lines, "relay").attention, 2);

    let api = Path::new(API);
    assert_eq!(hidden_since(&rows[2], api, &p.hidden), Some(t("2026-09-10T10:00:00Z")));
    assert_eq!(hidden_with(api, &p.hidden), Some("its project"));
    assert_eq!(hidden_with(Path::new(RELAY), &p.hidden), None);
    assert!(comes_through(&rows[2], t("2026-09-10T10:00:00Z")));
    assert!(!comes_through(&rows[2], t("2026-09-10T10:30:00Z")), "not after, at");
}

#[test]
fn projects_go_by_name_and_rows_as_they_came() {
    let rows = [
        row("work1", RunState::Working, API, "2026-09-10T11:50:00Z"),
        row("fail1", RunState::Failed, RELAY, "2026-09-10T11:00:00Z"),
        row("blok1", RunState::Blocked, API, "2026-09-10T11:55:00Z"),
        row("done1", RunState::Finished, RELAY, "2026-09-10T10:00:00Z"),
    ];
    let p = prefs(&[(RELAY, "relay")]);
    let lines = build_with(&rows, &p, "");
    assert_eq!(
        keys(&lines),
        ["[api]", "r:work1", "r:blok1", "[relay]", "r:done1", "r:fail1"],
        "projects by name, however urgent; rows as they came; api is named by its folder"
    );
    assert_eq!(counts(&lines), (2, 1, 1));
    let h = head_of(&lines, "relay");
    assert_eq!((h.place.as_str(), h.key.as_str()), (RELAY, "g:/home/f/relay"));
    assert_eq!(headline(&lines, ""), "relay · 2 need you · 1 working · 1 finished");

    let lines = build_with(&rows, &p, "api");
    assert_eq!(keys(&lines), ["[api]", "r:work1", "r:blok1"]);
    assert_eq!(headline(&lines, "api"), "relay · 1 needs you · 1 working · ⌕ api");
    assert_eq!(headline(&[], ""), "relay · nothing running");
}

#[test]
fn a_named_folder_shows_even_with_nothing_in_it() {
    let rows = [row("work1", RunState::Working, RELAY, "2026-09-10T11:50:00Z")];
    let p = prefs(&[(RELAY, "relay"), ("/home/f/idle", "idle")]);
    // The idle project is a line of its own, in its place by name.
    assert_eq!(keys(&build_with(&rows, &p, "")), ["[idle]", "[relay]", "r:work1"]);
    // It carries the folder so selecting it lands on the project.
    match build_with(&rows, &p, "idle").last() {
        Some(Line::Group(h)) => {
            assert_eq!(h.name, "idle");
            assert_eq!(h.workspace, Path::new("/home/f/idle"));
        }
        _ => panic!("the idle folder is the last line it filters to"),
    }
    assert_eq!(keys(&build_with(&rows, &p, "idle")), ["[idle]"]);
    // A toggle narrows what is inside projects, never which projects there
    // are: the idle one and yours stay, even where nothing in them matches.
    assert_eq!(keys(&toggled(&rows, &p, (false, true, false))), ["[idle]", "[relay]"]);
}

#[test]
fn with_no_rows_and_no_folders_the_list_is_empty() {
    let lines = build_with(&[], &ListPrefs::default(), "");
    assert!(lines.is_empty());
    assert_eq!(headline(&lines, ""), "relay · nothing running");
}

#[test]
fn sessions_stand_under_their_folders_project() {
    let rows = [
        row("work", RunState::Working, RELAY, "2026-09-10T11:30:00Z"),
        // In the project's folder, and deeper in it — a worktree there.
        native("bg", RunState::Working, RELAY, "2026-09-10T11:00:00Z"),
        native(
            "deep",
            RunState::Idle,
            "/home/f/relay/.claude/worktrees/x",
            "2026-09-10T11:10:00Z",
        ),
        // Outside every named folder: its own folder's group.
        hooked("away", RunState::Working, "/home/f/scratch", "2026-09-10T11:20:00Z"),
    ];
    let p = prefs(&[(RELAY, "relay")]);
    let lines = build_with(&rows, &p, "");
    assert_eq!(
        keys(&lines),
        [
            "[relay]",
            "n:claude:bg",
            "n:claude:deep",
            "r:work",
            "[scratch]",
            "o:codex:away"
        ]
    );
    assert_eq!(head_of(&lines, "scratch").workspace, PathBuf::from("/home/f/scratch"));
}

#[test]
fn a_session_with_no_start_stands_after_those_with_one() {
    let rows = [
        hooked("h", RunState::Working, RELAY, "2026-09-10T09:00:00Z"),
        row("late", RunState::Working, RELAY, "2026-09-10T11:00:00Z"),
        row("early", RunState::Working, RELAY, "2026-09-10T10:00:00Z"),
    ];
    let p = prefs(&[(RELAY, "relay")]);
    assert_eq!(
        keys(&build_with(&rows, &p, "")),
        ["[relay]", "r:early", "r:late", "o:codex:h"]
    );
}

#[test]
fn a_hidden_session_comes_through_when_it_comes_to_need_you() {
    let rows = [
        // Blocked after its project was hidden: comes through.
        native("after", RunState::Blocked, RELAY, "2026-09-10T11:30:00Z"),
        // Blocked before: stays hidden.
        native("before", RunState::Blocked, RELAY, "2026-09-10T10:30:00Z"),
        // At work after: nothing needs you.
        hooked("busy", RunState::Working, RELAY, "2026-09-10T11:30:00Z"),
    ];
    let mut p = prefs(&[]);
    p.hidden = vec![hid(&format!("g:{RELAY}"), "2026-09-10T11:00:00Z")];
    assert_eq!(keys(&build_with(&rows, &p, "")), ["n:claude:after"]);
    // Its own hide, later than it came to need you: hidden again.
    p.hidden.push(hid("n:claude:after", "2026-09-10T11:45:00Z"));
    assert!(keys(&build_with(&rows, &p, "")).is_empty());
    assert_eq!(
        keys(&toggled(&rows, &p, (true, true, true))),
        ["[relay]", "n:claude:before", "n:claude:after", "o:codex:busy"]
    );
}

#[test]
fn a_session_hides_by_its_own_key_or_its_projects() {
    let rows = [native("bg", RunState::Working, RELAY, "2026-09-10T11:00:00Z")];
    for key in ["n:claude:bg".to_string(), format!("g:{RELAY}")] {
        let mut p = prefs(&[(RELAY, "relay")]);
        p.hidden = vec![hid(&key, "2026-09-10T11:00:00Z")];
        assert!(
            !keys(&build_with(&rows, &p, "")).contains(&"n:claude:bg".to_string()),
            "{key}"
        );
        assert_eq!(
            keys(&toggled(&rows, &p, (true, true, true))),
            ["[relay]", "n:claude:bg"],
            "{key}"
        );
    }
}

#[test]
fn a_search_finds_the_agents_own_sessions() {
    let mut bg = native("bg", RunState::Blocked, RELAY, "2026-09-10T11:00:00Z");
    bg.summary = Some("asks about the schema".into());
    let rows = [
        row("work", RunState::Working, RELAY, "2026-09-10T11:30:00Z"),
        bg,
        hooked("away", RunState::Idle, "/home/f/scratch", "2026-09-10T11:20:00Z"),
    ];
    let p = prefs(&[(RELAY, "relay")]);
    let shown = |f: &str| keys(&build_with(&rows, &p, f));
    assert_eq!(shown("kind:claude"), ["[relay]", "n:claude:bg"]);
    assert_eq!(shown("kind:codex"), ["[scratch]", "o:codex:away"]);
    assert_eq!(shown("kind:agent in:relay"), ["[relay]", "n:claude:bg", "r:work"]);
    assert_eq!(shown("is:needs"), ["[relay]", "n:claude:bg"]);
    assert_eq!(shown("is:going kind:codex"), ["[scratch]", "o:codex:away"]);
    assert_eq!(shown("schema"), ["[relay]", "n:claude:bg"], "by its summary");
    assert_eq!(shown("scratch"), ["[scratch]", "o:codex:away"]);
    let kinds = list_kinds();
    for k in ["agent", "shell", "claude", "codex", "gemini"] {
        assert!(kinds.iter().any(|x| x == k), "{k}: {kinds:?}");
    }
}

#[test]
fn by_state_the_rows_stand_under_needs_input_working_and_done() {
    let rows = [
        row("a", RunState::Working, API, "2026-09-10T10:00:00Z"),
        row("b", RunState::NeedsInput, RELAY, "2026-09-10T10:05:00Z"),
        row("c", RunState::Finished, API, "2026-09-10T09:00:00Z"),
        row("d", RunState::Failed, RELAY, "2026-09-10T10:10:00Z"),
        of_kind(RunKind::Shell, row("e", RunState::Idle, RELAY, "2026-09-10T09:30:00Z")),
        native("f", RunState::Blocked, API, "2026-09-10T09:50:00Z"),
        row("g", RunState::Stopped, API, "2026-09-10T08:00:00Z"),
        row("pin", RunState::Working, API, "2026-09-10T07:00:00Z"),
    ];
    let mut p = prefs(&[(API, "api"), ("/home/f/idle", "idle")]);
    p.pins = vec!["r:pin".into()];
    p.grouping = Grouping::State;
    let lines = build_with(&rows, &p, "");
    // The pins first, as ever; then each heading, its rows oldest first.
    // No project headers, not even a named folder's.
    assert_eq!(
        keys(&lines),
        [
            "r:pin",
            "[Needs input]",
            "n:claude:f",
            "r:b",
            "[Working]",
            "r:a",
            "[Done]",
            "r:g",
            "r:c",
            "r:e",
            "r:d",
        ]
    );
    assert_eq!(head_of(&lines, "Working").key, "s:working");
    // A heading with nothing under it is left out.
    assert_eq!(
        keys(&build_with(&rows, &p, "is:needs")),
        ["[Needs input]", "n:claude:f", "r:b"]
    );
    // What needs you and is off the list is hinted on its heading, which
    // stands for it alone if it must.
    p.show_agents = false;
    let lines = build_with(&rows, &p, "");
    assert_eq!(keys(&lines), ["r:pin", "[Needs input]", "[Done]", "r:e"]);
    assert_eq!(head_of(&lines, "Needs input").attention, 2);
    assert_eq!(head_of(&lines, "Done").attention, 1, "it failed");
    // Grouped by state, nothing moves.
    p.show_agents = true;
    let lines = build_with(&rows, &p, "");
    for key in ["r:b", "s:working"] {
        assert_eq!(
            reorder(&lines, &lines, &p.order, &p.pins, key, true),
            Err("by state, rows stand oldest first · ^s groups them by project"),
            "{key}"
        );
    }
    // `^s` back: the same rows by project.
    p.grouping = Grouping::Project;
    assert_eq!(keys(&build_with(&rows, &p, ""))[..3], ["r:pin", "[api]", "r:g"]);
}

#[test]
fn a_project_hints_when_something_hidden_needs_you() {
    let rows = [
        row("failed", RunState::Failed, RELAY, "2026-09-10T11:00:00Z"),
        row("work", RunState::Working, RELAY, "2026-09-10T11:40:00Z"),
    ];
    let mut p = prefs(&[(RELAY, "relay")]);
    // Hidden once it had already failed, so it stays off the list and does
    // not come through — it is genuinely buried.
    p.hidden = vec![hid("r:failed", "2026-09-10T11:30:00Z")];
    let lines = build_with(&rows, &p, "");
    assert!(!keys(&lines).contains(&"r:failed".to_string()));
    assert_eq!(head_of(&lines, "relay").attention, 1);
    // Hidden on: it stands inline, so nothing is buried and no hint.
    let shown = toggled(&rows, &p, (true, true, true));
    assert_eq!(head_of(&shown, "relay").attention, 0);
}

#[test]
fn a_project_with_only_buried_rows_still_has_its_header_to_say_so() {
    let rows = [row(
        "asks",
        RunState::NeedsInput,
        "/home/f/scratch",
        "2026-09-10T11:00:00Z",
    )];
    let lines = toggled(&rows, &ListPrefs::default(), (false, true, false));
    assert_eq!(keys(&lines), ["[scratch]"]);
    assert_eq!(head_of(&lines, "scratch").attention, 1);
}

#[test]
fn agents_off_hints_a_needy_agent_on_its_project() {
    let rows = [row("ask", RunState::NeedsInput, RELAY, "2026-09-10T11:50:00Z")];
    let p = prefs(&[(RELAY, "relay")]);
    let on = toggled(&rows, &p, (true, true, false));
    assert!(keys(&on).contains(&"r:ask".to_string()));
    assert_eq!(head_of(&on, "relay").attention, 0);
    let off = toggled(&rows, &p, (false, true, false));
    assert!(!keys(&off).contains(&"r:ask".to_string()));
    assert_eq!(head_of(&off, "relay").attention, 1);
}

#[test]
fn the_hint_says_how_many_need_you() {
    assert_eq!(needs_hint(0), None);
    assert_eq!(needs_hint(1).as_deref(), Some("! 1 needs you"));
    assert_eq!(needs_hint(3).as_deref(), Some("! 3 need you"));
}

#[test]
fn a_shell_is_hidden_when_shells_are_off() {
    let rows = [of_kind(
        RunKind::Shell,
        row("sh", RunState::Working, RELAY, "2026-09-10T11:50:00Z"),
    )];
    let p = prefs(&[(RELAY, "relay")]);
    assert!(keys(&toggled(&rows, &p, (true, true, false))).contains(&"r:sh".to_string()));
    assert!(!keys(&toggled(&rows, &p, (true, false, false))).contains(&"r:sh".to_string()));
}

#[test]
fn shells_off_hints_a_needy_shell_on_its_project() {
    let rows = [of_kind(
        RunKind::Shell,
        row("sh", RunState::Blocked, RELAY, "2026-09-10T11:50:00Z"),
    )];
    let p = prefs(&[(RELAY, "relay")]);
    let on = toggled(&rows, &p, (true, true, false));
    assert!(keys(&on).contains(&"r:sh".to_string()));
    assert_eq!(head_of(&on, "relay").attention, 0);
    let off = toggled(&rows, &p, (true, false, false));
    assert!(!keys(&off).contains(&"r:sh".to_string()));
    assert_eq!(head_of(&off, "relay").attention, 1);
}

#[test]
fn a_row_wears_what_it_is_and_says_how_it_stands() {
    let mut sh = of_kind(RunKind::Shell, row("sh", RunState::Idle, "/w", "2026-09-10T11:00:00Z"));
    assert!(live_shell(&sh));
    assert_eq!(icon(&sh), SHELL);
    // One that ended is news like any other ended row.
    sh.state = RunState::Stopped;
    assert!(!live_shell(&sh));
    // So is one that wants you.
    sh.state = RunState::NeedsInput;
    assert!(!live_shell(&sh));
    let mut a = row("ag", RunState::Idle, "/w", "2026-09-10T11:00:00Z");
    a.kind = RunKind::Command;
    assert_eq!(icon(&a), COMMAND);
}

#[test]
fn an_agent_wears_its_harness_glyph() {
    let mut a = row("ag", RunState::Working, "/w", "2026-09-10T11:00:00Z");
    let mut look = |harness: &str| {
        a.harness = harness.into();
        icon(&a)
    };
    assert_eq!(look("claude"), "✻");
    assert_eq!(look("codex"), "⬡");
    assert_eq!(look("gemini"), "✧");
    // One Relay has no profile for is a plain agent.
    assert_eq!(look("fake-cli"), AGENT);
    use unicode_width::UnicodeWidthStr;
    for l in looks().values() {
        if let Some(i) = &l.icon {
            assert_eq!(i.width(), 1, "{i}");
        }
    }
}

#[test]
fn every_icon_is_one_quiet_column() {
    use unicode_width::UnicodeWidthStr;
    for i in [AGENT, SHELL, COMMAND] {
        assert_eq!(i.width(), 1, "{i}");
        assert_eq!(i.chars().count(), 1, "{i} carries no variation selector");
    }
}

#[test]
fn a_shell_says_where_it_is_from_its_project() {
    let home = std::env::var("HOME").unwrap();
    let project = PathBuf::from(&home).join("Projects/foodai");
    assert_eq!(shell_place(&project.join("app/src"), &project), "app/src");
    assert_eq!(shell_place(&project, &project), "", "at the root, nothing to say");
    assert_eq!(shell_place(&PathBuf::from(&home).join("tmp/x"), &project), "~/tmp/x");
    assert_eq!(shell_place(Path::new(&home), &project), "~");
    assert_eq!(shell_place(Path::new("/etc/nginx"), &project), "/etc/nginx");
    // A sibling that shares the project's start is not inside it.
    assert_eq!(
        shell_place(&PathBuf::from(&home).join("Projects/foodai-old"), &project),
        "~/Projects/foodai-old"
    );
    assert_eq!(
        short_path(&PathBuf::from(format!("{home}2"))),
        format!("{home}2"),
        "not home"
    );
}

#[test]
fn a_header_is_its_path_and_a_name_of_its_own() {
    let p = prefs(&[(API, "api"), ("/home/f/scratch-2", "scratch")]);
    let names = Names::new(&p.names);
    assert_eq!(names.own(Path::new(API)), None, "named for its folder: nothing to add");
    assert_eq!(
        names.own(Path::new("/home/f/scratch-2")).as_deref(),
        Some("scratch"),
        "renamed"
    );
    assert_eq!(names.own(Path::new(WEB)), None, "never named");
    assert_eq!(names.workspace(Path::new("/home/f/scratch-2")), "scratch");
    assert_eq!(names.workspace(Path::new(WEB)), "web");
    assert_eq!(names.containing(Path::new("/home/f/api/src")), Some(PathBuf::from(API)));

    let parts = |place: &str, own: Option<&str>, room: usize| {
        let (a, b, c) = place_parts(place, own, room);
        [a, b, c]
    };
    assert_eq!(parts("~/Projects/foodai", None, 40), ["~/Projects/", "foodai", ""]);
    assert_eq!(
        parts("~/tmp/scratch-2", Some("scratch"), 40),
        ["~/tmp/", "scratch-2", " · scratch"]
    );
    assert_eq!(parts("~", None, 40), ["", "~", ""]);
    assert_eq!(parts("/", None, 40), ["", "/", ""]);
    // Too long: the start goes, a folder at a time; the last stays whole.
    let long = "~/Projects/clients/acme/very/deep/tree/leaf-folder";
    let [a, b, c] = parts(long, None, 30);
    assert_eq!(
        [a.as_str(), b.as_str(), c.as_str()],
        ["…/very/deep/tree/", "leaf-folder", ""]
    );
    assert!(a.chars().count() + b.chars().count() <= 30);
    // With no room for any of the start, the name gives way, then the
    // folder stands alone, uncut, even past the room.
    assert_eq!(parts(long, Some("x"), 14), ["", "leaf-folder", ""]);
    assert_eq!(
        parts("~/a/an-extremely-long-folder-name", None, 10)[1],
        "an-extremely-long-folder-name"
    );
}

#[test]
fn a_shell_stands_where_it_came_as_any_other_row() {
    let rows = [
        row("work", RunState::Working, RELAY, "2026-09-10T11:55:00Z"),
        of_kind(
            RunKind::Shell,
            row("shell", RunState::Idle, RELAY, "2026-09-10T11:20:00Z"),
        ),
        row("asks", RunState::NeedsInput, RELAY, "2026-09-10T11:10:00Z"),
    ];
    let p = prefs(&[(RELAY, "relay")]);
    // Neither where you work nor what asks for you jumps ahead: each in the
    // order it started.
    assert_eq!(
        keys(&build_with(&rows, &p, "")),
        ["[relay]", "r:asks", "r:shell", "r:work"]
    );
}

/// Four rows in relay and one in api.
fn four() -> (Vec<Row>, ListPrefs) {
    let rows = vec![
        row("one", RunState::Working, RELAY, "2026-09-10T10:00:00Z"),
        row("two", RunState::Working, RELAY, "2026-09-10T10:01:00Z"),
        row("three", RunState::Idle, RELAY, "2026-09-10T10:02:00Z"),
        row("four", RunState::Finished, RELAY, "2026-09-10T10:03:00Z"),
        row("there", RunState::Working, API, "2026-09-10T10:00:00Z"),
    ];
    (rows, prefs(&[(API, "api"), (RELAY, "relay")]))
}

fn kept(rows: &[&str]) -> Vec<Order> {
    vec![Order {
        group: "g:/home/f/relay".into(),
        rows: rows.iter().map(|s| s.to_string()).collect(),
    }]
}

/// What becomes of a row later: its state, and when that came.
fn comes_to(rows: &mut [Row], name: &str, state: RunState, at: &str) {
    let r = rows.iter_mut().find(|r| r.name == name).unwrap();
    r.state = state;
    r.state_at = Some(t(at));
}

#[test]
fn a_project_stands_in_the_order_its_rows_came_until_you_put_it_in_yours() {
    let (rows, mut p) = four();
    // Nothing kept: the order they started in, the newest at the bottom,
    // whatever state each is in.
    assert_eq!(
        keys(&build_with(&rows, &p, "")),
        ["[api]", "r:there", "[relay]", "r:one", "r:two", "r:three", "r:four"]
    );
    // An order kept for a project is its order; the other project's own is
    // untouched, and an empty one is no order at all.
    p.order = kept(&["r:one", "r:three", "r:two", "r:four"]);
    assert_eq!(
        keys(&build_with(&rows, &p, "")),
        ["[api]", "r:there", "[relay]", "r:one", "r:three", "r:two", "r:four"]
    );
    p.order = kept(&[]);
    assert_eq!(
        keys(&build_with(&rows, &p, ""))[3..],
        ["r:one", "r:two", "r:three", "r:four"]
    );
    // What has left the list since is no matter: the rest keep theirs.
    p.order = kept(&["r:gone", "r:one", "r:three", "r:two", "r:four"]);
    assert_eq!(
        keys(&build_with(&rows, &p, ""))[3..],
        ["r:one", "r:three", "r:two", "r:four"]
    );
}

#[test]
fn a_row_stays_where_it_stands_whatever_comes_of_it() {
    for (order, stands) in [
        (Vec::new(), ["r:one", "r:two", "r:three", "r:four"]),
        (
            kept(&["r:one", "r:three", "r:two", "r:four"]),
            ["r:one", "r:three", "r:two", "r:four"],
        ),
    ] {
        let (mut rows, mut p) = four();
        p.order = order;
        // It asks, another fails, another blocks, the last finishes: each
        // says so where it is, and none of them moves.
        comes_to(&mut rows, "two", RunState::NeedsInput, "2026-09-10T11:50:00Z");
        comes_to(&mut rows, "four", RunState::Failed, "2026-09-10T11:51:00Z");
        comes_to(&mut rows, "three", RunState::Blocked, "2026-09-10T11:52:00Z");
        comes_to(&mut rows, "one", RunState::Finished, "2026-09-10T11:53:00Z");
        assert_eq!(keys(&build_with(&rows, &p, ""))[3..], stands);
        comes_to(&mut rows, "two", RunState::Working, "2026-09-10T11:55:00Z");
        assert_eq!(keys(&build_with(&rows, &p, ""))[3..], stands);
    }
}

#[test]
fn a_new_row_enters_at_the_bottom() {
    let new = |rows: &mut Vec<Row>, name: &str, at: &str| rows.push(row(name, RunState::Working, RELAY, at));
    let (mut rows, p) = four();
    new(&mut rows, "new", "2026-09-10T11:00:00Z");
    assert_eq!(
        keys(&build_with(&rows, &p, ""))[3..],
        ["r:one", "r:two", "r:three", "r:four", "r:new"]
    );
    // Below your order too, in the order they came.
    let (mut rows, mut p) = four();
    p.order = kept(&["r:one", "r:three", "r:two", "r:four"]);
    new(&mut rows, "newer", "2026-09-10T11:30:00Z");
    new(&mut rows, "new", "2026-09-10T11:00:00Z");
    let lines = build_with(&rows, &p, "");
    assert_eq!(
        keys(&lines)[3..],
        ["r:one", "r:three", "r:two", "r:four", "r:new", "r:newer"]
    );
    // Moved, it takes its place in your order, and the rest keep theirs.
    assert_eq!(
        reorder(&lines, &lines, &p.order, &p.pins, "r:new", true).unwrap().rows,
        ["r:one", "r:three", "r:two", "r:new", "r:four", "r:newer"]
    );
}

#[test]
fn a_row_moves_one_place_inside_its_project_and_no_further() {
    let (mut rows, mut p) = four();
    rows.push(row("pinned", RunState::Working, RELAY, "2026-09-10T11:00:00Z"));
    p.pins = vec!["r:pinned".into()];
    let lines = build_with(&rows, &p, "");
    assert_eq!(
        keys(&lines),
        [
            "r:pinned", "[api]", "r:there", "[relay]", "r:one", "r:two", "r:three", "r:four"
        ]
    );
    let mv = |key: &str, up: bool| reorder(&lines, &lines, &p.order, &p.pins, key, up);
    // From the list's own order, the whole of it is kept.
    assert_eq!(
        mv("r:two", true).unwrap(),
        Order {
            group: "g:/home/f/relay".into(),
            rows: ["r:two", "r:one", "r:three", "r:four"].map(String::from).to_vec(),
        }
    );
    assert_eq!(
        mv("r:one", false).unwrap().rows,
        ["r:two", "r:one", "r:three", "r:four"]
    );
    // Never out of its project, nor into the band.
    assert_eq!(mv("r:one", true), Err("already at the top of its project"));
    assert_eq!(mv("r:four", false), Err("already at the bottom of its project"));
    assert_eq!(mv("r:there", false), Err("already at the bottom of its project"));
    assert_eq!(mv("r:there", true), Err("already at the top of its project"));
    assert!(mv("r:pinned", false).unwrap_err().starts_with("a pinned row"));
    assert_eq!(mv("r:nope", true), Err("nothing here to move"));

    // What needs you moves as anything else, and is passed as anything else.
    let (mut rows, mut p) = four();
    p.order = kept(&["r:one", "r:three", "r:two", "r:four"]);
    comes_to(&mut rows, "two", RunState::Blocked, "2026-09-10T11:00:00Z");
    let lines = build_with(&rows, &p, "");
    assert_eq!(keys(&lines)[3..], ["r:one", "r:three", "r:two", "r:four"]);
    assert_eq!(
        reorder(&lines, &lines, &p.order, &p.pins, "r:two", true).unwrap().rows,
        ["r:one", "r:two", "r:three", "r:four"]
    );
    assert_eq!(
        reorder(&lines, &lines, &p.order, &p.pins, "r:four", true).unwrap().rows,
        ["r:one", "r:three", "r:four", "r:two"]
    );
}

#[test]
fn a_project_moves_up_and_down_among_the_projects_and_keeps_its_place() {
    let (api, relay, web) = ("g:/home/f/api", "g:/home/f/relay", "g:/home/f/web");
    let (rows, _) = four();
    let named = |more: &[(&str, &str)]| {
        let mut all = vec![(API, "api"), (RELAY, "relay"), (WEB, "web")];
        all.extend_from_slice(more);
        prefs(&all)
    };
    let heads = |p: &ListPrefs| -> Vec<String> {
        keys(&build_with(&rows, p, ""))
            .into_iter()
            .filter(|k| k.starts_with('['))
            .collect()
    };
    let mut three = named(&[]);
    // By name until you move one.
    assert_eq!(heads(&three), ["[api]", "[relay]", "[web]"]);
    let lines = build_with(&rows, &three, "");
    let mv = |key: &str, up: bool| reorder(&lines, &lines, &three.order, &three.pins, key, up);
    assert_eq!(
        mv(relay, true),
        Ok(Order {
            group: Order::PROJECTS.into(),
            rows: [relay, api, web].map(String::from).to_vec(),
        })
    );
    assert_eq!(mv(relay, false).unwrap().rows, [api, web, relay]);
    assert_eq!(mv(api, true), Err("already the first project"));
    assert_eq!(mv(web, false), Err("already the last project"));

    // Kept, the projects stand in it, each with what is under it.
    let order = vec![Order {
        group: Order::PROJECTS.into(),
        rows: [relay, api, web].map(String::from).to_vec(),
    }];
    three.order = order.clone();
    assert_eq!(
        keys(&build_with(&rows, &three, "")),
        [
            "[relay]", "r:one", "r:two", "r:three", "r:four", "[api]", "r:there", "[web]"
        ]
    );
    // A project the order does not know — new since — enters at the bottom,
    // however its name sorts.
    let mut four_of = named(&[("/home/f/aaa", "aaa")]);
    four_of.order = order;
    assert_eq!(heads(&four_of), ["[relay]", "[api]", "[web]", "[aaa]"]);
    // Moving builds on the order kept, and takes the new one into it.
    let lines = build_with(&rows, &four_of, "");
    assert_eq!(
        reorder(&lines, &lines, &four_of.order, &[], "g:/home/f/aaa", true)
            .unwrap()
            .rows,
        [relay, api, "g:/home/f/aaa", web]
    );
    // One the list does not show now — hidden, or gone — keeps its place.
    three.order[0].rows.insert(1, "g:/home/f/gone".into());
    let lines = build_with(&rows, &three, "");
    assert_eq!(
        reorder(&lines, &lines, &three.order, &[], api, true).unwrap().rows,
        [api, relay, "g:/home/f/gone", web]
    );
    // A search that leaves projects out moves among what it shows.
    let (shown, full) = (build_with(&rows, &three, "in:web"), build_with(&rows, &three, ""));
    assert_eq!(
        reorder(&shown, &full, &three.order, &[], web, true),
        Err("already the first project")
    );
}

#[test]
fn words_and_columns() {
    assert_eq!(state_words(RunState::NeedsInput, Confidence::Exact), "needs you");
    assert_eq!(state_words(RunState::Working, Confidence::Likely), "working~");
    assert_eq!(state_words(RunState::Finished, Confidence::Guess), "done?");
    assert_eq!(fit("abc", 5), "abc  ");
    assert_eq!(fit("abcdef", 5), "abcd…");
    assert_eq!(fit("abcdef", 0), "");
    assert_eq!(fit("héllo wörd", 6), "héllo…");
    // Wide glyphs take two columns each, padded or cut by columns.
    use unicode_width::UnicodeWidthStr;
    assert_eq!(fit("権限", 6), "権限  ");
    assert_eq!(
        fit("権限の確認", 6),
        "権限… ",
        "a glyph that would straddle the cut gives way"
    );
    assert_eq!(fit("権限の確認", 7), "権限の…");
    assert_eq!(fit("a権限の確認", 4), "a権…");
    for w in 0..12 {
        assert_eq!(fit("権限の確認 x", w).width(), w, "{w}");
    }
}

fn co(path: &str, branch: &str, worktrees: usize, badges: &[&str]) -> Checkout {
    Checkout {
        path: path.into(),
        branch: Some(branch.into()),
        detached: None,
        worktrees,
        badges: badges.iter().map(|b| b.to_string()).collect(),
    }
}

#[test]
fn a_header_says_its_folders_branch_worktrees_and_badge() {
    let rows = [
        row("a1", RunState::Working, "/home/f/api", "2026-09-10T11:50:00Z"),
        row("a2", RunState::Working, "/home/f/api", "2026-09-10T11:40:00Z"),
        row("w1", RunState::Working, "/home/f/web", "2026-09-10T11:30:00Z"),
        row("x1", RunState::Working, "/home/f/plain", "2026-09-10T11:20:00Z"),
    ];
    let checkouts: Checkouts = [
        ("/home/f/api".into(), co("/home/f/api", "main", 2, &["PR #95"])),
        ("/home/f/web".into(), co("/home/f/web", "main", 0, &[])),
    ]
    .into();
    let lines = build_in(&rows, &ListPrefs::default(), "", &checkouts);
    let words = |name: &str| {
        head_of(&lines, name)
            .checkout
            .as_ref()
            .map(|c| crate::git::checkout_words(c))
    };
    assert_eq!(words("api").as_deref(), Some("⎇ main · PR #95 · 2 worktrees"));
    assert_eq!(words("web").as_deref(), Some("⎇ main"));
    assert_eq!(words("plain"), None, "a folder outside a repo has no branch line");
}

#[test]
fn on_matches_the_branch_a_projects_folder_is_on() {
    let rows = [
        row("a1", RunState::Working, "/home/f/api", "2026-09-10T11:50:00Z"),
        row("w1", RunState::Working, "/home/f/web", "2026-09-10T11:30:00Z"),
    ];
    let checkouts: Checkouts = [
        ("/home/f/api".into(), co("/home/f/api", "feat/search", 0, &[])),
        ("/home/f/web".into(), co("/home/f/web", "main", 0, &[])),
    ]
    .into();
    let on = |q: &str| keys(&build_in(&rows, &ListPrefs::default(), q, &checkouts));
    assert_eq!(on("on:search"), ["[api]", "r:a1"]);
    assert_eq!(on("on:main"), ["[web]", "r:w1"]);
    assert!(on("on:nope").is_empty());
    // A named folder with nothing in it is found by its branch too.
    let p = prefs(&[("/home/f/web", "web")]);
    let only_api = [rows[0].clone()];
    assert_eq!(keys(&build_in(&only_api, &p, "on:main", &checkouts)), ["[web]"]);
}

#[test]
fn a_row_stands_under_the_named_folder_else_its_repo_root_else_its_own_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    // A repo with folders inside, a linked worktree (its `.git` a file) in
    // another place, and a plain folder.
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join("src/deep")).unwrap();
    let wt = root.join("repo-feat");
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: elsewhere\n").unwrap();
    let plain = root.join("plain/sub");
    std::fs::create_dir_all(&plain).unwrap();
    let at = |id: &str, cwd: &Path| row(id, RunState::Working, cwd.to_str().unwrap(), "2026-09-10T11:50:00Z");
    let rows = [
        at("deep", &repo.join("src/deep")),
        at("top", &repo),
        at("wt", &wt.join("src")),
        at("plain", &plain),
    ];
    let lines = build_with(&rows, &ListPrefs::default(), "");
    assert_eq!(
        keys(&lines),
        ["[repo]", "r:deep", "r:top", "[repo-feat]", "r:wt", "[sub]", "r:plain"],
        "the worktree is its own project; a plain folder is its own"
    );
    assert_eq!(head_of(&lines, "repo").workspace, repo);
    assert_eq!(head_of(&lines, "repo-feat").workspace, wt);
    let mut folders = vec![plain, repo, wt];
    folders.sort();
    assert_eq!(project_folders(&rows, &ListPrefs::default()), folders);

    // A folder you named, above the repo, wins over the repo's root.
    let p = prefs(&[(root.to_str().unwrap(), "all")]);
    let lines = build_with(&rows, &p, "");
    let heads = lines.iter().filter(|l| matches!(l, Line::Group(_))).count();
    assert_eq!((keys(&lines)[0].as_str(), heads), ("[all]", 1));
}
