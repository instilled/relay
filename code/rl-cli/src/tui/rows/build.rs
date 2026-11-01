//! Building the list: the subject a row is searched by, and assembling the rows a digest shows.

use super::*;

pub(crate) struct Group<'a> {
    key: String,
    /// The folder of whatever came first; a group's members share one.
    workspace: PathBuf,
    runs: Vec<&'a DigestItem>,
    flows: Vec<FlowRow<'a>>,
    outside: Vec<&'a Outside>,
    /// The questions asked in it, each with the key of the row it stands
    /// under when that row is there.
    asked: Vec<Asked<'a>>,
}

/// What a search matches a run on: its words, its project, what state it is
/// in, what it is, and the flow behind it.
pub(crate) fn run_subject<'a>(i: &'a DigestItem, project: &'a str) -> Subject<'a> {
    let r = &i.run;
    let mut is = vec![match r.state {
        RunState::Failed => "failed",
        RunState::NeedsInput | RunState::Blocked => "needs",
        RunState::Finished => "done",
        RunState::Stopped => "stopped",
        RunState::Starting | RunState::Working | RunState::Stale | RunState::Idle => "going",
    }];
    if r.pinned_at.is_some() {
        is.push("pinned");
    }
    if i.run.hidden_at.is_some() {
        is.push("hidden");
    }
    if i.unseen {
        is.push("new");
    }
    let mut kind = vec![match r.kind {
        RunKind::Agent => "agent",
        RunKind::Shell => "shell",
        RunKind::Command | RunKind::Plugin => "command",
    }];
    // `kind:<agent>`: that agent, whoever started it.
    if r.kind == RunKind::Agent
        && let Some(cli) = r.cli.as_deref()
    {
        kind.push(cli);
    }
    let by = match &r.cause {
        Cause::Flow { flow, .. } => {
            kind.push("flow");
            Some(flow.as_str())
        }
        _ => None,
    };
    Subject {
        text: vec![&r.title, r.cli.as_deref().unwrap_or(""), said(i)],
        project: vec![project, r.workspace.to_str().unwrap_or("")],
        is,
        kind,
        by: by.into_iter().collect(),
        on: r.branch.as_deref(),
    }
}

/// What a search matches one of an agent's own sessions on: its name, where
/// it lives (`lives`), its project and folder, how it stands as a run
/// would, and that it is an agent, and whose (`kind:claude`, `kind:codex`).
pub(crate) fn outside_subject<'a>(s: &'a Outside, project: &'a str, lives: &'a str, hidden: bool) -> Subject<'a> {
    let mut is = vec![outside::is_word(s)];
    if hidden {
        is.push("hidden");
    }
    Subject {
        text: vec![&s.name, &s.session_id, lives],
        project: vec![project, s.cwd.to_str().unwrap_or("")],
        is,
        kind: vec!["agent", &s.agent],
        ..Subject::default()
    }
}

/// A row as the search ranks it (`query::ranked`): a header by its name, the
/// rest by title, project and summary; a flow that has not run, by the
/// group its flow (in `flows`) stands in.
pub fn named(row: &Row, d: &Digest, names: &Names, flows: &[Flow]) -> Named {
    let path = |p: &Path| p.to_string_lossy().into_owned();
    match row {
        Row::Group(h) => Named {
            head: h.name.clone(),
            rest: vec![h.place.clone(), path(&h.workspace)],
            ..Named::default()
        },
        Row::Question(q) => {
            let asker = q.run.as_deref().and_then(|id| d.items.iter().find(|i| i.run.id == id));
            let key = asker.map_or_else(|| path(&q.workspace), |i| i.run.place_key());
            Named {
                title: q.title.clone(),
                project: names.group(&key),
                summary: q.summary.clone().unwrap_or_default(),
                rest: vec![path(&q.workspace)],
                ..Named::default()
            }
        }
        Row::Run(i) => Named {
            title: i.run.title.clone(),
            project: names.group(&i.run.place_key()),
            summary: said(i).to_string(),
            rest: vec![i.run.cli.clone().unwrap_or_default(), path(&i.run.workspace)],
            ..Named::default()
        },
        Row::Flow(f) => match f.last() {
            Some(i) => {
                let r = &i.run;
                Named {
                    title: f.name.clone(),
                    project: names.group(&r.place_key()),
                    summary: f.summary(),
                    rest: vec![r.title.clone(), r.cli.clone().unwrap_or_default(), path(&r.workspace)],
                    ..Named::default()
                }
            }
            None => Named {
                title: f.name.clone(),
                project: group_name(&flow_group(flows, &f.name).0, names),
                summary: f.when.clone().unwrap_or_default(),
                ..Named::default()
            },
        },
        Row::Outside(c) => Named {
            title: c.title(),
            project: names.group(&outside_group(c, names).0),
            summary: outside::lives(c),
            rest: vec![path(&c.cwd)],
            ..Named::default()
        },
    }
}

/// Everything on the list, in order, narrowed to what the filter — a search,
/// see `query` — hits.
pub fn build<'a>(d: &'a Digest, cx: &Context<'_>) -> Vec<Row<'a>> {
    let names = cx.names;
    let now = cx.now;
    let query = Query::parse(cx.filter);
    let mut groups: Vec<Group<'a>> = Vec::new();
    let group_at = |key: String, workspace: &Path, groups: &mut Vec<Group<'a>>| -> usize {
        match groups.iter().position(|g| g.key == key) {
            Some(i) => i,
            None => {
                groups.push(Group {
                    key,
                    workspace: workspace.to_path_buf(),
                    runs: Vec::new(),
                    flows: Vec::new(),
                    outside: Vec::new(),
                    asked: Vec::new(),
                });
                groups.len() - 1
            }
        }
    };
    let when_of = |name: &str| -> Option<String> {
        cx.flows
            .iter()
            .find(|f| f.name == name)
            .map(|f| flows::when_words(&f.when()))
    };
    let run_by_id = |id: &str| d.items.iter().map(|i| &i.run).find(|r| r.id == id);
    // A flow's remembered history must be its FULL history, independent of
    // the filter — the filter only decides whether the flow's row is shown at
    // all. So flows are accumulated here first, unconditionally, and folded
    // into their group afterwards only if the flow or one of its own runs hit.
    struct FlowAcc<'a> {
        key: String,
        name: String,
        group_key: String,
        workspace: PathBuf,
        runs: Vec<&'a DigestItem>,
        hit: bool,
    }
    let mut flows_acc: Vec<FlowAcc<'a>> = Vec::new();
    // Pinned runs stand above everything, however old, whatever the toggles —
    // kept on purpose, so neither a day nor a toggle takes them away. Only the
    // filter does, as with anything else.
    let mut pinned: Vec<&'a DigestItem> = Vec::new();
    // What you hid is off the list unless `show_hidden` brings it back inline,
    // in its place; what of it has come to need you since comes through, above
    // the projects, whenever hidden is off.
    // By how it ranks, then since when it has needed you, then its key.
    let mut through: Vec<(u8, Option<Timestamp>, String, Row<'a>)> = Vec::new();
    // Per project (by its group key), how many of its items need you but are
    // off the list now — hidden and not come through, or a toggle dropped
    // them. Its header hints at them, so nothing urgent is silently buried.
    let mut buried: HashMap<String, usize> = HashMap::new();
    for i in &d.items {
        let r = &i.run;
        // With flows off a flow's run is not on the list, nor hinted on its project.
        let flow_run = matches!(r.cause, Cause::Flow { .. });
        // A summary of everything runs in the Relay home: Relay's doing, not
        // a project of yours. `rl ls --all` and the events have it.
        if matches!(r.cause, Cause::Assistant(_)) && cx.relay.is_some_and(|h| r.cwd == h) {
            continue;
        }
        if let Some(since) = hidden_since(r, names, cx.flows, &d.hidden)
            && !cx.show_hidden
        {
            if comes_through(r, since)
                && keeps(r, cx.show_agents, cx.show_flows, cx.show_shells)
                && query.hits(&run_subject(i, &names.group(&r.place_key())))
            {
                through.push((rank::score(r), Some(r.state_at), r.id.clone(), Row::Run(i)));
            } else if !comes_through(r, since) && r.state.needs_attention() && (cx.show_flows || !flow_run) {
                // Hidden while it already needed you, so it does not come
                // through: its project hints it is there.
                *buried.entry(group_of(r, names, cx.flows).0).or_insert(0) += 1;
            }
            continue;
        }
        // Hidden on: the run stands inline, in its place.
        if r.pinned_at.is_some() {
            // Pinned stands above the agents and shells toggles, but a flow's
            // run is the flow's: with flows off it is not on the list.
            if (cx.show_flows || !flow_run) && query.hits(&run_subject(i, &names.group(&r.place_key()))) {
                pinned.push(i);
            }
            continue;
        }
        let live = !r.state.is_terminal() || i.unseen || r.ended_at.is_some_and(|t| now.duration_since(t) <= RECENT);
        // A flow with a row of its own keeps all the history the digest
        // has of it, however old: its row is there anyway.
        let kept = flow_of(r).is_some_and(|name| cx.flows.iter().any(|f| f.name == name && stands_on_the_list(f)));
        if !live && !kept {
            continue;
        }
        if !keeps(r, cx.show_agents, cx.show_flows, cx.show_shells) {
            // A toggle takes it off the list; if it needs you, its project
            // hints it is still there.
            if r.state.needs_attention() && (cx.show_flows || !flow_run) {
                *buried.entry(group_of(r, names, cx.flows).0).or_insert(0) += 1;
            }
            continue;
        }
        let (key, workspace) = group_of(r, names, cx.flows);
        let name = group_name(&key, names);
        if let Some(flow) = flow_of(r) {
            let mut subject = run_subject(i, &name);
            subject.text.push(flow);
            let item_hit = query.hits(&subject);
            let fkey = flow_key(flow);
            match flows_acc.iter_mut().find(|acc| acc.key == fkey) {
                Some(acc) => {
                    acc.runs.push(i);
                    acc.hit = acc.hit || item_hit;
                }
                None => flows_acc.push(FlowAcc {
                    key: fkey,
                    name: flow.to_string(),
                    group_key: key,
                    workspace,
                    runs: vec![i],
                    hit: item_hit,
                }),
            }
            continue;
        }
        if !query.hits(&run_subject(i, &name)) {
            continue;
        }
        let g = group_at(key, &workspace, &mut groups);
        groups[g].runs.push(i);
    }
    // A flow with a yes has its row whether or not it has run — what starts
    // it and when it fires next, before it ever did — when flows are on, and
    // hidden as its runs would be.
    if cx.show_flows {
        for f in cx.flows.iter().filter(|f| stands_on_the_list(f)) {
            let key = flow_key(&f.name);
            if flows_acc.iter().any(|acc| acc.key == key)
                || (flow_hidden(&f.name, cx.flows, &d.hidden) && !cx.show_hidden)
            {
                continue;
            }
            let (group_key, workspace) = flow_group(cx.flows, &f.name);
            let project = group_name(&group_key, names);
            let hit = query.hits(&Subject {
                text: vec![&f.name],
                project: vec![&project, workspace.to_str().unwrap_or("")],
                kind: vec!["flow"],
                by: vec![&f.name],
                ..Subject::default()
            });
            flows_acc.push(FlowAcc {
                key,
                name: f.name.clone(),
                group_key,
                workspace,
                runs: Vec::new(),
                hit,
            });
        }
    }
    // Agents' own sessions, under their folder's project: agents, so shown
    // when agents are on; hidden as a flow is, and come through as a run does.
    if cx.show_agents {
        for c in &d.outside {
            let since = outside_hidden_since(c, names, &d.hidden);
            let hidden = since.is_some();
            let (key, workspace) = outside_group(c, names);
            let project = names.group(&key);
            let lives = outside::lives(c);
            let hits = || query.hits(&outside_subject(c, &project, &lives, hidden));
            match since {
                Some(since) if !cx.show_hidden => {
                    if outside::comes_through(c, since) && hits() {
                        through.push((outside::score(c), c.state_at, c.key(), Row::Outside(c)));
                    } else if !outside::comes_through(c, since) && c.run_state().needs_attention() {
                        *buried.entry(key).or_insert(0) += 1;
                    }
                    continue;
                }
                _ => {}
            }
            if !hits() {
                continue;
            }
            let g = group_at(key, &workspace, &mut groups);
            groups[g].outside.push(c);
        }
    } else {
        // Agents off: an agent's own session that needs you is off the list,
        // so its project hints it is there.
        for c in &d.outside {
            if c.run_state().needs_attention() {
                *buried.entry(outside_group(c, names).0).or_insert(0) += 1;
            }
        }
    }
    // Fold each flow into its group now, with its full history, but only if
    // the flow (or one of its own runs) actually matched the filter.
    for acc in flows_acc {
        if !acc.hit {
            continue;
        }
        let g = group_at(acc.group_key, &acc.workspace, &mut groups);
        groups[g].flows.push(FlowRow {
            key: acc.key,
            when: when_of(&acc.name),
            firing: d.firings.iter().find(|f| f.flow == acc.name),
            name: acc.name,
            runs: acc.runs,
        });
    }
    // A question stands in its project, under the row that asked it — or,
    // where that run is pinned or has come through, under it there. It
    // ignores the toggles: it is what needs you, whatever you are showing.
    let banded: HashSet<&str> = pinned
        .iter()
        .copied()
        .chain(through.iter().filter_map(|(.., row)| row.item()))
        .map(|i| i.run.id.as_str())
        .collect();
    let mut band_asked: Vec<Asked<'a>> = Vec::new();
    for q in &d.decisions {
        let asker = q.run.as_deref().and_then(run_by_id);
        let (key, workspace) = match asker {
            Some(r) => group_of(r, names, cx.flows),
            None => (q.workspace.to_string_lossy().into_owned(), q.workspace.clone()),
        };
        let name = group_name(&key, names);
        // A flow's question is its firing's: the chain is the firing.
        let firing = firing_of(q, &d.firings).map(|f| f.flow.as_str());
        let by = firing.or(asker.and_then(|r| match &r.cause {
            Cause::Flow { flow, .. } => Some(flow.as_str()),
            _ => None,
        }));
        // With flows off, a flow's question is not on the list either.
        if !cx.show_flows && by.is_some() {
            continue;
        }
        let subject = Subject {
            text: vec![&q.title, q.summary.as_deref().unwrap_or("")],
            project: vec![&name, q.workspace.to_str().unwrap_or("")],
            is: vec!["needs"],
            kind: vec!["question"],
            by: by.into_iter().collect(),
            on: asker.and_then(|r| r.branch.as_deref()),
        };
        if !query.hits(&subject) {
            continue;
        }
        let under = match asker {
            Some(r) => Some(flow_of(r).map_or_else(|| format!("r:{}", r.id), flow_key)),
            None => firing.map(flow_key),
        };
        if asker.is_some_and(|r| banded.contains(r.id.as_str())) {
            band_asked.push((under, q));
            continue;
        }
        let g = group_at(key, &workspace, &mut groups);
        groups[g].asked.push((under, q));
    }
    for g in &mut groups {
        for f in &mut g.flows {
            f.runs.sort_by_key(|i| std::cmp::Reverse(i.at));
        }
    }
    // Which folders already have a line, so idle projects don't double up.
    let shown: HashSet<PathBuf> = groups.iter().map(|g| g.workspace.clone()).collect();
    pinned.sort_by_key(|i| (i.run.pinned_at, i.run.id.clone()));
    through.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let band: Vec<Row<'a>> = through
        .into_iter()
        .map(|(_, _, _, row)| row)
        .chain(pinned.into_iter().map(Row::Run))
        .collect();
    let mut rows = attach(band, band_asked);
    // Each block is a project's header and what is under it.
    let mut blocks: Vec<(String, String, Vec<Row<'a>>)> = Vec::new();
    let mut everywhere: Vec<Row<'a>> = Vec::new();
    for g in groups {
        let name = names.group(&g.key);
        let head = if g.key == EVERYWHERE_KEY {
            Head::everywhere()
        } else {
            let mut h = head(&g.key, &g.workspace, d, cx);
            h.attention = buried.get(&g.key).copied().unwrap_or(0);
            h
        };
        let mut block = vec![Row::Group(head)];
        // Runs and agents' own sessions share one order, which nothing that
        // happens to a row changes: the order they came in, the newest at the
        // bottom.
        let mut mixed: Vec<Row<'a>> = g.runs.into_iter().map(Row::Run).collect();
        mixed.extend(g.outside.into_iter().map(Row::Outside));
        mixed.sort_by_cached_key(came);
        // Once you have put a project's rows in order, it keeps yours: the
        // rows stand where you put them, and what is new enters at the bottom.
        let kept = block[0]
            .key()
            .and_then(|k| d.order.iter().find(|o| o.group == k))
            .filter(|o| !o.rows.is_empty());
        let mut mixed = match kept {
            Some(o) => in_order(mixed, &o.rows),
            None => mixed,
        };
        // Its flows under all of that, by name: what stands there, below what moves.
        let mut flows: Vec<Row<'a>> = g.flows.into_iter().map(Row::Flow).collect();
        flows.sort_by_cached_key(|r| match r {
            Row::Flow(f) => f.name.clone(),
            _ => String::new(),
        });
        mixed.extend(flows);
        block.extend(attach(mixed, g.asked));
        if g.key == EVERYWHERE_KEY {
            everywhere = block;
        } else {
            blocks.push((name.to_lowercase(), g.key, block));
        }
    }
    // A registered folder is always on the list, whatever the toggles, even
    // before anything has run there — so you can select it, read its stats,
    // see its flows or open its shell. The toggles narrow what is shown inside
    // projects, never which projects there are; only the filter takes them
    // away — and `/hide`, which puts a project's header off the list, back
    // inline only when hidden is on.
    let hidden_heads: Vec<&str> = d.hidden.iter().filter_map(|h| h.key.strip_prefix("g:")).collect();
    let heads: Vec<PathBuf> = if cx.show_hidden {
        // Every registered folder, the hidden ones among them, plus any hidden
        // folder that is not registered.
        let mut hs = names.paths();
        for k in &hidden_heads {
            if *k != EVERYWHERE {
                let p = PathBuf::from(*k);
                if !hs.contains(&p) {
                    hs.push(p);
                }
            }
        }
        hs
    } else {
        names
            .paths()
            .into_iter()
            .filter(|p| !hidden_heads.iter().any(|k| Path::new(k) == p))
            .collect()
    };
    if cx.show_flows
        && cx.show_hidden
        && everywhere.is_empty()
        && hidden_heads.contains(&EVERYWHERE)
        && query.hits_project(&[EVERYWHERE], None)
    {
        everywhere = vec![Row::Group(Head::everywhere())];
    }
    for p in heads {
        if shown.contains(&p) {
            continue;
        }
        let place = names.place(&p.to_string_lossy(), &p);
        let name = names.workspace(&p);
        let checkout = checkout_of(&d.checkouts, &p);
        if !query.hits_project(
            &[&name, &place, p.to_str().unwrap_or("")],
            checkout.and_then(|c| c.branch.as_deref()),
        ) {
            continue;
        }
        let mut h = head(&p.to_string_lossy(), &p, d, cx);
        h.attention = buried.get(&p.to_string_lossy().into_owned()).copied().unwrap_or(0);
        blocks.push((
            name.to_lowercase(),
            p.to_string_lossy().into_owned(),
            vec![Row::Group(h)],
        ));
    }
    // Projects in the order you put them (shift-↑/↓ on a header), the rest
    // after them by name — a project new to the list enters at the bottom —
    // so each keeps its place as work comes and goes: what needs you says
    // so with its marks and in the headline, not by jumping the queue.
    let projects = d.order.iter().find(|o| o.group == Order::PROJECTS);
    let place = |block: &[Row]| {
        let key = block.first().and_then(Row::key)?;
        projects?.rows.iter().position(|k| *k == key)
    };
    blocks.sort_by_cached_key(|(name, key, block)| (place(block).unwrap_or(usize::MAX), name.clone(), key.clone()));
    rows.extend(blocks.into_iter().flat_map(|(_, _, block)| block));
    // The flows with no `in` are said in the headline (`everywhere`). Their
    // rows, and what flows ran outside every project, are gathered after the
    // projects, under one `everywhere`, rather than under whatever folder that
    // was — all of it flow content, so only when flows are on.
    if cx.show_flows {
        rows.extend(everywhere);
    }
    rows
}

/// A project's header: the group `key` (a ref, or a folder), whose work is
/// in `workspace`.
pub fn head(key: &str, workspace: &Path, d: &Digest, cx: &Context<'_>) -> Head {
    let names = cx.names;
    Head {
        place: names.place(key, workspace),
        workspace: workspace.to_path_buf(),
        name: names.group(key),
        own: names.own(key, workspace),
        flows: chips(cx.flows, Some(workspace), cx.now),
        checkout: checkout_of(&d.checkouts, workspace).cloned(),
        attention: 0,
    }
}
