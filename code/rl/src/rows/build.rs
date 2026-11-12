//! Building the list: what a row is searched by, and assembling the lines the rows make.

use super::*;

/// What a search matches a row on: its words, its project, how it stands,
/// and what it is — an agent, and whose (`kind:claude`).
pub fn subject<'a>(
    r: &'a Row,
    title: &'a str,
    project: &'a str,
    on: Option<&'a str>,
    pinned: bool,
    hidden: bool,
) -> Subject<'a> {
    let mut is = vec![is_word(r.state)];
    if pinned {
        is.push("pinned");
    }
    if hidden {
        is.push("hidden");
    }
    let mut kind = vec![match r.kind {
        RunKind::Agent => "agent",
        RunKind::Shell => "shell",
        RunKind::Command => "command",
    }];
    if r.kind == RunKind::Agent {
        kind.push(&r.harness);
    }
    Subject {
        text: vec![title, &r.name, &r.harness, r.summary.as_deref().unwrap_or("")],
        project: vec![project, r.cwd.to_str().unwrap_or("")],
        is,
        kind,
        on,
    }
}

/// A line as the search ranks it (`query::ranked`): a header by its name,
/// a row by title, project and summary.
pub fn named(line: &Line, names: &Names) -> Named {
    let path = |p: &Path| p.to_string_lossy().into_owned();
    match line {
        Line::Group(h) => Named {
            head: h.name.clone(),
            rest: vec![h.place.clone(), path(&h.workspace)],
            ..Named::default()
        },
        Line::Session(r) => Named {
            title: names.title(r).to_string(),
            project: names.workspace(&project_of(r, names)),
            summary: r.summary.clone().unwrap_or_default(),
            // The harness's own name stays searchable under yours.
            rest: vec![r.harness.clone(), path(&r.cwd), r.name.clone()],
            ..Named::default()
        },
    }
}

/// The branch a project's folder is on, if it is in a repo and on one.
fn branch_of<'a>(checkouts: &'a Checkouts, project: &Path) -> Option<&'a str> {
    checkouts.get(project)?.branch.as_deref()
}

/// A project's header, with its folder's checkout when there is one.
pub fn head(folder: &Path, names: &Names, checkouts: &Checkouts, attention: usize) -> Head {
    Head {
        checkout: checkouts.get(folder).cloned().map(Box::new),
        key: head_key(folder),
        place: short_path(folder),
        workspace: folder.to_path_buf(),
        name: names.workspace(folder),
        own: names.own(folder),
        attention,
        empty: false,
        heading: None,
    }
}

/// Everything on the list, in order, narrowed to what the filter — a search,
/// see `query` — hits.
pub fn build<'a>(rows: &'a [Row], cx: &Context<'_>) -> Vec<Line<'a>> {
    let prefs = cx.prefs;
    let names = cx.names();
    let query = Query::parse(cx.filter);
    // By project, each with its rows, in the order the projects first came.
    let mut groups: Vec<(PathBuf, Vec<&'a Row>)> = Vec::new();
    // Pinned rows stand above everything, whatever the toggles: kept on
    // purpose, so no toggle takes them away. Only the filter does.
    let mut pinned: Vec<&'a Row> = Vec::new();
    // What you hid is off the list unless `show_hidden` brings it back inline,
    // in its place; what of it has come to need you since comes through,
    // above the projects, whenever hidden is off. By how it ranks, then
    // since when it has needed you, then its key.
    let mut through: Vec<(u8, Option<Timestamp>, String, &'a Row)> = Vec::new();
    // Per project, and per heading, how many of its rows need you but are
    // off the list now. Its header hints at them.
    let mut buried: HashMap<PathBuf, usize> = HashMap::new();
    let mut buried_under: HashMap<Heading, usize> = HashMap::new();
    let mut bury = |project: &Path, r: &Row| {
        *buried.entry(project.to_path_buf()).or_insert(0) += 1;
        *buried_under.entry(Heading::of(r.state)).or_insert(0) += 1;
    };
    // Every project some row stands under, shown or not: a header not among
    // them has nothing in it.
    let mut held: HashSet<PathBuf> = HashSet::new();
    for r in rows {
        let project = project_of(r, &names);
        held.insert(project.clone());
        let project_name = names.workspace(&project);
        let since = hidden_since(r, &project, &prefs.hidden);
        let is_pinned = cx.pinned(r);
        let on = branch_of(cx.checkouts, &project);
        let hits = || {
            query.hits(&subject(
                r,
                names.title(r),
                &project_name,
                on,
                is_pinned,
                since.is_some(),
            ))
        };
        let kept = keeps(r, prefs.show_agents, prefs.show_shells);
        if let Some(since) = since
            && !prefs.show_hidden
        {
            if comes_through(r, since) && kept {
                if hits() {
                    through.push((rank::score(r), r.state_at, r.key.clone(), r));
                }
            } else if wants(r.state) {
                // Hidden while it already needed you, so it does not come
                // through — or it would, but a toggle takes it off: its
                // project hints it is there.
                bury(&project, r);
            }
            continue;
        }
        if is_pinned {
            if hits() {
                pinned.push(r);
            }
            continue;
        }
        if !kept {
            // A toggle takes it off the list; if it needs you, its project
            // hints it is still there.
            if wants(r.state) {
                bury(&project, r);
            }
            continue;
        }
        if !hits() {
            continue;
        }
        match groups.iter_mut().find(|(f, _)| *f == project) {
            Some((_, g)) => g.push(r),
            None => groups.push((project, vec![r])),
        }
    }
    through.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    pinned.sort_by_key(|r| prefs.pins.iter().position(|p| *p == r.key));
    let mut lines: Vec<Line<'a>> = through
        .into_iter()
        .map(|(.., r)| r)
        .chain(pinned)
        .map(Line::Session)
        .collect();
    match prefs.grouping {
        Grouping::Project => lines.extend(by_project(groups, &buried, &held, cx, &query)),
        Grouping::State => lines.extend(by_state(groups, &buried_under)),
    }
    lines
}

/// The projects, each its header and its rows: in the order you put them,
/// the rest after them by name.
fn by_project<'a>(
    groups: Vec<(PathBuf, Vec<&'a Row>)>,
    buried: &HashMap<PathBuf, usize>,
    held: &HashSet<PathBuf>,
    cx: &Context<'_>,
    query: &Query,
) -> Vec<Line<'a>> {
    let prefs = cx.prefs;
    let names = cx.names();
    let attention = |p: &Path| buried.get(p).copied().unwrap_or(0);
    let mut blocks: Vec<(String, String, Vec<Line<'a>>)> = Vec::new();
    let shown: Vec<PathBuf> = groups.iter().map(|(f, _)| f.clone()).collect();
    for (folder, mut rows) in groups {
        let h = head(&folder, &names, cx.checkouts, attention(&folder));
        // The order they came in, which nothing that happens to a row
        // changes — until you put the project's rows in order: then yours,
        // and what is new enters at the bottom.
        rows.sort_by_cached_key(|r| came(r));
        let rows = match prefs.order.iter().find(|o| o.group == h.key && !o.rows.is_empty()) {
            Some(o) => in_order(rows, &o.rows),
            None => rows,
        };
        let mut block = vec![Line::Group(h.clone())];
        block.extend(rows.into_iter().map(Line::Session));
        blocks.push((h.name.to_lowercase(), h.key, block));
    }
    // A folder you named or added (`/project`) is on the list, whatever the
    // toggles, even with nothing in it; so is one with rows that need you
    // buried in it. The
    // toggles narrow what is shown inside projects, never which projects
    // there are; only the filter takes them away — and hiding, which puts a
    // project's header off the list, back inline only when hidden is on.
    let hidden_heads: Vec<PathBuf> = prefs
        .hidden
        .iter()
        .filter_map(|h| h.key.strip_prefix("g:"))
        .map(PathBuf::from)
        .collect();
    let mut heads: Vec<PathBuf> = names.paths();
    heads.extend(added(prefs));
    heads.extend(buried.keys().cloned());
    if prefs.show_hidden {
        heads.extend(hidden_heads.iter().cloned());
    } else {
        heads.retain(|p| !hidden_heads.contains(p));
    }
    heads.sort();
    heads.dedup();
    for p in heads {
        if shown.contains(&p) {
            continue;
        }
        let h = Head {
            empty: !held.contains(&p),
            ..head(&p, &names, cx.checkouts, attention(&p))
        };
        if !query.hits_project(
            &[&h.name, &h.place, p.to_str().unwrap_or("")],
            branch_of(cx.checkouts, &p),
        ) {
            continue;
        }
        blocks.push((h.name.to_lowercase(), h.key.clone(), vec![Line::Group(h)]));
    }
    // Projects in the order you put them (shift-↑/↓ on a header), the rest
    // after them by name — a project new to the list enters at the bottom —
    // so each keeps its place as work comes and goes.
    let projects = prefs.order.iter().find(|o| o.group == Order::PROJECTS);
    let place = |key: &str| projects.and_then(|o| o.rows.iter().position(|k| k == key));
    blocks.sort_by_cached_key(|(name, key, _)| (place(key).unwrap_or(usize::MAX), name.clone(), key.clone()));
    blocks.into_iter().flat_map(|(.., block)| block).collect()
}

/// Needs input · Working · Done, each with its rows oldest first; a heading
/// with nothing under it and nothing buried is left out.
fn by_state<'a>(groups: Vec<(PathBuf, Vec<&'a Row>)>, buried: &HashMap<Heading, usize>) -> Vec<Line<'a>> {
    let mut all: Vec<&'a Row> = groups.into_iter().flat_map(|(_, rows)| rows).collect();
    all.sort_by_cached_key(|r| came(r));
    let mut lines = Vec::new();
    for heading in Heading::ALL {
        let attention = buried.get(&heading).copied().unwrap_or(0);
        let under: Vec<&'a Row> = all
            .iter()
            .copied()
            .filter(|r| Heading::of(r.state) == heading)
            .collect();
        if under.is_empty() && attention == 0 {
            continue;
        }
        lines.push(Line::Group(Head::heading(heading, attention)));
        lines.extend(under.into_iter().map(Line::Session));
    }
    lines
}
