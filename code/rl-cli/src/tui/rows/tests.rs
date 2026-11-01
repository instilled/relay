use super::*;
use rl_proto::decision::{DecisionOption, DecisionStatus};
use rl_proto::flow::{FiringStatus, StepStatus};
use rl_proto::types::{Confidence, RunKind, RunState};

#[test]
fn a_flows_marks_are_one_per_firing_and_say_its_worst() {
    let t: Timestamp = "2026-09-10T11:00:00Z".parse().unwrap();
    let ran = |id: &str, chain: &str, state: RunState| {
        let mut r = Run::new(id, RunKind::Command, id, vec![], "/w", "/w", t);
        r.chain = chain.into();
        r.state = state;
        DigestItem {
            run: r,
            summary: String::new(),
            at: t,
            unseen: false,
            joined_by: None,
            emitted: vec![],
            cwd: None,
            recap: None,
        }
    };
    // Newest first, as a row keeps them: fi3 stopped; fi2 two steps, well; fi1 a step failed.
    let items = [
        ran("c", "fi3", RunState::Stopped),
        ran("b2", "fi2", RunState::Finished),
        ran("b1", "fi2", RunState::Finished),
        ran("a2", "fi1", RunState::Failed),
        ran("a1", "fi1", RunState::Finished),
    ];
    let f = FlowRow {
        key: "f:x".into(),
        name: "x".into(),
        runs: items.iter().collect(),
        when: None,
        firing: None,
    };
    assert_eq!(f.marks(10), "×·○", "oldest first, each firing's worst step");
    assert_eq!(f.marks(2), "·○", "the newest firings");
}

fn item(id: &str, state: RunState, ws: &str, at: &str, unseen: bool) -> DigestItem {
    let t: Timestamp = at.parse().unwrap();
    let mut r = Run::new(id, RunKind::Agent, format!("title {id}"), vec![], ws, ws, t);
    r.state = state;
    r.confidence = Confidence::Exact;
    r.state_at = t;
    if state.is_terminal() {
        r.ended_at = Some(t);
    }
    DigestItem {
        run: r,
        summary: format!("summary {id}"),
        at: t,
        unseen,
        joined_by: None,
        emitted: vec![],
        cwd: None,
        recap: None,
    }
}

#[test]
fn a_row_says_its_recap_while_it_waits_and_what_it_does_at_work() {
    let at = "2026-09-23T10:00:00Z";
    let with = |state: RunState| {
        let mut i = item("a1", state, "/w", at, false);
        i.run.cli = Some("claude".into());
        i.summary = "tool: Bash".into();
        i.recap = Some("Search fixed; nothing committed. Next: say commit.".into());
        i
    };
    for state in [RunState::Starting, RunState::Working] {
        assert_eq!(said(&with(state)), "tool: Bash", "{state:?}");
    }
    for state in [
        RunState::Idle,
        RunState::NeedsInput,
        RunState::Blocked,
        RunState::Stale,
        RunState::Finished,
        RunState::Failed,
        RunState::Stopped,
    ] {
        assert_eq!(
            said(&with(state)),
            "Search fixed; nothing committed. Next: say commit.",
            "{state:?}"
        );
    }
    // No recap kept: where it stands, as ever.
    let mut none = with(RunState::Idle);
    none.recap = None;
    assert_eq!(said(&none), "tool: Bash");
    // What the row says is what a search finds.
    let d = Digest {
        since: at.parse().unwrap(),
        items: vec![with(RunState::Idle)],
        decisions: vec![],
        checkouts: vec![],
        lost: vec![],
        hidden: vec![],
        order: vec![],
        firings: Vec::new(),
        outside: vec![],
    };
    let names = Names::default();
    let now: Timestamp = at.parse().unwrap();
    assert_eq!(keys(&build_with(&d, &names, "commit", now)), ["[w]", "r:a1"]);
}

#[test]
fn what_a_restart_took_is_said_with_the_command_that_picks_it_up() {
    let at = "2026-09-23T10:00:00Z";
    let agent = item("a1", RunState::Failed, "/w", at, false);
    let mut cmd = item("c1", RunState::Failed, "/w", at, false);
    cmd.run.kind = RunKind::Command;
    let d = |lost: &[&str], items: Vec<DigestItem>| Digest {
        since: at.parse().unwrap(),
        items,
        decisions: vec![],
        checkouts: vec![],
        lost: lost.iter().map(|s| s.to_string()).collect(),
        hidden: Vec::new(),
        order: Vec::new(),
        firings: Vec::new(),
        outside: vec![],
    };
    assert_eq!(
        lost_note(&d(&[], vec![agent.clone()])),
        None,
        "nothing lost, nothing said"
    );
    assert_eq!(
        lost_note(&d(&["a1", "c1"], vec![agent.clone(), cmd.clone()])).as_deref(),
        Some("2 lost in a restart · /resume all")
    );
    // `/resume all` would leave a command out: only commands, then the
    // words that take them.
    assert_eq!(
        lost_note(&d(&["c1"], vec![agent, cmd])).as_deref(),
        Some("1 lost in a restart · /resume all commands")
    );
}

fn question(id: &str, ws: &str, run: Option<&str>, at: &str) -> Decision {
    let t: Timestamp = at.parse().unwrap();
    Decision {
        id: id.into(),
        workspace: ws.into(),
        run: run.map(Into::into),
        chain: id.into(),
        group: None,
        title: format!("question {id}"),
        summary: None,
        options: vec![DecisionOption {
            key: "ok".into(),
            ..Default::default()
        }],
        multi: false,
        status: DecisionStatus::Open,
        created_at: t,
        updated_at: t,
        expires_at: None,
        resolved_at: None,
        chosen: vec![],
        outcome_runs: vec![],
        note: None,
    }
}

fn keys(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|r| match r {
            Row::Group(h) => format!("[{}]", h.name),
            other => other.key().unwrap(),
        })
        .collect()
}

/// The header of the project called `name`, among the built rows.
fn head_of<'a>(rows: &'a [Row], name: &str) -> &'a Head {
    rows.iter()
        .find_map(|r| match r {
            Row::Group(h) if h.name == name => Some(h),
            _ => None,
        })
        .expect("a project header")
}

/// The list's toggles `(flows, agents, shells, hidden)`: no flows, the rest on, hidden off.
const DEFAULT: (bool, bool, bool, bool) = (false, true, true, false);

/// `DEFAULT` with the flows on, as the catch-up has them.
const FLOWS_ON: (bool, bool, bool, bool) = (true, true, true, false);

fn build_with<'a>(d: &'a Digest, names: &Names, filter: &str, now: Timestamp) -> Vec<Row<'a>> {
    toggles_build(d, names, &[], DEFAULT, filter, now)
}

fn lens_build<'a>(d: &'a Digest, names: &Names, t: (bool, bool, bool, bool), now: Timestamp) -> Vec<Row<'a>> {
    toggles_build(d, names, &[], t, "", now)
}

/// The list as the catch-up builds it, flows on, with these flows loaded.
fn flows_build<'a>(d: &'a Digest, names: &Names, flows: &[Flow], filter: &str, now: Timestamp) -> Vec<Row<'a>> {
    toggles_build(d, names, flows, FLOWS_ON, filter, now)
}

/// The list with the flows loaded and the toggles `(flows, agents, shells, hidden)`.
fn toggles_build<'a>(
    d: &'a Digest,
    names: &Names,
    flows: &[Flow],
    (show_flows, show_agents, show_shells, show_hidden): (bool, bool, bool, bool),
    filter: &str,
    now: Timestamp,
) -> Vec<Row<'a>> {
    build(
        d,
        &Context {
            names,
            flows,
            show_flows,
            show_agents,
            show_shells,
            show_hidden,
            filter,
            now,
            relay: None,
            parked: &[],
        },
    )
}

/// A flow as `flow.list` gives it: on, every 30m, `in` the folder given.
fn flow(name: &str, in_: Option<&str>) -> Flow {
    Flow {
        name: name.into(),
        hash: "h".into(),
        def: serde_json::json!({
            "in_": in_,
            "trigger": { "when": { "Every": 1800 }, "on": [] },
        }),
        source: format!("/h/flows/{name}").into(),
        folder: in_.map(PathBuf::from),
        paused: false,
        approved_hash: Some("h".into()),
        last_fired_at: None,
        next_at: None,
        removed_at: None,
    }
}

/// A step of a flow, as the cause of a run.
fn step_of(flow: &str) -> Cause {
    Cause::Flow {
        flow: flow.into(),
        step: "probe".into(),
    }
}

#[test]
fn a_search_narrows_by_words_and_by_what_things_are() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let (api, web) = ("/home/f/api", "/home/f/web");
    let at = "2026-09-10T11:00:00Z";
    let mut agent = item("agent", RunState::Working, api, at, false);
    agent.run.kind = RunKind::Agent;
    agent.run.title = "fix the login".into();
    let mut failed = item("failed", RunState::Failed, web, at, false);
    failed.run.title = "cargo test".into();
    failed.run.kind = RunKind::Command;
    let mut probe = item("probe", RunState::Working, web, at, false);
    probe.run.kind = RunKind::Command;
    probe.run.cause = step_of("nightly");
    // The question is a firing's: its chain is the firing's id.
    let asking = Firing {
        id: "q1".into(),
        flow: "tests-failed".into(),
        ..firing(FiringStatus::Waiting, &[])
    };
    let d = Digest {
        since: now,
        items: vec![agent, failed, probe],
        decisions: vec![question("q1", api, None, at)],
        checkouts: Vec::new(),
        lost: Vec::new(),
        hidden: Vec::new(),
        order: Vec::new(),
        firings: vec![asking],
        outside: vec![],
    };
    let names = Names::new([
        (PathBuf::from(api), "api".to_string()),
        (PathBuf::from(web), "web".to_string()),
        (PathBuf::from("/home/f/idle"), "idle".to_string()),
    ]);
    let shown = |f: &str| keys(&flows_build(&d, &names, &[], f, now));
    // Every word somewhere: the title and the project both count.
    assert_eq!(shown("login api"), ["[api]", "r:agent"]);
    assert_eq!(shown("login web"), Vec::<String>::new());
    // A project's name keeps all of it; an idle project answers to its name.
    assert_eq!(shown("in:api"), ["[api]", "r:agent", "q:q1"]);
    assert_eq!(shown("idle"), ["[idle]"]);
    // Tokens pick by what a thing is, and take no bare project along.
    assert_eq!(shown("is:failed"), ["[web]", "r:failed"]);
    assert_eq!(shown("kind:agent"), ["[api]", "r:agent"]);
    assert_eq!(
        shown("kind:question"),
        ["[api]", "q:q1"],
        "a question stands in its project"
    );
    assert_eq!(shown("by:nightly"), ["[web]", "r:probe"]);
    assert_eq!(shown("by:tests"), ["[api]", "q:q1"], "a question by its flow's firing");
    // A word nothing says finds nothing.
    assert_eq!(shown("global"), Vec::<String>::new());
}

#[test]
fn a_summary_of_everything_is_not_a_project_of_its_own() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let (api, relay) = ("/home/f/api", "/home/f/.relay");
    let at = "2026-09-10T11:00:00Z";
    // The top, written in the Relay home; a recap, in the project's folder.
    let mut top = item("s1", RunState::Finished, relay, at, true);
    top.run.cause = Cause::Assistant("summary".into());
    let mut recap = item("s2", RunState::Finished, api, at, true);
    recap.run.cause = Cause::Assistant("summary".into());
    // Anything else run there is the list's as ever.
    let shell = item("sh1", RunState::Finished, relay, at, true);
    let d = Digest {
        since: now,
        items: vec![top, recap, shell],
        decisions: vec![],
        checkouts: Vec::new(),
        lost: Vec::new(),
        hidden: Vec::new(),
        order: Vec::new(),
        firings: Vec::new(),
        outside: vec![],
    };
    let names = Names::new([(PathBuf::from(api), "api".to_string())]);
    let cx = |relay: Option<&'static str>| Context {
        names: &names,
        flows: &[],
        parked: &[],
        show_flows: true,
        show_agents: true,
        show_shells: true,
        show_hidden: false,
        filter: "",
        now,
        relay: relay.map(Path::new),
    };
    assert_eq!(
        keys(&build(&d, &cx(Some(relay)))),
        ["[.relay]", "r:sh1", "[api]", "r:s2"]
    );
    let mut d = d;
    d.items.retain(|i| i.run.id != "sh1");
    assert_eq!(
        keys(&build(&d, &cx(Some(relay)))),
        ["[api]", "r:s2"],
        "no group for the top alone"
    );
    assert_eq!(keys(&build(&d, &cx(None))), ["[.relay]", "r:s1", "[api]", "r:s2"]);
}

#[test]
fn a_search_lands_on_the_project_it_names_not_what_mentions_it() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let (api, web) = ("/home/f/api", "/home/f/web");
    let at = "2026-09-10T11:00:00Z";
    let mut pinned = item("pinned", RunState::Working, api, at, false);
    pinned.run.pinned_at = Some(now);
    pinned.summary = "waiting on web to deploy".into();
    let mut deploy = item("deploy", RunState::Working, api, at, false);
    deploy.run.title = "web deploy".into();
    let d = Digest {
        since: now,
        items: vec![pinned, deploy, item("w1", RunState::Working, web, at, false)],
        decisions: vec![],
        checkouts: Vec::new(),
        lost: Vec::new(),
        hidden: Vec::new(),
        order: Vec::new(),
        firings: Vec::new(),
        outside: vec![],
    };
    let names = Names::new([
        (PathBuf::from(api), "api".to_string()),
        (PathBuf::from(web), "web".to_string()),
    ]);
    let best = |f: &str| {
        let rows = build_with(&d, &names, f, now);
        let named: Vec<Named> = rows.iter().map(|r| named(r, &d, &names, &[])).collect();
        let list = keys(&rows);
        let order = crate::tui::query::ranked(&Query::parse(f).words, &named);
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
fn the_toggles_keep_a_run_by_what_it_is_or_what_started_it() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let ws = "/home/f/relay";
    let agent = item("agent", RunState::Working, ws, "2026-09-10T11:00:00Z", false);
    let mut shell = item("shell", RunState::Working, ws, "2026-09-10T11:01:00Z", false);
    // What a warm shell really is: the `sh` profile's kind wins over the
    // `Command` hint it was started with. Not an agent, nothing started
    // it — a shell, which the shells toggle governs.
    shell.run.kind = RunKind::Shell;
    // A flow's agent step: both an agent and a flow's.
    let mut probe = item("probe", RunState::Working, ws, "2026-09-10T11:02:00Z", false);
    probe.run.cause = step_of("nightly");
    // A flow's shell step: a flow's, and not an agent.
    let mut onfail = item("onfail", RunState::Working, ws, "2026-09-10T11:03:00Z", false);
    onfail.run.cause = step_of("on-fail");
    onfail.run.kind = RunKind::Shell;
    let d = Digest {
        since: now,
        items: vec![agent, shell, probe, onfail],
        decisions: vec![],
        checkouts: Vec::new(),
        lost: Vec::new(),
        hidden: Vec::new(),
        order: Vec::new(),
        firings: Vec::new(),
        outside: vec![],
    };
    let names = Names::new([(PathBuf::from(ws), "relay".to_string())]);
    let runs = |t| {
        lens_build(&d, &names, t, now)
            .iter()
            .filter_map(|r| r.item().map(|i| i.run.id.clone()))
            .collect::<Vec<_>>()
    };
    // All on, in the order they started.
    assert_eq!(runs((true, true, true, false)), ["agent", "shell", "probe", "onfail"]);
    // Agents off drops the lone agent; the shell stays (shells on), and
    // the flow's steps stay — the agent step as a flow's, the shell step
    // as a shell.
    assert_eq!(runs((true, false, true, false)), ["shell", "probe", "onfail"]);
    // Flows off drops both flow steps, however agents and shells are set:
    // a flow's run is the flow's, whatever its kind.
    assert_eq!(runs((false, true, true, false)), ["agent", "shell"]);
    // Shells off drops the plain shell; the flow's steps are the flow's.
    assert_eq!(runs((false, true, false, false)), ["agent"]);
}

#[test]
fn with_flows_off_no_flow_run_nor_its_question_is_on_the_list() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let ws = "/home/f/relay";
    // A flow's agent step that is asking, a plain run, and the question.
    let mut a1 = fired("a1", "pulse", RunState::NeedsInput, "2026-09-10T11:00:00Z", true);
    a1.run.kind = RunKind::Agent;
    let mut d = digest(
        now,
        vec![a1, item("w", RunState::Working, ws, "2026-09-10T11:01:00Z", false)],
        vec![],
    );
    d.decisions = vec![question("q1", ws, Some("a1"), "2026-09-10T11:02:00Z")];
    let names = Names::new([(PathBuf::from(ws), "relay".to_string())]);
    let flows = [flow("pulse", Some(ws))];
    let rows = toggles_build(&d, &names, &flows, (false, true, true, false), "", now);
    assert_eq!(keys(&rows), ["[relay]", "r:w"]);
}

#[test]
fn with_flows_off_a_pinned_or_come_through_flow_run_is_not_in_the_band() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let ws = "/home/f/relay";
    let mut pinned = fired("pin", "pulse", RunState::Finished, "2026-09-10T11:00:00Z", false);
    pinned.run.pinned_at = Some("2026-09-10T11:05:00Z".parse().unwrap());
    // Hidden earlier, since failed: it would come through.
    let mut back = fired("back", "pulse", RunState::Failed, "2026-09-10T11:30:00Z", true);
    back.run.hidden_at = Some("2026-09-10T10:00:00Z".parse().unwrap());
    // A live pinned one too: not terminal, so `flow_of` does not know it.
    let mut live = fired("live", "pulse", RunState::NeedsInput, "2026-09-10T11:40:00Z", true);
    live.run.pinned_at = Some("2026-09-10T11:45:00Z".parse().unwrap());
    let d = digest(now, vec![pinned, back, live], vec![]);
    let names = Names::new([(PathBuf::from(ws), "relay".to_string())]);
    let flows = [flow("pulse", Some(ws))];
    let on = keys(&toggles_build(&d, &names, &flows, (true, true, true, false), "", now));
    for k in ["r:pin", "r:back", "r:live"] {
        assert!(on.contains(&k.to_string()), "{on:?}");
    }
    let off = keys(&toggles_build(&d, &names, &flows, (false, true, true, false), "", now));
    assert!(
        !off.iter().any(|k| ["r:pin", "r:back", "r:live"].contains(&k.as_str())),
        "{off:?}"
    );
}

#[test]
fn with_flows_off_a_flow_run_that_needs_you_is_not_hinted_on_its_project() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let ws = "/home/f/relay";
    let asking = fired("a1", "pulse", RunState::NeedsInput, "2026-09-10T11:00:00Z", true);
    let mut hid_ = fired("a2", "pulse", RunState::NeedsInput, "2026-09-10T11:10:00Z", true);
    hid_.run.hidden_at = Some("2026-09-10T11:20:00Z".parse().unwrap());
    let d = digest(
        now,
        vec![
            asking,
            hid_,
            item("w", RunState::Working, ws, "2026-09-10T11:01:00Z", false),
        ],
        vec![],
    );
    let names = Names::new([(PathBuf::from(ws), "relay".to_string())]);
    let flows = [flow("pulse", Some(ws))];
    let rows = toggles_build(&d, &names, &flows, (false, true, true, false), "", now);
    assert_eq!(head_of(&rows, "relay").attention, 0);
}

#[test]
fn pinned_runs_stand_above_everything_however_old_whatever_the_toggles() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let mut old = item(
        "old01",
        RunState::Finished,
        "/home/f/api",
        "2026-09-01T10:00:00Z",
        false,
    );
    old.run.pinned_at = Some("2026-09-02T10:00:00Z".parse().unwrap());
    let mut shell = item(
        "shell",
        RunState::Working,
        "/home/f/relay",
        "2026-09-10T11:00:00Z",
        false,
    );
    shell.run.kind = RunKind::Shell;
    shell.run.pinned_at = Some("2026-09-03T10:00:00Z".parse().unwrap());
    let d = Digest {
        since: now,
        items: vec![
            shell,
            item("fail1", RunState::Failed, "/home/f/relay", "2026-09-10T11:30:00Z", true),
            old,
        ],
        decisions: vec![question("q1", "/home/f/relay", Some("fail1"), "2026-09-10T11:31:00Z")],
        checkouts: Vec::new(),
        lost: Vec::new(),
        hidden: Vec::new(),
        order: Vec::new(),
        firings: Vec::new(),
        outside: vec![],
    };
    let names = Names::new([
        (PathBuf::from("/home/f/relay"), "relay".to_string()),
        (PathBuf::from("/home/f/api"), "api".to_string()),
    ]);
    // In the order they were pinned, above the projects; a week-old run
    // stays, and its folder keeps its place by name.
    assert_eq!(
        keys(&build_with(&d, &names, "", now)),
        ["r:old01", "r:shell", "[api]", "[relay]", "r:fail1", "q:q1"]
    );
    // No toggle takes a pin away.
    let flows = lens_build(&d, &names, (true, false, true, false), now);
    assert_eq!(&keys(&flows)[..2], ["r:old01", "r:shell"]);
    // The filter does, and matches a pin by its folder too.
    assert_eq!(keys(&build_with(&d, &names, "api", now)), ["r:old01", "[api]"]);
}

fn hid(key: &str, at: &str) -> Hidden {
    Hidden {
        key: key.into(),
        at: at.parse().unwrap(),
    }
}

#[test]
fn what_you_hid_shows_inline_when_hidden_is_on() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let (api, relay, scratch) = ("/home/f/api", "/home/f/relay", "/home/f/scratch");
    let at = "2026-09-10T11:00:00Z";
    let mut mine = item("mine", RunState::Working, relay, at, false);
    mine.run.hidden_at = Some("2026-09-10T11:10:00Z".parse().unwrap());
    let mut fired = item("fired", RunState::Finished, relay, at, false);
    fired.run.cause = step_of("nightly");
    let mut pinned = item("pinned", RunState::Working, relay, at, false);
    pinned.run.pinned_at = Some(at.parse().unwrap());
    pinned.run.hidden_at = Some("2026-09-10T11:10:00Z".parse().unwrap());
    let d = Digest {
        since: now,
        items: vec![
            mine,
            item("kept", RunState::Working, relay, at, false),
            fired,
            pinned,
            item("inapi", RunState::Finished, api, at, false),
            item("loose", RunState::Working, scratch, at, false),
        ],
        decisions: Vec::new(),
        checkouts: Vec::new(),
        lost: Vec::new(),
        hidden: vec![
            // A registered project, a folder that is not one, a flow, and
            // a folder with nothing on the list at all.
            hid(&format!("g:{api}"), "2026-09-10T11:10:00Z"),
            hid(&format!("g:{scratch}"), "2026-09-10T11:10:00Z"),
            hid("f:nightly", "2026-09-10T11:10:00Z"),
            hid("g:/home/f/gone", "2026-09-10T11:10:00Z"),
        ],
        order: Vec::new(),
        firings: Vec::new(),
        outside: vec![],
    };
    let names = Names::new([
        (PathBuf::from(api), "api".to_string()),
        (PathBuf::from(relay), "relay".to_string()),
    ]);
    let nightly = [flow("nightly", Some(relay))];
    let view = |t| keys(&toggles_build(&d, &names, &nightly, t, "", now));
    // Hidden off, the default: what you hid is off the list — the project's
    // header too, the pin too. Nothing hidden needs you, so nothing comes
    // through. `kept` is an agent, so agents off takes it away.
    assert_eq!(view((true, true, true, false)), ["[relay]", "r:kept"]);
    assert_eq!(view((false, true, true, false)), ["[relay]", "r:kept"]);
    assert_eq!(view((true, false, true, false)), ["[relay]"]);
    // Hidden on, the hidden rows stand inline in their places, alongside
    // the kept ones: a hidden project's header, registered or not, even
    // with nothing under it; the flow folded as ever; a hidden pin in the
    // pinned band; `kept`, not hidden, among them.
    assert_eq!(
        view((true, true, true, true)),
        [
            "r:pinned",
            "[api]",
            "r:inapi",
            "[gone]",
            "[relay]",
            "r:kept",
            "r:mine",
            "f:nightly",
            "[scratch]",
            "r:loose",
        ]
    );
    // The search reads the word too: with hidden on, `is:hidden` keeps
    // only what is hidden.
    assert_eq!(
        keys(&build(
            &d,
            &Context {
                names: &names,
                flows: &nightly,
                filter: "is:hidden",
                show_flows: true,
                show_agents: true,
                show_shells: true,
                show_hidden: true,
                now,
                relay: None,
                parked: &[],
            },
        )),
        ["r:pinned", "[relay]", "r:mine"]
    );
}

#[test]
fn what_you_hid_comes_through_when_it_comes_to_need_you() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let (api, relay) = ("/home/f/api", "/home/f/relay");
    let t = |s: &str| -> Timestamp { s.parse().unwrap() };
    // Hidden while it worked, and failed after: it comes through.
    let mut failed = item("failed", RunState::Failed, relay, "2026-09-10T11:00:00Z", true);
    failed.run.hidden_at = Some(t("2026-09-10T10:00:00Z"));
    // Hidden once it had failed: that was you saying so; it stays away.
    let mut seen = item("seen", RunState::Failed, relay, "2026-09-10T11:00:00Z", true);
    seen.run.hidden_at = Some(t("2026-09-10T11:30:00Z"));
    // In a hidden project: one that asks after comes through, one still
    // working does not.
    let asks = item("asks", RunState::NeedsInput, api, "2026-09-10T10:30:00Z", true);
    let busy = item("busy", RunState::Working, api, "2026-09-10T10:30:00Z", false);
    // A hidden flow's run that failed after comes through as a run.
    let mut firing = item("firing", RunState::Failed, relay, "2026-09-10T11:20:00Z", true);
    firing.run.cause = step_of("nightly");
    let d = Digest {
        since: now,
        items: vec![failed, seen, asks, busy, firing],
        decisions: vec![question("q1", relay, None, "2026-09-10T11:31:00Z")],
        checkouts: Vec::new(),
        lost: Vec::new(),
        hidden: vec![
            hid(&format!("g:{api}"), "2026-09-10T10:00:00Z"),
            hid("f:nightly", "2026-09-10T10:00:00Z"),
        ],
        order: Vec::new(),
        firings: Vec::new(),
        outside: vec![],
    };
    let names = Names::new([
        (PathBuf::from(api), "api".to_string()),
        (PathBuf::from(relay), "relay".to_string()),
    ]);
    let nightly = [flow("nightly", Some(relay))];
    let all = flows_build(&d, &names, &nightly, "", now);
    // Above the projects, what asks before what failed, then in the
    // order it came; its project stays hidden.
    assert_eq!(keys(&all), ["r:asks", "r:failed", "r:firing", "[relay]", "q:q1"]);
    let (_, need, ..) = counts(&all);
    assert_eq!(need, 3, "the headline counts them as needing you");
    // A toggle still narrows it, as it would any run that needs you:
    // agents off leaves only the flow's run among those come through.
    assert_eq!(
        keys(&toggles_build(
            &d,
            &names,
            &nightly,
            (true, false, true, false),
            "",
            now
        )),
        ["r:firing", "[relay]", "q:q1"]
    );
    // With hidden on they are in their places, not above them: the run
    // folds into its flow again, which stands by its run's start. The
    // question, which ignores the toggles, is in its project here too.
    assert_eq!(
        keys(&toggles_build(&d, &names, &nightly, (true, true, true, true), "", now)),
        [
            "[api]",
            "r:asks",
            "r:busy",
            "[relay]",
            "r:failed",
            "r:seen",
            "f:nightly",
            "q:q1"
        ]
    );
    // Hiding it again is how it goes back: the time moves on.
    let mut again = d.clone();
    again.items[0].run.hidden_at = Some(t("2026-09-10T11:45:00Z"));
    assert_eq!(
        keys(&flows_build(&again, &names, &nightly, "", now)),
        ["r:asks", "r:firing", "[relay]", "q:q1"]
    );
    let r = &d.items[2].run;
    assert_eq!(hidden_since(r, &names, &[], &d.hidden), Some(t("2026-09-10T10:00:00Z")));
    assert_eq!(hidden_with(r, &names, &[], &d.hidden), Some("its project"));
    assert_eq!(hidden_with(&d.items[4].run, &names, &[], &d.hidden), Some("its flow"));
    assert_eq!(hidden_with(&d.items[0].run, &names, &[], &d.hidden), None);
    assert!(comes_through(r, t("2026-09-10T10:00:00Z")));
    assert!(!comes_through(r, t("2026-09-10T10:30:00Z")), "not after, at");
}

#[test]
fn a_folder_is_one_project_whatever_branch_its_runs_are_on() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let on = |mut i: DigestItem, b: &str| {
        i.run.branch = Some(b.into());
        i
    };
    let co = |p: &str, b: &str, worktrees| Checkout {
        path: p.into(),
        branch: Some(b.into()),
        detached: None,
        worktrees,
        badges: vec![],
    };
    let d = Digest {
        since: now,
        items: vec![
            on(
                item("a1", RunState::Working, "/home/f/api", "2026-09-10T11:50:00Z", false),
                "main",
            ),
            on(
                item("a2", RunState::Working, "/home/f/api", "2026-09-10T11:40:00Z", false),
                "feat",
            ),
            on(
                item("w1", RunState::Working, "/home/f/web", "2026-09-10T11:30:00Z", false),
                "main",
            ),
        ],
        decisions: vec![],
        checkouts: vec![co("/home/f/api", "main", 3), co("/home/f/web", "main", 0)],
        lost: Vec::new(),
        hidden: Vec::new(),
        order: Vec::new(),
        firings: Vec::new(),
        outside: vec![],
    };
    let names = Names::default();
    let rows = build_with(&d, &names, "", now);
    // Two repos on `main` stay two projects, each named for its folder.
    assert_eq!(keys(&rows), ["[api]", "r:a2", "r:a1", "[web]", "r:w1"]);
    let head = |n: usize| match &rows[n] {
        Row::Group(h) => checkout_words(h.checkout.as_ref().unwrap()),
        _ => panic!("a header"),
    };
    assert_eq!((head(0), head(3)), ("⎇ main · 3 worktrees".into(), "⎇ main".into()));
    // Only the run on another branch than its folder's says so.
    assert_eq!(branch_aside(&d.items[0].run, &d.checkouts), None);
    assert_eq!(branch_aside(&d.items[1].run, &d.checkouts).as_deref(), Some("⎇ feat"));
    assert_eq!(
        branch_aside(&d.items[1].run, &[]),
        None,
        "no checkout, nothing to differ from"
    );

    assert_eq!(keys(&build_with(&d, &names, "on:feat", now)), ["[api]", "r:a2"]);
    assert_eq!(
        keys(&build_with(&d, &names, "on:main", now)),
        ["[api]", "r:a1", "[web]", "r:w1"]
    );
}

#[test]
fn a_checkout_in_words() {
    let mut c = Checkout {
        path: "/w".into(),
        branch: None,
        detached: Some("a1b2c3d".into()),
        worktrees: 0,
        badges: vec![],
    };
    assert_eq!(checkout_words(&c), "⎇ a1b2c3d");
    c.branch = Some("main".into());
    c.detached = None;
    c.worktrees = 1;
    assert_eq!(checkout_words(&c), "⎇ main · 1 worktree");
    c.worktrees = 4;
    c.badges = vec!["PR #95".into()];
    assert_eq!(checkout_words(&c), "⎇ main · PR #95 · 4 worktrees");
}

#[test]
fn a_question_stands_in_its_project_and_projects_go_by_name() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let d = Digest {
        since: now,
        items: vec![
            item("work1", RunState::Working, "/home/f/api", "2026-09-10T11:50:00Z", false),
            item("fail1", RunState::Failed, "/home/f/relay", "2026-09-10T11:00:00Z", true),
            item("blok1", RunState::Blocked, "/home/f/api", "2026-09-10T11:55:00Z", true),
            item(
                "done1",
                RunState::Finished,
                "/home/f/relay",
                "2026-09-10T10:00:00Z",
                false,
            ),
            item(
                "old01",
                RunState::Finished,
                "/home/f/relay",
                "2026-09-08T10:00:00Z",
                false,
            ),
        ],
        decisions: vec![question("q1", "/home/f/relay", Some("fail1"), "2026-09-10T11:01:00Z")],
        checkouts: Vec::new(),
        lost: Vec::new(),
        hidden: Vec::new(),
        order: Vec::new(),
        firings: Vec::new(),
        outside: vec![],
    };
    let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
    let rows = build_with(&d, &names, "", now);
    assert_eq!(
        keys(&rows),
        ["[api]", "r:work1", "r:blok1", "[relay]", "r:done1", "r:fail1", "q:q1"],
        "projects by name, however urgent; rows as they came; the question under its run; api is named by its folder; old01 is gone"
    );
    assert_eq!(counts(&rows), (1, 2, 1, 1));
    match &rows[3] {
        Row::Group(h) => assert_eq!((h.name.as_str(), h.place.as_str()), ("relay", "/home/f/relay")),
        _ => panic!("relay's header"),
    }
    assert_eq!(
        headline(&rows, 2, "", 0),
        "relay · 1 to decide · 2 need you · 1 working · 1 finished · 2 new"
    );

    let rows = build_with(&d, &names, "api", now);
    assert_eq!(keys(&rows), ["[api]", "r:work1", "r:blok1"]);
    assert_eq!(headline(&rows, 0, "api", 0), "relay · 1 needs you · 1 working · ⌕ api");
    assert_eq!(headline(&[], 0, "", 0), "relay · nothing running");
}

#[test]
fn a_registered_folder_shows_even_with_no_runs() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let d = Digest {
        since: now,
        items: vec![item(
            "work1",
            RunState::Working,
            "/home/f/relay",
            "2026-09-10T11:50:00Z",
            false,
        )],
        decisions: vec![],
        checkouts: Vec::new(),
        lost: Vec::new(),
        hidden: Vec::new(),
        order: Vec::new(),
        firings: Vec::new(),
        outside: vec![],
    };
    let names = Names::new([
        (PathBuf::from("/home/f/relay"), "relay".to_string()),
        (PathBuf::from("/home/f/idle"), "idle".to_string()),
    ]);
    // The idle project is a line of its own, in its place by name.
    assert_eq!(keys(&build_with(&d, &names, "", now)), ["[idle]", "[relay]", "r:work1"]);
    // It carries the folder so selecting it lands on the project.
    match build_with(&d, &names, "idle", now).last() {
        Some(Row::Group(h)) => {
            assert_eq!(h.name, "idle");
            assert_eq!(h.workspace, Path::new("/home/f/idle"));
        }
        _ => panic!("idle folder is the last row it filters to"),
    }
    // The filter reaches idle projects too.
    assert_eq!(keys(&build_with(&d, &names, "idle", now)), ["[idle]"]);
    // A toggle narrows what is inside projects, never which projects there
    // are: the idle one and yours stay, even where nothing in them matches.
    assert_eq!(
        keys(&lens_build(&d, &names, (true, false, true, false), now)),
        ["[idle]", "[relay]"]
    );
}

#[test]
fn with_no_runs_and_no_folders_the_list_is_empty() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let d = digest(now, vec![], vec![]);
    let waiting = [Flow {
        approved_hash: None,
        ..flow("pulse", None)
    }];
    let rows = flows_build(&d, &Names::default(), &waiting, "", now);
    // A flow waiting for its first yes still has a row, under everywhere.
    assert_eq!(keys(&rows), ["[everywhere]", "f:pulse"]);
    // One with a yes has its row, but has not done anything: a fresh home
    // still says it has nothing running.
    let flows = [flow("pulse", None)];
    let rows = flows_build(&d, &Names::default(), &flows, "", now);
    assert_eq!(keys(&rows), ["[everywhere]", "f:pulse"]);
    assert_eq!(headline(&rows, 0, "", 0), "relay · nothing running");
}

/// The list's digest: these runs and firings, and nothing else.
fn digest(now: Timestamp, items: Vec<DigestItem>, firings: Vec<Firing>) -> Digest {
    Digest {
        since: now,
        items,
        decisions: vec![],
        checkouts: Vec::new(),
        lost: Vec::new(),
        hidden: Vec::new(),
        order: Vec::new(),
        firings,
        outside: vec![],
    }
}

#[test]
fn a_flow_tally_in_words() {
    let t = |total, running, needs| {
        FlowTally {
            total,
            running,
            needs,
            parked: 0,
        }
        .words()
    };
    assert_eq!(t(3, 1, 1), "3 flows · 1 running · 1 needs you");
    assert_eq!(t(1, 0, 0), "1 flow");
    assert_eq!(t(2, 0, 2), "2 flows · 2 needs you");
    assert_eq!(t(0, 0, 0), "");
}

#[test]
fn a_project_line_tallies_its_flows_and_what_needs_you() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let flows = vec![
        flow("a", Some(relay)),
        Flow {
            approved_hash: None,
            ..flow("b", Some(relay))
        },
        flow("c", Some(relay)),
        flow("x", None),
    ];
    let running = Firing {
        flow: "a".into(),
        ..firing(FiringStatus::Running, &[])
    };
    let failed = |unseen| fired("c1", "c", RunState::Failed, "2026-09-10T11:00:00Z", unseen);
    let names = Names::new([(relay.into(), "relay".to_string())]);
    let tally_of = |items: Vec<DigestItem>, decisions: Vec<Decision>| {
        let mut d = digest(now, items, vec![running.clone()]);
        d.decisions = decisions;
        let rows = toggles_build(&d, &names, &flows, (false, true, true, false), "", now);
        head_of(&rows, "relay").tally
    };
    assert_eq!(
        tally_of(vec![failed(true)], vec![]),
        FlowTally {
            total: 3,
            running: 1,
            needs: 2,
            parked: 0
        }
    );
    assert_eq!(tally_of(vec![failed(false)], vec![]).needs, 1);
    let asked = question("q1", relay, Some("c1"), "2026-09-10T11:30:00Z");
    assert_eq!(tally_of(vec![failed(false)], vec![asked]).needs, 2);

    // A failure the flow has since got past (a later run ended well) no longer lights the line.
    let well = fired("c2", "c", RunState::Finished, "2026-09-10T11:30:00Z", false);
    assert_eq!(tally_of(vec![failed(true), well], vec![]).needs, 1);
    let before = fired("c0", "c", RunState::Finished, "2026-09-10T10:30:00Z", false);
    assert_eq!(
        tally_of(vec![before, failed(true)], vec![]).needs,
        2,
        "an older good run does not"
    );

    // Each of these puts a flow in needs on its own.
    let needs = |name: &str, items: Vec<DigestItem>, decisions: Vec<Decision>, firings: Vec<Firing>| {
        let mut d = digest(now, items, firings);
        d.decisions = decisions;
        flow_needs(&flow(name, Some(relay)), &d)
    };
    let nothing = || needs("d", vec![], vec![], vec![]);
    assert!(!nothing(), "nothing open, nothing needed");
    let firing_d = Firing {
        id: "fi9".into(),
        flow: "d".into(),
        ..firing(FiringStatus::Waiting, &[])
    };
    let ask = question("fi9", relay, None, "2026-09-10T11:30:00Z");
    assert!(
        needs("d", vec![], vec![ask], vec![firing_d]),
        "a question owned by its firing"
    );
    let at = "2026-09-10T11:00:00Z";
    let asking = fired("d1", "d", RunState::NeedsInput, at, false);
    assert!(needs("d", vec![asking], vec![], vec![]), "a run asking");
    let blocked = fired("d1", "d", RunState::Blocked, at, false);
    assert!(needs("d", vec![blocked], vec![], vec![]), "a run blocked");
}

#[test]
fn the_everywhere_line_stands_with_its_tally_when_a_flow_has_no_in() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let d = digest(now, vec![], vec![]);
    let names = Names::default();
    let loose = [
        flow("pulse", None),
        Flow {
            approved_hash: None,
            ..flow("deploy", None)
        },
    ];
    let rows = toggles_build(&d, &names, &loose, DEFAULT, "", now);
    // No runs under it, and the list has no flow rows: only its line.
    assert_eq!(keys(&rows), ["[everywhere]"]);
    assert_eq!(
        head_of(&rows, EVERYWHERE).tally,
        FlowTally {
            total: 2,
            running: 0,
            needs: 1,
            parked: 0
        }
    );
    // A flow `in` a folder, or one removed, is not everywhere's.
    let homed = [flow("pulse", Some("/home/f/relay")), {
        let mut gone = flow("old", None);
        gone.removed_at = Some(now);
        gone
    }];
    assert!(keys(&toggles_build(&d, &names, &homed, DEFAULT, "", now)).is_empty());
    assert!(keys(&toggles_build(&d, &names, &[], DEFAULT, "", now)).is_empty());
    // A search for something else takes it away, as it does a project's line.
    assert!(keys(&toggles_build(&d, &names, &loose, DEFAULT, "zzzz", now)).is_empty());
    // With the flows on (the catch-up) its line carries the tally too.
    let rows = flows_build(&d, &names, &loose, "", now);
    assert_eq!(head_of(&rows, EVERYWHERE).tally.total, 2);
}

#[test]
fn the_headline_counts_a_flow_that_needs_you() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let mut d = digest(
        now,
        vec![],
        vec![Firing {
            id: "fi9".into(),
            flow: "pulse".into(),
            ..firing(FiringStatus::Waiting, &[])
        }],
    );
    d.decisions = vec![question("fi9", "/home/f/relay", None, "2026-09-10T11:30:00Z")];
    let flows = [flow("pulse", None), flow("nightly", Some("/home/f/relay"))];
    let n = flows_need(&flows, &d);
    assert_eq!(n, 1);
    assert_eq!(headline(&[], 0, "", n), "relay · 1 needs you");
    assert_eq!(headline(&[], 0, "", 0), "relay · nothing running");
    // Beside the rest that needs you, it is one more.
    let q = question("q", "/w", None, "2026-09-10T11:30:00Z");
    let rows = [Row::Question(&q)];
    assert_eq!(headline(&rows, 0, "", 2), "relay · 1 to decide · 2 need you");
}

#[test]
fn the_headline_says_the_flows_with_no_in_and_a_yes_they_wait_for() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let d = digest(now, vec![], vec![]);
    let say = |flows: &[Flow]| everywhere(flows, &d);
    let (pulse, nightly, notify) = (flow("pulse", None), flow("nightly", None), flow("notify", None));
    let waiting = Flow {
        approved_hash: None,
        ..flow("deploy", None)
    };
    assert_eq!(say(&[]), None, "no flows, nothing said");
    // A flow `in` a folder is its header's, not the headline's.
    assert_eq!(say(&[flow("nightly", Some("/home/f/relay"))]), None);
    assert_eq!(
        say(&[pulse.clone(), nightly.clone(), notify.clone()]),
        Some(("↻ 3 everywhere".into(), false))
    );
    assert_eq!(
        say(std::slice::from_ref(&waiting)),
        Some(("↻ 1 everywhere · deploy needs you".into(), true))
    );
    let all = say(&[pulse, nightly, notify, waiting]).unwrap();
    assert_eq!(all, ("↻ 4 everywhere · deploy needs you".into(), true));
    assert!(!all.0.contains("/flow approve"));
}

#[test]
fn the_headline_names_the_one_flow_with_no_in_that_needs_you() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let d = digest(now, vec![], vec![]);
    let waiting = |n: &str| Flow {
        approved_hash: None,
        ..flow(n, None)
    };
    let say = |flows: &[Flow]| everywhere(flows, &d).unwrap();
    assert_eq!(
        say(&[flow("nightly", None), waiting("pulse")]),
        ("↻ 2 everywhere · pulse needs you".into(), true)
    );
    // With more than one, the count.
    assert_eq!(
        say(&[waiting("pulse"), waiting("deploy")]),
        ("↻ 2 everywhere · 2 needs you".into(), true)
    );
}

#[test]
fn the_headline_says_a_flow_did_not_load_beside_them() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let d = digest(now, vec![], vec![]);
    let parts = |flows: &[Flow], broken| global_parts(flows, &d, broken);
    let flows = [flow("pulse", None), flow("nightly", None)];
    assert_eq!(parts(&flows, false), [("↻ 2 everywhere".to_string(), Tone::Plain)]);
    assert_eq!(
        parts(&flows, true),
        [
            ("↻ 2 everywhere".to_string(), Tone::Plain),
            ("flows broken".to_string(), Tone::Bad)
        ]
    );
    // None loaded: it still says so.
    assert_eq!(parts(&[], true), [("flows broken".to_string(), Tone::Bad)]);
    assert!(parts(&[], false).is_empty());
}

fn fired(id: &str, flow: &str, state: RunState, at: &str, unseen: bool) -> DigestItem {
    let mut i = item(id, state, "/home/f/relay", at, unseen);
    i.run.cause = step_of(flow);
    i.summary = if state == RunState::Failed {
        "exit 1 · 0s".into()
    } else {
        "exit 0 · 0s".into()
    };
    i
}

#[test]
fn a_flows_runs_are_one_row() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let mut items: Vec<DigestItem> = (0..30)
        .map(|n| {
            fired(
                &format!("p{n:02}"),
                "pulse",
                RunState::Finished,
                &format!("2026-09-10T11:{n:02}:00Z"),
                false,
            )
        })
        .collect();
    items.push(fired("bad", "pulse", RunState::Failed, "2026-09-10T10:30:00Z", true));
    // Its run still going stays its own row; so does one started by hand.
    let mut going = fired("go", "pulse", RunState::Working, "2026-09-10T11:59:30Z", false);
    going.run.ended_at = None;
    items.push(going);
    items.push(item(
        "hand",
        RunState::Finished,
        "/home/f/relay",
        "2026-09-10T11:58:00Z",
        false,
    ));
    let d = digest(now, items, vec![]);
    let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
    let flows = [flow("pulse", Some("/home/f/relay"))];
    let rows = flows_build(&d, &names, &flows, "", now);
    assert_eq!(
        keys(&rows),
        ["[relay]", "r:hand", "r:go", "f:pulse"],
        "the runs in the order they came, the flow under them"
    );
    let Some(Row::Flow(f)) = rows.iter().find(|r| matches!(r, Row::Flow(_))) else {
        panic!("no flow row");
    };
    assert_eq!(f.runs.len(), 31);
    assert_eq!(f.last().unwrap().run.id, "p29");
    assert_eq!((f.ok(), f.failed(), f.stopped()), (30, 1, 0));
    assert!(f.unseen(), "the failed run is still news");
    assert_eq!(f.when.as_deref(), Some("every 30m"));
    assert_eq!(
        f.summary(),
        "exit 0 · 0s",
        "the last run's own line; the tally is the marks', the cadence the pane's"
    );
    assert_eq!(f.marks(31).chars().filter(|c| *c == '×').count(), 1);
    assert!(f.marks(31).starts_with('×'), "oldest first: {}", f.marks(31));
    assert_eq!(counts(&rows), (0, 0, 1, 1));
    assert_eq!(headline(&rows, 1, "", 0), "relay · 1 working · 1 finished · 1 new");

    // Never opened: whatever happens, a flow is one row.
    let opened_rows = flows_build(&d, &names, &flows, "", now);
    assert_eq!(keys(&opened_rows), keys(&rows), "no row grows into its runs");

    // The filter finds a flow by its name.
    let rows = flows_build(&d, &names, &flows, "puls", now);
    assert_eq!(keys(&rows), ["[relay]", "f:pulse"], "{:?}", keys(&rows));

    // A flow whose last run failed ranks as failed and says so.
    let d2 = digest(
        now,
        vec![
            fired("a", "pulse", RunState::Finished, "2026-09-10T11:00:00Z", false),
            fired("b", "pulse", RunState::Failed, "2026-09-10T11:30:00Z", true),
            item("w", RunState::Working, "/home/f/relay", "2026-09-10T11:40:00Z", false),
        ],
        vec![],
    );
    let rows = flows_build(&d2, &names, &flows, "", now);
    assert_eq!(
        keys(&rows),
        ["[relay]", "r:w", "f:pulse"],
        "failed, it still stands under the runs"
    );
    assert_eq!(counts(&rows), (0, 1, 1, 0));
}

#[test]
fn a_flow_filtered_by_one_runs_text_keeps_its_full_history() {
    // A filter that matches only one run's own summary (not the flow or
    // group name) must still show the flow's FULL history — not just the
    // matching run — since `ok()`/`failed()`/`marks()`/`summary()` are
    // all computed from `FlowRow.runs`.
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let mut items: Vec<DigestItem> = (0..30)
        .map(|n| {
            fired(
                &format!("p{n:02}"),
                "pulse",
                RunState::Finished,
                &format!("2026-09-10T11:{n:02}:00Z"),
                false,
            )
        })
        .collect();
    items.push(fired("bad", "pulse", RunState::Failed, "2026-09-10T11:30:30Z", true));
    let d = digest(now, items, vec![]);
    let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
    let flows = [flow("pulse", Some("/home/f/relay"))];
    // "exit 1" only appears in the failing run's own summary, not in the
    // flow's name ("pulse") or the group name ("relay").
    let rows = flows_build(&d, &names, &flows, "exit 1", now);
    assert_eq!(keys(&rows), ["[relay]", "f:pulse"], "{:?}", keys(&rows));
    let Some(Row::Flow(f)) = rows.iter().find(|r| matches!(r, Row::Flow(_))) else {
        panic!("no flow row");
    };
    assert_eq!(f.runs.len(), 31, "the flow's full history, not just the matching run");
    assert_eq!(
        (f.ok(), f.failed(), f.stopped()),
        (30, 1, 0),
        "stats reflect the flow's true, overwhelmingly-successful history"
    );
}

/// The latest firing of `prod-logs`, with its steps where they stand.
fn firing(status: FiringStatus, steps: &[(&str, StepStatus)]) -> Firing {
    Firing {
        id: "fi1".into(),
        flow: "prod-logs".into(),
        trigger: "when".into(),
        event: None,
        status,
        started_at: "2026-09-10T11:00:00Z".parse().unwrap(),
        ended_at: None,
        outcome: None,
        steps: steps
            .iter()
            .map(|(name, status)| rl_proto::flow::StepState {
                name: name.to_string(),
                status: *status,
                run: None,
                decision: None,
            })
            .collect(),
    }
}

#[test]
fn a_flow_row_says_where_its_firing_stands() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let d = digest(
        now,
        vec![
            fired("probe", "prod-logs", RunState::Finished, "2026-09-10T11:01:00Z", false),
            fired("triage", "prod-logs", RunState::Finished, "2026-09-10T11:02:00Z", false),
        ],
        vec![firing(
            FiringStatus::Waiting,
            &[
                ("probe", StepStatus::Done),
                ("triage", StepStatus::Done),
                ("review", StepStatus::Waiting),
            ],
        )],
    );
    let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
    let flows = [flow("prod-logs", Some("/home/f/relay"))];
    let rows = flows_build(&d, &names, &flows, "", now);
    let Some(Row::Flow(f)) = rows.iter().find(|r| matches!(r, Row::Flow(_))) else {
        panic!("no flow row: {:?}", keys(&rows));
    };
    assert_eq!(f.runs.len(), 2);
    let at = f.firing.expect("its latest firing");
    assert_eq!(flows::state_words(at), "waiting at review");
    // A flow that has not fired since the digest knew it says nothing.
    let d = digest(now, d.items.clone(), vec![]);
    let rows = flows_build(&d, &names, &flows, "", now);
    assert!(rows.iter().any(|r| matches!(r, Row::Flow(f) if f.firing.is_none())));
}

#[test]
fn a_flow_row_sits_in_its_in_project_wherever_its_runs_ran() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let (api, other) = ("/w/api", "/w/other");
    let mut ran = item("ran", RunState::Finished, other, "2026-09-10T11:00:00Z", false);
    ran.run.cause = step_of("prod-logs");
    let d = digest(now, vec![ran], vec![]);
    let names = Names::new([
        (PathBuf::from(api), "api".to_string()),
        (PathBuf::from(other), "other".to_string()),
    ]);
    let flows = [flow("prod-logs", Some(api))];
    assert_eq!(
        keys(&flows_build(&d, &names, &flows, "", now)),
        ["[api]", "f:prod-logs", "[other]"],
        "under the project it is in, not the folder its run ran in"
    );
    assert_eq!(group_key(&d.items[0].run, &names, &flows), api);
    // Not loaded now, where it is `in` is not known: everywhere.
    assert_eq!(
        keys(&flows_build(&d, &names, &[], "", now)),
        ["[api]", "[other]", "[everywhere]", "f:prod-logs"]
    );
    // With no `in`, everywhere too.
    assert_eq!(
        keys(&flows_build(&d, &names, &[flow("prod-logs", None)], "", now)),
        ["[api]", "[other]", "[everywhere]", "f:prod-logs"]
    );
    // Hiding the project it is in hides it with the project.
    let mut hidden = d.clone();
    hidden.hidden = vec![hid(&format!("g:{api}"), "2026-09-10T11:30:00Z")];
    assert_eq!(keys(&flows_build(&hidden, &names, &flows, "", now)), ["[other]"]);
}

#[test]
fn a_flow_with_a_yes_has_its_row_before_it_ever_fired() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    let d = digest(now, vec![], vec![]);
    let nightly = Flow {
        next_at: Some("2026-09-10T12:12:00Z".parse().unwrap()),
        ..flow("nightly", Some(relay))
    };
    let flows = [nightly.clone()];
    for t in [(true, true, true, false), (true, false, true, false)] {
        assert_eq!(
            keys(&toggles_build(&d, &names, &flows, t, "", now)),
            ["[relay]", "f:nightly"],
            "{t:?}"
        );
    }
    assert_eq!(
        keys(&toggles_build(&d, &names, &flows, (false, true, true, false), "", now)),
        ["[relay]"],
        "flows off"
    );
    let rows = flows_build(&d, &names, &flows, "", now);
    let Some(Row::Flow(f)) = rows.iter().find(|r| matches!(r, Row::Flow(_))) else {
        panic!("no flow row");
    };
    assert!(f.runs.is_empty() && f.last().is_none());
    assert_eq!(flows::stands(f, flows.first(), now), "every 30m · next in 12m");
    assert_eq!(f.marks(10), "");
    assert_eq!(counts(&rows), (0, 0, 0, 0));

    // Paused, or changed since its yes, it says so.
    let says = |x: Flow| {
        let flows = [x];
        flows_build(&d, &names, &flows, "", now).iter().find_map(|r| match r {
            Row::Flow(f) => Some(flows::stands(f, flows.first(), now)),
            _ => None,
        })
    };
    let paused = Flow {
        paused: true,
        ..nightly.clone()
    };
    assert_eq!(says(paused).as_deref(), Some("paused · every 30m"));
    let changed = Flow {
        hash: "changed".into(),
        ..nightly.clone()
    };
    assert_eq!(says(changed).as_deref(), Some("needs a yes again · every 30m"));

    // Never said yes to: still a row, where you approve it — it says so.
    // Removed, with no run of its own still on the list: no row.
    let waiting = Flow {
        approved_hash: None,
        ..nightly.clone()
    };
    assert_eq!(says(waiting).as_deref(), Some("needs a yes · every 30m"));
    let old = fired("old", "nightly", RunState::Finished, "2026-09-07T12:00:00Z", false);
    let d = digest(now, vec![old], vec![]);
    let removed = [Flow {
        removed_at: Some(now),
        ..nightly
    }];
    assert_eq!(keys(&flows_build(&d, &names, &removed, "", now)), ["[relay]"]);
}

#[test]
fn every_defined_flow_stands_as_a_row_even_before_a_yes() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    let d = digest(now, vec![], vec![]);
    // A flow still waiting for its first yes: it has a row of its own —
    // that is where you see it and approve it, not a header chip.
    let waiting = [Flow {
        approved_hash: None,
        ..flow("deploy", Some(relay))
    }];
    let rows = flows_build(&d, &names, &waiting, "", now);
    assert_eq!(keys(&rows), ["[relay]", "f:deploy"]);
    // Flows off takes it away like any flow row.
    assert_eq!(
        keys(&toggles_build(
            &d,
            &names,
            &waiting,
            (false, true, true, false),
            "",
            now
        )),
        ["[relay]"]
    );
    // The row says it needs a yes, with what starts it.
    let Some(Row::Flow(f)) = rows.iter().find(|r| matches!(r, Row::Flow(_))) else {
        panic!("no flow row");
    };
    assert_eq!(flows::stands(f, waiting.first(), now), "needs a yes · every 30m");
}

#[test]
fn a_project_hints_when_something_hidden_needs_you() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    // Hidden once it had already failed, so it stays off the list and does
    // not come through — it is genuinely buried.
    let mut failed = item("failed", RunState::Failed, relay, "2026-09-10T11:00:00Z", false);
    failed.run.hidden_at = Some("2026-09-10T11:30:00Z".parse().unwrap());
    let working = item("work", RunState::Working, relay, "2026-09-10T11:40:00Z", false);
    let d = digest(now, vec![failed, working], vec![]);
    let rows = build_with(&d, &names, "", now);
    assert!(
        !keys(&rows).contains(&"r:failed".to_string()),
        "the failed run is off the list"
    );
    assert_eq!(
        head_of(&rows, "relay").attention,
        1,
        "the header hints that one needs you"
    );
    // Hidden on: it stands inline, so nothing is buried and no hint.
    let shown = toggles_build(&d, &names, &[], (true, true, true, true), "", now);
    assert_eq!(head_of(&shown, "relay").attention, 0);
}

#[test]
fn agents_off_hints_a_needy_agent_on_its_project() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    // An agent waiting on you, recent so it is live.
    let needy = item("ask", RunState::NeedsInput, relay, "2026-09-10T11:50:00Z", false);
    let d = digest(now, vec![needy], vec![]);
    // Agents on: it shows, nothing buried.
    let on = toggles_build(&d, &names, &[], (true, true, true, false), "", now);
    assert!(keys(&on).contains(&"r:ask".to_string()));
    assert_eq!(head_of(&on, "relay").attention, 0);
    // Agents off: it is off the list, and the header hints.
    let off = toggles_build(&d, &names, &[], (true, false, true, false), "", now);
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
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    // A plain warm shell, nothing started it.
    let mut shell = item("sh", RunState::Working, relay, "2026-09-10T11:50:00Z", false);
    shell.run.kind = RunKind::Shell;
    let d = digest(now, vec![shell], vec![]);
    // Shells on: it is on the list.
    let on = toggles_build(&d, &names, &[], (true, true, true, false), "", now);
    assert!(keys(&on).contains(&"r:sh".to_string()));
    // Shells off: it is gone.
    let off = toggles_build(&d, &names, &[], (true, true, false, false), "", now);
    assert!(!keys(&off).contains(&"r:sh".to_string()));
}

#[test]
fn shells_off_hints_a_needy_shell_on_its_project() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    // A shell blocked on you, recent so it is live.
    let mut needy = item("sh", RunState::Blocked, relay, "2026-09-10T11:50:00Z", false);
    needy.run.kind = RunKind::Shell;
    let d = digest(now, vec![needy], vec![]);
    // Shells on: it shows, nothing buried.
    let on = toggles_build(&d, &names, &[], (true, true, true, false), "", now);
    assert!(keys(&on).contains(&"r:sh".to_string()));
    assert_eq!(head_of(&on, "relay").attention, 0);
    // Shells off: it is off the list, and the header hints it needs you.
    let off = toggles_build(&d, &names, &[], (true, true, false, false), "", now);
    assert!(!keys(&off).contains(&"r:sh".to_string()));
    assert_eq!(head_of(&off, "relay").attention, 1);
}

#[test]
fn a_flows_row_keeps_its_history_however_old() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    let days_ago = "2026-09-07T12:00:00Z";
    let d = digest(
        now,
        vec![
            fired("old", "pulse", RunState::Failed, days_ago, false),
            item("hand", RunState::Finished, relay, days_ago, false),
        ],
        vec![],
    );
    let flows = [flow("pulse", Some(relay))];
    let rows = flows_build(&d, &names, &flows, "", now);
    assert_eq!(
        keys(&rows),
        ["[relay]", "f:pulse"],
        "a run by hand that old is off the list"
    );
    let Some(Row::Flow(f)) = rows.iter().find(|r| matches!(r, Row::Flow(_))) else {
        panic!("no flow row");
    };
    assert_eq!(f.marks(10), "×", "its run of three days ago");
    // A flow not loaded now has no row of its own: its old runs go as
    // any other.
    assert_eq!(keys(&flows_build(&d, &names, &[], "", now)), ["[relay]"]);
}

#[test]
fn a_search_and_hide_take_an_unfired_flows_row_as_any_other() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    let d = digest(now, vec![], vec![]);
    let flows = [flow("nightly", Some(relay))];
    let shown = |d: &Digest, filter: &str| keys(&flows_build(d, &names, &flows, filter, now));
    let hidden_on = |d: &Digest| keys(&toggles_build(d, &names, &flows, (true, true, true, true), "", now));
    assert_eq!(shown(&d, "night"), ["[relay]", "f:nightly"]);
    assert_eq!(shown(&d, "kind:flow"), ["[relay]", "f:nightly"]);
    assert_eq!(shown(&d, "by:nightly"), ["[relay]", "f:nightly"]);
    assert_eq!(shown(&d, "in:relay kind:flow"), ["[relay]", "f:nightly"]);
    assert_eq!(shown(&d, "kind:agent"), Vec::<String>::new());
    assert_eq!(shown(&d, "tidy"), Vec::<String>::new());

    // `/hide` on it, or on its project: off the list, back only with
    // hidden on.
    for key in ["f:nightly".to_string(), format!("g:{relay}")] {
        let mut hidden = d.clone();
        hidden.hidden = vec![hid(&key, "2026-09-10T11:00:00Z")];
        assert!(!shown(&hidden, "").contains(&"f:nightly".to_string()), "{key}");
        assert_eq!(hidden_on(&hidden), ["[relay]", "f:nightly"], "{key}");
    }
    // Hidden on with nothing hidden is just the list as it stands.
    assert_eq!(hidden_on(&d), ["[relay]", "f:nightly"], "nothing hidden");
}

#[test]
fn every_flow_stands_at_the_bottom_of_its_project_by_name_and_stays_there() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    let d = digest(
        now,
        vec![fired("p1", "pulse", RunState::Finished, "2026-09-10T11:00:00Z", false)],
        vec![],
    );
    let flows = [
        flow("beta", Some(relay)),
        flow("pulse", Some(relay)),
        flow("alpha", Some(relay)),
    ];
    assert_eq!(
        keys(&flows_build(&d, &names, &flows, "", now)),
        ["[relay]", "f:alpha", "f:beta", "f:pulse"],
        "by name, whether or not they ran"
    );

    // Under the runs, whatever order was kept — one that names a flow above a run among it —
    // and a flow does not move, nor a run down past the last of the runs.
    let mut d = digest(
        now,
        vec![
            fired("p1", "pulse", RunState::Finished, "2026-09-10T11:00:00Z", false),
            item("work", RunState::Working, relay, "2026-09-10T11:30:00Z", false),
        ],
        vec![],
    );
    d.order = kept(&["f:pulse", "r:work"]);
    let rows = flows_build(&d, &names, &flows, "", now);
    assert_eq!(keys(&rows), ["[relay]", "r:work", "f:alpha", "f:beta", "f:pulse"]);
    assert_eq!(
        reorder(&rows, &rows, &d.order, "f:pulse", true),
        Err("flows stay at the bottom of their project, by name")
    );
    assert_eq!(
        reorder(&rows, &rows, &d.order, "r:work", false),
        Err("already at the bottom of its project")
    );
}

#[test]
fn words_for_causes_and_askers() {
    assert_eq!(cause_words(&Cause::Manual), "started by hand");
    assert_eq!(
        cause_words(&Cause::Flow {
            flow: "nightly".into(),
            step: "probe".into()
        }),
        "flow nightly · probe"
    );
    let q = question("q", "/w", None, "2026-09-10T11:01:00Z");
    let run = item("r", RunState::Failed, "/w", "2026-09-10T11:00:00Z", false).run;
    assert_eq!(asked_by(&q, Some(&run), None), "title r");
    assert_eq!(asked_by(&q, None, None), "you");
    // A flow's ask step: its firing is the question's chain, and the step
    // that asks is the one holding it.
    let mut asking = Firing {
        id: "q".into(),
        ..firing(
            FiringStatus::Waiting,
            &[("probe", StepStatus::Done), ("review", StepStatus::Waiting)],
        )
    };
    let firings = [asking.clone()];
    assert_eq!(firing_of(&q, &firings).map(|f| f.id.as_str()), Some("q"));
    assert_eq!(asked_by(&q, None, firing_of(&q, &firings)), "flow prod-logs · review");
    asking.steps[1].status = StepStatus::Done;
    asking.steps[1].decision = Some("q".into());
    assert_eq!(asked_by(&q, None, Some(&asking)), "flow prod-logs · review");
    assert!(firing_of(&question("other", "/w", None, "2026-09-10T11:01:00Z"), &firings).is_none());
}

#[test]
fn a_groups_header_carries_the_flows_in_its_folder() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let next = |name: &str, in_: Option<&str>, at: &str| Flow {
        next_at: Some(at.parse().unwrap()),
        ..flow(name, in_)
    };
    let flows = vec![
        next("nightly", Some(relay), "2026-09-10T16:00:00Z"),
        flow("tests-failed", Some(relay)),
        Flow {
            approved_hash: None,
            ..flow("deploy", Some(relay))
        },
        Flow {
            paused: true,
            ..flow("tidy", Some(relay))
        },
        flow("elsewhere", Some("/home/f/api")),
        next("pulse", None, "2026-09-10T12:00:30Z"),
    ];
    let texts = |c: &[Chip]| c.iter().map(Chip::text).collect::<Vec<_>>();
    assert_eq!(
        texts(&chips(&flows, Some(Path::new(relay)), now)),
        ["! deploy", "↻ nightly 4h", "↻ tests-failed", "↻ tidy paused"],
        "what needs a yes first, then by name; api's flow stays out"
    );
    assert_eq!(texts(&chips(&flows, None, now)), ["↻ pulse 30s"]);

    // On the list: the folder's flows ride its header as the tally, and
    // every flow defined has its row under it, run or not — the one
    // waiting for its first yes among them; those with no `in` stand
    // under `everywhere`.
    let d = digest(
        now,
        vec![item("work1", RunState::Working, relay, "2026-09-10T11:59:00Z", false)],
        vec![],
    );
    let names = Names::new([(relay.into(), "relay".to_string())]);
    let build_flows = |flows: &[Flow]| {
        flows_build(&d, &names, flows, "", now)
            .iter()
            .map(|r| match r {
                Row::Group(h) => format!("[{}] {}", h.name, h.tally.words()),
                other => other.key().unwrap(),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        build_flows(&flows),
        [
            "[api] 1 flow",
            "f:elsewhere",
            "[relay] 4 flows · 1 needs you",
            "r:work1",
            "f:deploy",
            "f:nightly",
            "f:tests-failed",
            "f:tidy",
            "[everywhere] 1 flow",
            "f:pulse"
        ],
        "what has run first, then the rest by name"
    );
}

#[test]
fn a_row_wears_what_it_is_and_says_how_it_stands() {
    let mut i = item("sh", RunState::Idle, "/w", "2026-09-10T11:00:00Z", false);
    i.run.kind = RunKind::Shell;
    assert!(live_shell(&i.run));
    assert_eq!(icon(&i.run), SHELL);
    assert_eq!(
        run_words(&i.run),
        "",
        "a shell at rest says nothing: its icon says what it is"
    );
    // One that ended is news like any other ended run, and reads as one.
    i.run.state = RunState::Stopped;
    assert!(!live_shell(&i.run));
    assert_eq!((icon(&i.run), run_words(&i.run)), (SHELL, "stopped".to_string()));
    // So is one that wants you: the word says so.
    i.run.state = RunState::NeedsInput;
    assert!(!live_shell(&i.run));
    assert_eq!(run_words(&i.run), "needs you");
    // A warm shell is one by its tag, whatever its argv resolved to.
    let mut warm = item("warm", RunState::Idle, "/w", "2026-09-10T11:00:00Z", false);
    warm.run.kind = RunKind::Command;
    warm.run.tags = vec![SHELL_TAG.into()];
    assert_eq!((icon(&warm.run), run_words(&warm.run)), (SHELL, String::new()));
    // An agent says its state; a command, a plugin too, is a wrench.
    let mut a = item("ag", RunState::Idle, "/w", "2026-09-10T11:00:00Z", false);
    assert_eq!((icon(&a.run), run_words(&a.run)), (AGENT, "idle".to_string()));
    a.run.kind = RunKind::Command;
    a.run.state = RunState::Finished;
    assert_eq!((icon(&a.run), run_words(&a.run)), (COMMAND, "done".to_string()));
    a.run.kind = RunKind::Plugin;
    assert_eq!(icon(&a.run), COMMAND);
}

#[test]
fn an_agent_wears_its_clis_glyph_in_its_clis_colour() {
    use ratatui::style::{Color, Style};
    let mut a = item("ag", RunState::Working, "/w", "2026-09-10T11:00:00Z", false);
    let mut look = |cli: Option<&str>| {
        a.run.cli = cli.map(String::from);
        let i = icon(&a.run);
        (i, icon_style(i, cli, false))
    };
    assert_eq!(
        look(Some("claude")),
        ("✻", Style::default().fg(Color::Rgb(0xD9, 0x77, 0x57)))
    );
    assert_eq!(look(Some("codex")), ("⬡", Style::default()));
    assert_eq!(look(Some("gemini")).0, "✧");
    assert_eq!(look(Some("cursor")).0, "❖");
    assert_eq!(look(Some("opencode")).0, "⊡");
    // One Relay has no profile for, or none at all, is a plain agent.
    assert_eq!(look(Some("fake-cli")), (AGENT, Style::default()));
    assert_eq!(look(None), (AGENT, Style::default()));
    // Ended, it is quiet whatever it is.
    assert_eq!(icon_style("✻", Some("claude"), true), crate::tui::theme::dim());
    for l in looks().values() {
        if let Some(i) = &l.icon {
            assert_eq!(ratatui::text::Span::raw(i.as_str()).width(), 1, "{i}");
        }
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
fn every_icon_is_one_quiet_column() {
    use ratatui::text::Span;
    for i in [AGENT, SHELL, COMMAND, FLOW, QUESTION] {
        assert_eq!(Span::raw(i).width(), 1, "{i}");
        assert_eq!(i.chars().count(), 1, "{i} carries no variation selector");
    }
    assert_eq!(icon_style(SHELL, None, false), crate::tui::theme::accent());
    assert_eq!(icon_style(QUESTION, None, false), crate::tui::theme::needs());
    assert_eq!(icon_style(SHELL, None, true), crate::tui::theme::dim());
}

#[test]
fn a_header_is_its_path_and_a_name_of_its_own() {
    let names = Names::new([
        (PathBuf::from("/home/f/api"), "api".to_string()),
        (PathBuf::from("/home/f/scratch-2"), "scratch".to_string()),
    ]);
    let own = |k: &str| names.own(k, Path::new(if k.starts_with('/') { k } else { "/home/f/api" }));
    assert_eq!(own("/home/f/api"), None, "named for its folder: nothing to add");
    assert_eq!(own("/home/f/scratch-2").as_deref(), Some("scratch"), "renamed");
    assert_eq!(own("/home/f/web"), None, "never registered");
    assert_eq!(own("RLX-12").as_deref(), Some("RLX-12"), "a ref");

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
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let ws = "/home/f/relay";
    let names = Names::new([(PathBuf::from(ws), "relay".to_string())]);
    let mut shell = item("shell", RunState::Idle, ws, "2026-09-10T11:20:00Z", false);
    shell.run.kind = RunKind::Shell;
    let asks = item("asks", RunState::NeedsInput, ws, "2026-09-10T11:10:00Z", false);
    let work = item("work", RunState::Working, ws, "2026-09-10T11:55:00Z", false);
    let d = Digest {
        since: now,
        items: vec![work, shell, asks],
        decisions: vec![],
        checkouts: Vec::new(),
        lost: Vec::new(),
        hidden: Vec::new(),
        order: Vec::new(),
        firings: Vec::new(),
        outside: vec![],
    };
    // Neither where you work nor what asks for you jumps ahead: each in
    // the order it started.
    assert_eq!(
        keys(&build_with(&d, &names, "", now)),
        ["[relay]", "r:asks", "r:shell", "r:work"]
    );
}

#[test]
fn a_flows_runs_outside_every_project_are_everywhere() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let at = "2026-09-10T11:00:00Z";
    let (home, relay, web) = ("/home/f", "/home/f/relay", "/home/f/web");
    let by = |id: &str, state: RunState, ws: &str, cause: Cause| {
        let mut i = item(id, state, ws, at, false);
        i.run.cause = cause;
        i
    };
    let d = digest(
        now,
        vec![
            // A flow with no `in`, in home and in a project: one row.
            by("mentions", RunState::Finished, home, step_of("gchat-mentions")),
            by("mentions2", RunState::Failed, home, step_of("gchat-mentions")),
            by("inproj", RunState::Finished, relay, step_of("gchat-mentions")),
            // A flow's run still going, outside every project.
            by("fix", RunState::Working, home, step_of("look-at-it")),
            // A flow `in` a folder that is not a registered project.
            by("webs", RunState::Finished, web, step_of("pulse")),
            // By hand, in home.
            by("hand", RunState::Working, home, Cause::Manual),
        ],
        vec![],
    );
    let flows = vec![
        flow("gchat-mentions", None),
        flow("look-at-it", None),
        flow("pulse", Some(web)),
    ];
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    let shown = |filter: &str| keys(&flows_build(&d, &names, &flows, filter, now));
    let home_name = Names::default().workspace(Path::new(home));
    assert_eq!(
        shown(""),
        [
            // By hand, a run keeps its folder.
            format!("[{home_name}]"),
            "r:hand".into(),
            "[relay]".into(),
            // A flow's row is in the folder it is `in`.
            "[web]".into(),
            "f:pulse".into(),
            // The rest, after the projects, however they sort.
            "[everywhere]".into(),
            "r:fix".into(),
            "f:gchat-mentions".into(),
            // Its run still going is its own row; the flow has its row
            // anyway, quiet, until that run ends and folds into it.
            "f:look-at-it".into(),
        ],
        "outside every project: everywhere, after the projects"
    );

    // It answers to its word, as a project answers to its name.
    assert_eq!(
        shown("in:everywhere"),
        ["[everywhere]", "r:fix", "f:gchat-mentions", "f:look-at-it"]
    );

    // Its header is no folder, and its key says so.
    let rows = flows_build(&d, &names, &flows, "", now);
    let head = rows
        .iter()
        .find_map(|r| match r {
            Row::Group(h) if h.is_everywhere() => Some(h.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (head.place.as_str(), head.workspace.as_os_str().is_empty()),
        ("everywhere", true)
    );
    assert_eq!(Row::Group(head).key().as_deref(), Some("g:everywhere"));

    // A flow not loaded now leaves its row everywhere: where it is `in`
    // is not known. A run by hand keeps its folder.
    let bare = keys(&flows_build(&d, &names, &[], "", now));
    let at = |k: &str| {
        bare.iter()
            .position(|x| x == k)
            .unwrap_or_else(|| panic!("{k}: {bare:?}"))
    };
    assert!(at("[everywhere]") < at("f:gchat-mentions"));
    assert!(at("[everywhere]") < at("f:pulse"));
    assert!(at(&format!("[{home_name}]")) < at("r:hand"));
}

/// One project of four runs, started a minute apart in the order of
/// their names (`one` first, `four` last), whatever they came to; and
/// another project of one.
fn four(order: Vec<Order>) -> (Digest, Names) {
    let (api, relay) = ("/home/f/api", "/home/f/relay");
    let d = Digest {
        since: "2026-09-10T09:00:00Z".parse().unwrap(),
        items: vec![
            item("one", RunState::Working, relay, "2026-09-10T10:00:00Z", false),
            item("two", RunState::Working, relay, "2026-09-10T10:01:00Z", false),
            item("three", RunState::Idle, relay, "2026-09-10T10:02:00Z", false),
            item("four", RunState::Finished, relay, "2026-09-10T10:03:00Z", false),
            item("there", RunState::Working, api, "2026-09-10T10:00:00Z", false),
        ],
        decisions: Vec::new(),
        checkouts: Vec::new(),
        lost: Vec::new(),
        hidden: Vec::new(),
        order,
        firings: Vec::new(),
        outside: vec![],
    };
    let names = Names::new([
        (PathBuf::from(api), "api".to_string()),
        (PathBuf::from(relay), "relay".to_string()),
    ]);
    (d, names)
}

fn kept(rows: &[&str]) -> Vec<Order> {
    vec![Order {
        group: "g:/home/f/relay".into(),
        rows: rows.iter().map(|s| s.to_string()).collect(),
    }]
}

/// What becomes of a run later: its state, and when that came.
fn comes_to(d: &mut Digest, id: &str, state: RunState, at: &str) {
    let t: Timestamp = at.parse().unwrap();
    let i = d.items.iter_mut().find(|i| i.run.id == id).unwrap();
    i.run.state = state;
    i.run.state_at = t;
    i.at = t;
    i.unseen = true;
}

const NOW: &str = "2026-09-10T12:00:00Z";

#[test]
fn a_project_stands_in_the_order_its_rows_came_until_you_put_it_in_yours() {
    let now: Timestamp = NOW.parse().unwrap();
    // Nothing kept: the order they started in, the newest at the bottom,
    // whatever state each is in.
    let (d, names) = four(Vec::new());
    assert_eq!(
        keys(&build_with(&d, &names, "", now)),
        ["[api]", "r:there", "[relay]", "r:one", "r:two", "r:three", "r:four"]
    );
    // An order kept for a project is its order; the other project's own
    // is untouched, and an empty one is no order at all.
    let (d, names) = four(kept(&["r:one", "r:three", "r:two", "r:four"]));
    assert_eq!(
        keys(&build_with(&d, &names, "", now)),
        ["[api]", "r:there", "[relay]", "r:one", "r:three", "r:two", "r:four"]
    );
    let (d, names) = four(kept(&[]));
    assert_eq!(
        keys(&build_with(&d, &names, "", now))[3..],
        ["r:one", "r:two", "r:three", "r:four"]
    );
    // What has left the list since is no matter: the rest keep theirs.
    let (d, names) = four(kept(&["r:gone", "r:one", "r:three", "r:two", "r:four"]));
    assert_eq!(
        keys(&build_with(&d, &names, "", now))[3..],
        ["r:one", "r:three", "r:two", "r:four"]
    );
}

#[test]
fn a_row_stays_where_it_stands_whatever_comes_of_it() {
    let now: Timestamp = NOW.parse().unwrap();
    for (order, stands) in [
        (Vec::new(), ["r:one", "r:two", "r:three", "r:four"]),
        (
            kept(&["r:one", "r:three", "r:two", "r:four"]),
            ["r:one", "r:three", "r:two", "r:four"],
        ),
    ] {
        let (mut d, names) = four(order);
        // It asks, another fails, another blocks, the last finishes:
        // each says so where it is, and none of them moves.
        comes_to(&mut d, "two", RunState::NeedsInput, "2026-09-10T11:50:00Z");
        comes_to(&mut d, "four", RunState::Failed, "2026-09-10T11:51:00Z");
        comes_to(&mut d, "three", RunState::Blocked, "2026-09-10T11:52:00Z");
        comes_to(&mut d, "one", RunState::Finished, "2026-09-10T11:53:00Z");
        assert_eq!(keys(&build_with(&d, &names, "", now))[3..], stands);
        // Answered, nothing moves either.
        comes_to(&mut d, "two", RunState::Working, "2026-09-10T11:55:00Z");
        assert_eq!(keys(&build_with(&d, &names, "", now))[3..], stands);
    }
    // A shell is a row as any other: where it came, not first.
    let (mut d, names) = four(Vec::new());
    d.items[2].run.kind = RunKind::Shell;
    assert_eq!(
        keys(&build_with(&d, &names, "", now))[3..],
        ["r:one", "r:two", "r:three", "r:four"]
    );
}

#[test]
fn a_new_run_enters_at_the_bottom() {
    let now: Timestamp = NOW.parse().unwrap();
    let new =
        |d: &mut Digest, id: &str, at: &str| d.items.push(item(id, RunState::Working, "/home/f/relay", at, false));
    let (mut d, names) = four(Vec::new());
    new(&mut d, "new", "2026-09-10T11:00:00Z");
    assert_eq!(
        keys(&build_with(&d, &names, "", now))[3..],
        ["r:one", "r:two", "r:three", "r:four", "r:new"]
    );
    // Below your order too, in the order they came.
    let (mut d, names) = four(kept(&["r:one", "r:three", "r:two", "r:four"]));
    new(&mut d, "newer", "2026-09-10T11:30:00Z");
    new(&mut d, "new", "2026-09-10T11:00:00Z");
    let rows = build_with(&d, &names, "", now);
    assert_eq!(
        keys(&rows)[3..],
        ["r:one", "r:three", "r:two", "r:four", "r:new", "r:newer"]
    );
    // Moved, it takes its place in your order, and the rest keep theirs.
    assert_eq!(
        reorder(&rows, &rows, &d.order, "r:new", true).unwrap().rows,
        ["r:one", "r:three", "r:two", "r:new", "r:four", "r:newer"]
    );
}

#[test]
fn a_question_stands_under_the_run_that_asked_it() {
    let now: Timestamp = NOW.parse().unwrap();
    let relay = "/home/f/relay";
    let asked = |order: Vec<Order>| {
        let (mut d, names) = four(order);
        comes_to(&mut d, "two", RunState::NeedsInput, "2026-09-10T11:00:00Z");
        d.decisions = vec![
            // Asked by no run that is on the list: at the bottom of its
            // project, though it was asked first.
            question("q0", relay, None, "2026-09-10T09:30:00Z"),
            question("q1", relay, Some("two"), "2026-09-10T11:00:00Z"),
            // A folder with nothing else on the list has its header for it.
            question("q2", "/home/f/web", None, "2026-09-10T11:10:00Z"),
        ];
        (d, names)
    };
    let (d, names) = asked(Vec::new());
    let rows = build_with(&d, &names, "", now);
    // No questions above the projects: each in its own.
    assert_eq!(
        keys(&rows),
        [
            "[api]", "r:there", "[relay]", "r:one", "r:two", "q:q1", "r:three", "r:four", "q:q0", "[web]", "q:q2"
        ]
    );
    assert_eq!(counts(&rows), (3, 1, 3, 1));
    // In your order, it goes where its run goes.
    let (d, names) = asked(kept(&["r:two", "r:one", "r:three", "r:four"]));
    let rows = build_with(&d, &names, "", now);
    assert_eq!(
        keys(&rows)[3..9],
        ["r:two", "q:q1", "r:one", "r:three", "r:four", "q:q0"]
    );
    // A row moves past a question as past nothing, and the question is
    // not part of the order kept.
    assert_eq!(
        reorder(&rows, &rows, &d.order, "r:one", true).unwrap().rows,
        ["r:one", "r:two", "r:three", "r:four"]
    );
    assert_eq!(
        reorder(&rows, &rows, &d.order, "r:two", false).unwrap().rows,
        ["r:one", "r:two", "r:three", "r:four"]
    );
    assert_eq!(
        reorder(&rows, &rows, &d.order, "r:four", false),
        Err("already at the bottom of its project")
    );
    assert_eq!(
        reorder(&rows, &rows, &d.order, "q:q1", true),
        Err("a question stays under the run that asked it")
    );
    // A toggle leaves its run out, never the question.
    let flows = lens_build(&d, &names, (true, false, true, false), now);
    assert_eq!(keys(&flows), ["[api]", "[relay]", "q:q0", "q:q1", "[web]", "q:q2"]);
}

#[test]
fn a_row_moves_one_place_inside_its_project_and_no_further() {
    let now: Timestamp = NOW.parse().unwrap();
    let (mut d, names) = four(Vec::new());
    let mut pinned = item(
        "pinned",
        RunState::Working,
        "/home/f/relay",
        "2026-09-10T11:00:00Z",
        false,
    );
    pinned.run.pinned_at = Some("2026-09-10T11:00:00Z".parse().unwrap());
    d.items.push(pinned);
    let rows = build_with(&d, &names, "", now);
    assert_eq!(
        keys(&rows),
        [
            "r:pinned", "[api]", "r:there", "[relay]", "r:one", "r:two", "r:three", "r:four"
        ]
    );
    let mv = |key: &str, up: bool| reorder(&rows, &rows, &d.order, key, up);
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
    // Never out of its project, nor into the band, nor anything but a row.
    assert_eq!(mv("r:one", true), Err("already at the top of its project"));
    assert_eq!(mv("r:four", false), Err("already at the bottom of its project"));
    assert_eq!(mv("r:there", false), Err("already at the bottom of its project"));
    assert_eq!(mv("r:there", true), Err("already at the top of its project"));
    assert!(mv("r:pinned", false).unwrap_err().starts_with("a pinned run"));
    assert_eq!(mv("r:nope", true), Err("nothing here to move"));

    // What needs you moves as anything else, and is passed as anything else.
    let (mut d, names) = four(kept(&["r:one", "r:three", "r:two", "r:four"]));
    comes_to(&mut d, "two", RunState::Blocked, "2026-09-10T11:00:00Z");
    let rows = build_with(&d, &names, "", now);
    assert_eq!(keys(&rows)[3..], ["r:one", "r:three", "r:two", "r:four"]);
    assert_eq!(
        reorder(&rows, &rows, &d.order, "r:two", true).unwrap().rows,
        ["r:one", "r:two", "r:three", "r:four"]
    );
    assert_eq!(
        reorder(&rows, &rows, &d.order, "r:four", true).unwrap().rows,
        ["r:one", "r:three", "r:four", "r:two"]
    );
}

#[test]
fn a_project_moves_up_and_down_among_the_projects_and_keeps_its_place() {
    let now: Timestamp = NOW.parse().unwrap();
    let (api, relay, web) = ("g:/home/f/api", "g:/home/f/relay", "g:/home/f/web");
    let (mut d, _) = four(Vec::new());
    // A flow's run outside every project: `everywhere`, after them all.
    let mut fix = item("fix", RunState::Working, "/home/f", "2026-09-10T11:00:00Z", false);
    fix.run.cause = step_of("look-at-it");
    d.items.push(fix);
    let names = |more: &[(&str, &str)]| {
        let mut all = vec![
            (PathBuf::from("/home/f/api"), "api".to_string()),
            (PathBuf::from("/home/f/relay"), "relay".to_string()),
            (PathBuf::from("/home/f/web"), "web".to_string()),
        ];
        all.extend(more.iter().map(|(p, n)| (PathBuf::from(p), n.to_string())));
        Names::new(all)
    };
    let heads = |d: &Digest, names: &Names| -> Vec<String> {
        keys(&flows_build(d, names, &[], "", now))
            .into_iter()
            .filter(|k| k.starts_with('['))
            .collect()
    };
    let three = names(&[]);
    // By name until you move one.
    assert_eq!(heads(&d, &three), ["[api]", "[relay]", "[web]", "[everywhere]"]);
    let rows = flows_build(&d, &three, &[], "", now);
    let mv = |key: &str, up: bool| reorder(&rows, &rows, &d.order, key, up);
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
    assert_eq!(
        mv("g:everywhere", true),
        Err("everywhere stays last, below the projects")
    );

    // Kept, the projects stand in it, each with what is under it; a
    // project's own rows keep theirs.
    d.order = vec![Order {
        group: Order::PROJECTS.into(),
        rows: [relay, api, web].map(String::from).to_vec(),
    }];
    assert_eq!(
        keys(&flows_build(&d, &three, &[], "", now)),
        [
            "[relay]",
            "r:one",
            "r:two",
            "r:three",
            "r:four",
            "[api]",
            "r:there",
            "[web]",
            "[everywhere]",
            "r:fix"
        ]
    );
    // A project the order does not know — new since — enters at the
    // bottom, by name, however its name sorts; everywhere stays last.
    let four_of = names(&[("/home/f/aaa", "aaa")]);
    assert_eq!(
        heads(&d, &four_of),
        ["[relay]", "[api]", "[web]", "[aaa]", "[everywhere]"]
    );
    // Moving builds on the order kept, and takes the new one into it.
    let rows = flows_build(&d, &four_of, &[], "", now);
    assert_eq!(
        reorder(&rows, &rows, &d.order, "g:/home/f/aaa", true).unwrap().rows,
        [relay, api, "g:/home/f/aaa", web]
    );
    // One the list does not show now — hidden, or gone — keeps its place.
    d.order[0].rows.insert(1, "g:/home/f/gone".into());
    let rows = flows_build(&d, &three, &[], "", now);
    assert_eq!(
        reorder(&rows, &rows, &d.order, api, true).unwrap().rows,
        [api, relay, "g:/home/f/gone", web]
    );
    // A search that leaves projects out moves among what it shows.
    let (shown, full) = (
        flows_build(&d, &three, &[], "in:web", now),
        flows_build(&d, &three, &[], "", now),
    );
    assert_eq!(
        reorder(&shown, &full, &d.order, web, true),
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
    assert_eq!(fit("héllo wörld", 6), "héllo…");
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

/// One of Claude's own sessions in `cwd`, as the digest carries it.
fn claude_in(id: &str, cwd: &str, state: Option<&str>, status: &str) -> Outside {
    Outside {
        cwd: PathBuf::from(cwd),
        ..outside::session(id, state, status, None)
    }
}

#[test]
fn claudes_own_sessions_stand_under_their_folders_project() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    let mut d = digest(
        now,
        vec![item("work", RunState::Working, relay, "2026-09-10T11:30:00Z", false)],
        vec![],
    );
    d.outside = vec![
        // In the project's folder, and deeper in it — a worktree there.
        claude_in("bg", relay, Some("working"), "busy"),
        claude_in("deep", "/home/f/relay/.claude/worktrees/x", None, "idle"),
        // Outside every registered folder: its own folder's group.
        claude_in("away", "/home/f/scratch", None, "busy"),
    ];
    let rows = build_with(&d, &names, "", now);
    assert_eq!(
        keys(&rows),
        [
            "[relay]",
            "o:claude:sess-bg",
            "o:claude:sess-deep",
            "r:work",
            "[scratch]",
            "o:claude:sess-away"
        ]
    );
    let Some(Row::Group(h)) = rows.iter().find(|r| matches!(r, Row::Group(h) if h.name == "scratch")) else {
        panic!("no scratch header");
    };
    assert_eq!(h.workspace, PathBuf::from("/home/f/scratch"));
    // A run there would be filed the same way.
    let run_there = item("r", RunState::Working, "/home/f/scratch", "2026-09-10T11:30:00Z", false);
    assert_eq!(group_key(&run_there.run, &names, &[]), h.workspace.to_string_lossy());
}

#[test]
fn a_session_stands_where_rld_filed_it() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let names = Names::new([(PathBuf::from("/home/f/relay"), "relay".to_string())]);
    let mut d = digest(now, vec![], vec![]);
    // Deep in a repo no project registers: under the repo's root, as a
    // run started there is, not under its own folder.
    let mut deep = claude_in("deep", "/home/f/tools/src/bin", None, "idle");
    deep.workspace = Some(PathBuf::from("/home/f/tools"));
    d.outside = vec![deep, claude_in("guess", "/home/f/relay/sub", None, "idle")];
    let rows = build_with(&d, &names, "", now);
    assert_eq!(
        keys(&rows),
        ["[relay]", "o:claude:sess-guess", "[tools]", "o:claude:sess-deep"],
        "filed by rld, else guessed by the registered folder"
    );
}

#[test]
fn a_hidden_session_comes_through_when_it_comes_to_need_you() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let t = |s: &str| -> Timestamp { s.parse().unwrap() };
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    let session = |id: &str, state: &str, status: &str, at: &str| Outside {
        state_at: Some(t(at)),
        ..claude_in(id, relay, Some(state), status)
    };
    let mut d = digest(now, vec![], vec![]);
    d.outside = vec![
        // Blocked after its project was hidden: comes through.
        session("after", "blocked", "waiting", "2026-09-10T11:30:00Z"),
        // Blocked before: stays hidden.
        session("before", "blocked", "waiting", "2026-09-10T10:30:00Z"),
        // At work after: nothing needs you.
        session("busy", "working", "busy", "2026-09-10T11:30:00Z"),
    ];
    d.hidden = vec![Hidden {
        key: format!("g:{relay}"),
        at: t("2026-09-10T11:00:00Z"),
    }];
    assert_eq!(keys(&build_with(&d, &names, "", now)), ["o:claude:sess-after"]);
    // Its own `/hide`, later than it came to need you: hidden again.
    d.hidden.push(Hidden {
        key: "o:claude:sess-after".into(),
        at: t("2026-09-10T11:45:00Z"),
    });
    assert!(keys(&build_with(&d, &names, "", now)).is_empty());
    assert_eq!(
        keys(&lens_build(&d, &names, (true, true, true, true), now)),
        [
            "[relay]",
            "o:claude:sess-after",
            "o:claude:sess-before",
            "o:claude:sess-busy"
        ]
    );
}

#[test]
fn claudes_own_sessions_are_agents_not_flows() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    let mut d = digest(now, vec![], vec![]);
    d.outside = vec![claude_in("bg", relay, Some("working"), "busy")];
    assert_eq!(
        keys(&lens_build(&d, &names, DEFAULT, now)),
        ["[relay]", "o:claude:sess-bg"]
    );
    assert_eq!(
        keys(&lens_build(&d, &names, (false, true, true, false), now)),
        ["[relay]", "o:claude:sess-bg"],
        "agents on, flows off: a session shows"
    );
    assert_eq!(
        keys(&lens_build(&d, &names, (true, false, true, false), now)),
        ["[relay]"],
        "agents off: no session, it is not a flow"
    );
}

#[test]
fn agents_own_sessions_stand_with_runs_in_the_order_they_came() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    let mut d = digest(
        now,
        vec![item("idle", RunState::Idle, relay, "2026-09-10T11:30:00Z", false)],
        vec![],
    );
    let at = |t: &str, c: Outside| Outside {
        started_at: Some(t.parse().unwrap()),
        ..c
    };
    let blocked = Outside {
        waiting_for: Some("permission prompt".into()),
        ..claude_in("blocked", relay, Some("blocked"), "waiting")
    };
    d.outside = vec![
        at("2026-09-10T11:10:00Z", claude_in("done", relay, Some("done"), "")),
        at(
            "2026-09-10T11:50:00Z",
            claude_in("working", relay, Some("working"), "busy"),
        ),
        at("2026-09-10T11:20:00Z", blocked),
        at("2026-09-10T11:40:00Z", claude_in("asks", relay, None, "waiting")),
    ];
    let rows = build_with(&d, &names, "", now);
    assert_eq!(
        keys(&rows),
        [
            "[relay]",
            "o:claude:sess-done",
            "o:claude:sess-blocked",
            "r:idle",
            "o:claude:sess-asks",
            "o:claude:sess-working"
        ],
        "what needs you does not jump ahead: each in the order it started"
    );
    // Those that need you say so where they stand.
    assert!(wants_you(&rows[2]) && wants_you(&rows[4]) && !wants_you(&rows[5]));
    // The headline counts them as runs: two need you, one works.
    assert_eq!(counts(&rows), (0, 2, 2, 1));
    assert_eq!(headline(&rows, 0, "", 0), "relay · 2 need you · 2 working · 1 finished");
}

#[test]
fn a_claude_session_hides_by_its_own_key_or_its_projects() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    let mut d = digest(now, vec![], vec![]);
    d.outside = vec![claude_in("bg", relay, Some("working"), "busy")];
    for key in ["o:claude:sess-bg".to_string(), format!("g:{relay}")] {
        let mut hidden = d.clone();
        hidden.hidden = vec![hid(&key, "2026-09-10T11:00:00Z")];
        assert!(
            !keys(&lens_build(&hidden, &names, DEFAULT, now)).contains(&"o:claude:sess-bg".to_string()),
            "{key}"
        );
        assert_eq!(
            keys(&lens_build(&hidden, &names, (true, true, true, true), now)),
            ["[relay]", "o:claude:sess-bg"],
            "{key}"
        );
    }
}

#[test]
fn a_search_finds_claudes_own_sessions() {
    let now: Timestamp = "2026-09-10T12:00:00Z".parse().unwrap();
    let relay = "/home/f/relay";
    let names = Names::new([(PathBuf::from(relay), "relay".to_string())]);
    let mut d = digest(
        now,
        vec![item("work", RunState::Working, relay, "2026-09-10T11:30:00Z", false)],
        vec![],
    );
    d.outside = vec![
        claude_in("bg", relay, Some("blocked"), "waiting"),
        claude_in("away", "/home/f/scratch", None, "idle"),
    ];
    let shown = |f: &str| keys(&build_with(&d, &names, f, now));
    assert_eq!(
        shown("kind:claude"),
        ["[relay]", "o:claude:sess-bg", "[scratch]", "o:claude:sess-away"]
    );
    assert_eq!(shown("kind:agent in:relay"), ["[relay]", "o:claude:sess-bg", "r:work"]);
    assert_eq!(shown("is:needs"), ["[relay]", "o:claude:sess-bg"]);
    assert_eq!(shown("is:going kind:claude"), ["[scratch]", "o:claude:sess-away"]);
    assert_eq!(shown("name away"), ["[scratch]", "o:claude:sess-away"]);
    assert_eq!(shown("scratch"), ["[scratch]", "o:claude:sess-away"]);
    assert_eq!(shown("kind:flow"), Vec::<String>::new());
    // `kind:<agent>`: that agent's runs, and its own sessions.
    let mut both = d.clone();
    both.items[0].run.cli = Some("codex".into());
    both.outside.push(Outside {
        agent: "codex".into(),
        ..claude_in("cx", relay, None, "busy")
    });
    assert_eq!(
        keys(&build_with(&both, &names, "kind:codex", now)),
        ["[relay]", "o:codex:sess-cx", "r:work"]
    );
    assert!(!keys(&build_with(&both, &names, "kind:claude", now)).contains(&"r:work".to_string()));
    let kinds = list_kinds();
    for k in ["agent", "question", "claude", "codex", "gemini"] {
        assert!(kinds.iter().any(|x| x == k), "{k}: {kinds:?}");
    }
    // By its session id, and by where it lives.
    assert_eq!(shown("sess-aw"), ["[scratch]", "o:claude:sess-away"]);
    assert_eq!(shown("terminal"), ["[scratch]", "o:claude:sess-away"]);
    assert_eq!(shown("background"), ["[relay]", "o:claude:sess-bg"]);
    // The search ranks it by its name.
    let rows = build_with(&d, &names, "", now);
    let named: Vec<Named> = rows.iter().map(|r| named(r, &d, &names, &[])).collect();
    let order = crate::tui::query::ranked(&Query::parse("away").words, &named);
    assert_eq!(rows[order[0]].key().as_deref(), Some("o:claude:sess-away"));
}
