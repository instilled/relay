//! The order rows stand in: what wants you, what came when, your moved projects, and the counts and lines above the list.

use super::*;

/// The amber hint on a project's header: how many of its items need you but
/// are off the list now — hidden, or a toggle dropped them. `! 2 need you`;
/// nothing when none are buried, so the header stays quiet until something is.
pub fn needs_hint(attention: usize) -> Option<String> {
    (attention > 0).then(|| format!("! {attention} need{} you", if attention == 1 { "s" } else { "" }))
}

/// Whether a row wants you: it asks, is blocked, or failed — a flow whose
/// last run failed too. On the list it stays where it stands and says so;
/// the catch-up puts it first in its project.
pub fn wants_you(row: &Row) -> bool {
    let needs = |s: RunState| matches!(s, RunState::NeedsInput | RunState::Blocked | RunState::Failed);
    match row {
        Row::Run(i) => needs(i.run.state),
        Row::Flow(f) => f.last().is_some_and(|i| needs(i.run.state)),
        Row::Outside(c) => needs(c.run_state()),
        Row::Group(_) | Row::Question(_) => false,
    }
}

/// Where a row stands in a project you have not put in order: by when it came — a
/// run when it started, a flow when the oldest run the list has of it
/// started — the newest at the bottom; a flow that has not run after them,
/// by name. Nothing that happens to a row afterwards changes it.
pub(crate) fn came(row: &Row) -> (bool, Option<Timestamp>, String) {
    match row {
        Row::Run(i) => (false, Some(i.run.started_at), i.run.id.clone()),
        Row::Flow(f) => {
            let first = f.runs.iter().map(|i| i.run.started_at).min();
            (first.is_none(), first, f.name.clone())
        }
        Row::Outside(c) => (c.started_at.is_none(), c.started_at, c.key()),
        Row::Group(_) | Row::Question(_) => (true, None, String::new()),
    }
}

/// A project's rows, in the order they came, put in the order kept for it:
/// the rows it knows where it has them, then what it does not know yet —
/// new since — at the bottom, in the order they came.
pub(crate) fn in_order<'a>(came: Vec<Row<'a>>, kept: &[String]) -> Vec<Row<'a>> {
    let place = |r: &Row| r.key().and_then(|k| kept.iter().position(|x| *x == k));
    let (mut placed, new): (Vec<Row<'a>>, Vec<Row<'a>>) = came.into_iter().partition(|r| place(r).is_some());
    placed.sort_by_cached_key(|r| place(r));
    placed.into_iter().chain(new).collect()
}

/// Each question right under the row that asked it, oldest first; one whose
/// row is not among `rows` at the bottom.
pub(crate) fn attach<'a>(rows: Vec<Row<'a>>, mut asked: Vec<Asked<'a>>) -> Vec<Row<'a>> {
    asked.sort_by_key(|(_, q)| (q.created_at, q.id.clone()));
    let keys: HashSet<String> = rows.iter().filter_map(Row::key).collect();
    let (under, loose): (Vec<Asked<'a>>, Vec<Asked<'a>>) = asked
        .into_iter()
        .partition(|(k, _)| k.as_ref().is_some_and(|k| keys.contains(k)));
    let mut out = Vec::with_capacity(rows.len() + under.len() + loose.len());
    for row in rows {
        let key = row.key();
        out.push(row);
        out.extend(under.iter().filter(|(k, _)| *k == key).map(|(_, q)| Row::Question(q)));
    }
    out.extend(loose.into_iter().map(|(_, q)| Row::Question(q)));
    out
}

/// A project's rows as they stand on the list with nothing narrowing it
/// (`full`) — the order `in_order` keeps — but for its questions, which
/// stand under their rows rather than in the order.
pub(crate) fn standing(full: &[Row], group: &str) -> Vec<String> {
    let Some(head) = full.iter().position(|r| r.key().as_deref() == Some(group)) else {
        return Vec::new();
    };
    full[head + 1..]
        .iter()
        .take_while(|r| !matches!(r, Row::Group(_)))
        .filter(|r| !matches!(r, Row::Question(_)))
        .filter_map(Row::key)
        .collect()
}

/// A project's header's key — not `everywhere`'s, which is not one.
pub(crate) fn project_key(row: &Row) -> Option<String> {
    match row {
        Row::Group(h) if !h.is_everywhere() => row.key(),
        _ => None,
    }
}

/// The projects in the order they stand: the order kept for them, what it
/// has that the list does not show now keeping its place, then those it
/// does not know, as the list has them (`full`, nothing narrowing it).
pub(crate) fn projects_standing(full: &[Row], kept: &[Order]) -> Vec<String> {
    let mut keys: Vec<String> = kept
        .iter()
        .find(|o| o.group == Order::PROJECTS)
        .map(|o| o.rows.clone())
        .unwrap_or_default();
    for k in full.iter().filter_map(project_key) {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
}

/// The order to keep once the row `key` moves one place up (or down) among
/// what is `shown`: its project's whole order, from `full` — the list with
/// nothing narrowing it — so rows a search or a toggle leaves out keep theirs.
/// Rows move only inside their project, past a question as past nothing; a
/// header moves among the projects. A question, a pinned run and
/// `everywhere` stay put, and say why.
pub fn reorder(shown: &[Row], full: &[Row], kept: &[Order], key: &str, up: bool) -> Result<Order, &'static str> {
    let at = shown
        .iter()
        .position(|r| r.key().as_deref() == Some(key))
        .ok_or("nothing here to move")?;
    match &shown[at] {
        Row::Group(h) if h.is_everywhere() => return Err("everywhere stays last, below the projects"),
        Row::Group(_) => return move_project(shown, full, kept, key, up),
        Row::Question(_) => return Err("a question stays under the run that asked it"),
        Row::Run(_) | Row::Flow(_) | Row::Outside(_) => {}
    }
    let Some(head) = shown[..at].iter().rposition(|r| matches!(r, Row::Group(_))) else {
        return Err(if shown[at].item().is_some_and(|i| i.run.pinned_at.is_some()) {
            "a pinned run keeps the order it was pinned in · ^t lets it go back"
        } else {
            "it needs you, so it stays at the top until it is dealt with"
        });
    };
    // The row it passes: the next run or flow, over a question between.
    let step = |i: usize| if up { i.checked_sub(1) } else { Some(i + 1) };
    let mut next = step(at);
    while let Some(Row::Question(_)) = next.and_then(|i| shown.get(i)) {
        next = next.and_then(step);
    }
    let other = match next.and_then(|i| shown.get(i)) {
        Some(r @ (Row::Run(_) | Row::Flow(_) | Row::Outside(_))) => r.key().unwrap_or_default(),
        _ if up => return Err("already at the top of its project"),
        _ => return Err("already at the bottom of its project"),
    };
    let group = shown[head].key().unwrap_or_default();
    let mut rows = standing(full, &group);
    rows.retain(|k| k != key);
    let i = rows
        .iter()
        .position(|k| *k == other)
        .ok_or("the list moved under it · try again")?;
    rows.insert(if up { i } else { i + 1 }, key.to_string());
    Ok(Order { group, rows })
}

/// The projects' order once the project `key` moves one place up (or down)
/// among the projects `shown`, kept whole from `full`.
pub(crate) fn move_project(
    shown: &[Row],
    full: &[Row],
    kept: &[Order],
    key: &str,
    up: bool,
) -> Result<Order, &'static str> {
    let heads: Vec<String> = shown.iter().filter_map(project_key).collect();
    let at = heads.iter().position(|k| k == key).ok_or("nothing here to move")?;
    let next = if up { at.checked_sub(1) } else { Some(at + 1) };
    let other = match next.and_then(|i| heads.get(i)) {
        Some(k) => k.clone(),
        None if up => return Err("already the first project"),
        None => return Err("already the last project"),
    };
    let mut rows = projects_standing(full, kept);
    rows.retain(|k| k != key);
    let i = rows
        .iter()
        .position(|k| *k == other)
        .ok_or("the list moved under it · try again")?;
    rows.insert(if up { i } else { i + 1 }, key.to_string());
    Ok(Order {
        group: Order::PROJECTS.into(),
        rows,
    })
}

/// `(questions, need you, working, finished, flows)` over what the list shows.
/// A flow counts once, and as needing you when its last run failed; one of
/// an agent's own session as a run in its state would.
pub fn counts(rows: &[Row]) -> (usize, usize, usize, usize, usize) {
    let mut c = (0, 0, 0, 0, 0);
    let by_state = |s: RunState, c: &mut (usize, usize, usize, usize, usize)| {
        use rl_proto::types::RunState::*;
        match s {
            NeedsInput | Blocked | Failed => c.1 += 1,
            Starting | Working | Stale | Idle => c.2 += 1,
            Finished | Stopped => c.3 += 1,
        }
    };
    for r in rows {
        match r {
            Row::Group(..) => {}
            Row::Question(_) => c.0 += 1,
            Row::Run(i) => by_state(i.run.state, &mut c),
            Row::Outside(s) => by_state(s.run_state(), &mut c),
            // A flow that has not run is not what is moving: its header says it is there.
            Row::Flow(f) if f.runs.is_empty() => {}
            Row::Flow(f) => {
                c.4 += 1;
                if f.failed_last() {
                    c.1 += 1;
                }
            }
        }
    }
    c
}

/// What a restart took that is still on the list, as the headline says it,
/// with the command that picks it up. `/resume all` leaves plain commands
/// out, so when they are all there is, it says the command that takes them.
pub fn lost_note(d: &Digest) -> Option<String> {
    if d.lost.is_empty() {
        return None;
    }
    let command = |id: &String| {
        d.items
            .iter()
            .any(|i| &i.run.id == id && i.run.kind == RunKind::Command)
    };
    let how = if d.lost.iter().all(command) {
        "/resume all commands"
    } else {
        "/resume all"
    };
    Some(format!("{} lost in a restart · {how}", d.lost.len()))
}

/// The flows with no `in`, as the headline says them: how many there are —
/// `↻ 2 everywhere` — and, when any waits for a yes, that it does and where
/// to give it. `chips` are those ones (`chips(flows, None, ..)`); the flag
/// says one is waiting, which is drawn amber. Nothing when there are none.
pub fn everywhere(chips: &[Chip]) -> Option<(String, bool)> {
    if chips.is_empty() {
        return None;
    }
    let waiting = chips.iter().filter(|c| c.needs_yes()).count();
    let mut s = format!("{FLOW} {} everywhere", chips.len());
    let need = |n: usize| if n == 1 { "needs" } else { "need" };
    match waiting {
        0 => {}
        // All of them wait: the count already says how many.
        n if n == chips.len() => s.push_str(&format!(" {} a yes · /flow approve", need(n))),
        n => s.push_str(&format!(" · {n} {} a yes · /flow approve", need(n))),
    }
    Some((s, waiting > 0))
}

/// What the headline says of the flows with no `in`, piece by piece: the
/// tally (`everywhere`), amber when one waits for a yes, then — when a flow
/// of theirs did not load — `flows broken` in red beside it.
pub fn global_parts(chips: &[Chip], broken: bool) -> Vec<(String, Tone)> {
    let mut out = Vec::new();
    if let Some((note, waiting)) = everywhere(chips) {
        out.push((note, if waiting { Tone::Needs } else { Tone::Plain }));
    }
    if broken {
        out.push(("flows broken".into(), Tone::Bad));
    }
    out
}

/// The header line: what needs you first, then what is moving.
pub fn headline(rows: &[Row], news: usize, filter: &str) -> String {
    let (asked, need, working, fin, flows) = counts(rows);
    let mut parts = Vec::new();
    if asked > 0 {
        parts.push(format!("{asked} to decide"));
    }
    if need > 0 {
        parts.push(format!("{need} need{} you", if need == 1 { "s" } else { "" }));
    }
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if fin > 0 {
        parts.push(format!("{fin} finished"));
    }
    if flows > 0 {
        parts.push(format!("{flows} flow{}", if flows == 1 { "" } else { "s" }));
    }
    if news > 0 {
        parts.push(format!("{news} new"));
    }
    let mut s = String::from("relay");
    if parts.is_empty() {
        s.push_str(" · nothing running");
    }
    for p in parts {
        s.push_str(" · ");
        s.push_str(&p);
    }
    if !filter.is_empty() {
        s.push_str(&format!(" · ⌕ {filter}"));
    }
    s
}
