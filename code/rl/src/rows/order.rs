//! The order rows stand in: what came when, your moved projects and rows, and the counts and line above the list.

use super::*;

/// The amber hint on a header: how many of its rows need you but are off the
/// list now — hidden, or a toggle dropped them. `! 2 need you`; nothing when
/// none are buried, so the header stays quiet until something is.
pub fn needs_hint(attention: usize) -> Option<String> {
    (attention > 0).then(|| format!("! {attention} need{} you", if attention == 1 { "s" } else { "" }))
}

/// Whether a line wants you: its row asks, is blocked, or failed. On the
/// list it stays where it stands and says so.
pub fn wants_you(line: &Line) -> bool {
    line.row().is_some_and(|r| wants(r.state))
}

/// Where a row stands among rows you have not put in order: by when it
/// started — one with no start after them — then by key. Nothing that
/// happens to a row afterwards changes it.
pub fn came(r: &Row) -> (bool, Option<Timestamp>, String) {
    (r.started.is_none(), r.started, r.key.clone())
}

/// A project's rows, in the order they came, put in the order kept for it:
/// the rows it knows where it has them, then what it does not know yet —
/// new since — at the bottom, in the order they came.
pub fn in_order<'a>(came: Vec<&'a Row>, kept: &[String]) -> Vec<&'a Row> {
    let place = |r: &Row| kept.iter().position(|x| *x == r.key);
    let (mut placed, new): (Vec<&'a Row>, Vec<&'a Row>) = came.into_iter().partition(|r| place(r).is_some());
    placed.sort_by_cached_key(|r| place(r));
    placed.into_iter().chain(new).collect()
}

/// A project's rows as they stand on the list with nothing narrowing it
/// (`full`): the order `in_order` keeps.
pub fn standing(full: &[Line], group: &str) -> Vec<String> {
    let Some(head) = full.iter().position(|l| l.key() == group) else {
        return Vec::new();
    };
    full[head + 1..]
        .iter()
        .take_while(|l| !matches!(l, Line::Group(_)))
        .map(Line::key)
        .collect()
}

/// A project's header's key — not a state heading's.
pub fn project_key(line: &Line) -> Option<String> {
    match line {
        Line::Group(h) if h.is_project() => Some(h.key.clone()),
        _ => None,
    }
}

/// The projects in the order they stand: the order kept for them, what it
/// has that the list does not show now keeping its place, then those it
/// does not know, as the list has them (`full`, nothing narrowing it).
pub fn projects_standing(full: &[Line], kept: &[Order]) -> Vec<String> {
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

/// The order to keep once the line `key` moves one place up (or down) among
/// what is `shown`: its project's whole order, from `full` — the list with
/// nothing narrowing it — so rows a search or a toggle leaves out keep theirs.
/// Rows move only inside their project; a header moves among the projects.
/// Grouped by state, and in the band above the projects, nothing moves, and
/// it says why.
pub fn reorder(
    shown: &[Line],
    full: &[Line],
    kept: &[Order],
    pins: &[String],
    key: &str,
    up: bool,
) -> Result<Order, &'static str> {
    let at = shown
        .iter()
        .position(|l| l.key() == key)
        .ok_or("nothing here to move")?;
    if let Line::Group(h) = &shown[at] {
        if h.is_project() {
            return move_project(shown, full, kept, key, up);
        }
        return Err("by state, rows stand oldest first · ^s groups them by project");
    }
    let Some(head) = shown[..at].iter().rposition(|l| matches!(l, Line::Group(_))) else {
        return Err(if pins.iter().any(|p| p == key) {
            "a pinned row keeps the order it was pinned in · ^t lets it go back"
        } else {
            "it needs you, so it stays at the top until it is dealt with"
        });
    };
    let Line::Group(h) = &shown[head] else {
        unreachable!("rposition found a group")
    };
    if !h.is_project() {
        return Err("by state, rows stand oldest first · ^s groups them by project");
    }
    let next = if up { at.checked_sub(1) } else { Some(at + 1) };
    let other = match next.and_then(|i| shown.get(i)) {
        Some(Line::Session(r)) => r.key.clone(),
        _ if up => return Err("already at the top of its project"),
        _ => return Err("already at the bottom of its project"),
    };
    let group = h.key.clone();
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
pub fn move_project(shown: &[Line], full: &[Line], kept: &[Order], key: &str, up: bool) -> Result<Order, &'static str> {
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

/// `(need you, working, finished)` over what the list shows.
pub fn counts(lines: &[Line]) -> (usize, usize, usize) {
    let mut c = (0, 0, 0);
    for r in lines.iter().filter_map(Line::row) {
        use RunState::*;
        match r.state {
            NeedsInput | Blocked | Failed => c.0 += 1,
            Starting | Working | Stale | Idle => c.1 += 1,
            Finished | Stopped => c.2 += 1,
        }
    }
    c
}

/// The header line: what needs you first, then what is moving.
pub fn headline(lines: &[Line], filter: &str) -> String {
    let (need, working, fin) = counts(lines);
    let mut parts = Vec::new();
    if need > 0 {
        parts.push(format!("{need} need{} you", if need == 1 { "s" } else { "" }));
    }
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if fin > 0 {
        parts.push(format!("{fin} finished"));
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
