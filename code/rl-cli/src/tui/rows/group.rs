//! What a row belongs to — its group, whether it comes onto the list, and whether you have it hidden.

use super::*;

/// Whether a run's kind and cause let it onto the list, given the toggles.
/// A flow's run (by cause) is the flow's, whatever its kind: it shows when
/// `show_flows`, and then only. Any other run: one that is neither agent nor
/// shell is always shown; an agent run (by kind) when `show_agents`; a shell
/// run (by kind) when `show_shells`. Whether it is hidden is `build`'s to say.
/// Questions ignore all of it: they are what needs you.
pub(crate) fn keeps(r: &Run, show_agents: bool, show_flows: bool, show_shells: bool) -> bool {
    if matches!(r.cause, Cause::Flow { .. }) {
        return show_flows;
    }
    let is_agent = r.kind == RunKind::Agent;
    let is_shell = r.is_shell();
    (!is_agent && !is_shell) || (is_agent && show_agents) || (is_shell && show_shells)
}

/// The group a flow's row stands in, as `(key, folder)`: the folder it is
/// `in` (as the daemon resolved it, so it is the project's own path), else
/// `everywhere` — also for a flow not loaded now, whose `in` is not known.
pub(crate) fn flow_group(flows: &[Flow], name: &str) -> (String, PathBuf) {
    match flows.iter().find(|f| f.name == name).and_then(|f| f.folder.clone()) {
        Some(p) => (p.to_string_lossy().into_owned(), p),
        None => (EVERYWHERE_KEY.to_string(), PathBuf::new()),
    }
}

/// The group a run is shown under, as `(key, folder)`: an ended flow run's
/// is its flow's row's; a flow's run still going outside every project —
/// in a folder that is not a registered project, filed under no ref — is
/// `everywhere`'s; anything else goes under its ref, else its folder.
pub(crate) fn group_of(r: &Run, names: &Names, flows: &[Flow]) -> (String, PathBuf) {
    if let Some(flow) = flow_of(r) {
        return flow_group(flows, flow);
    }
    if matches!(r.cause, Cause::Flow { .. }) && r.ref_.is_none() && names.registered(&r.workspace).is_none() {
        return (EVERYWHERE_KEY.to_string(), PathBuf::new());
    }
    (r.place_key(), r.workspace.clone())
}

/// Whether a run is shown under `everywhere`: a flow's, outside every
/// project or folded into a flow with no `in`.
pub fn everywhere_run(r: &Run, names: &Names, flows: &[Flow]) -> bool {
    group_of(r, names, flows).0 == EVERYWHERE_KEY
}

/// The group a run is shown under: the `everywhere` one, or its ref, else
/// its folder — or, ended and a flow's, its flow's row's.
pub fn group_key(r: &Run, names: &Names, flows: &[Flow]) -> String {
    group_of(r, names, flows).0
}

/// The key a header is hidden by, as its row's own: `g:<folder>`, or
/// `g:everywhere`.
pub(crate) fn head_key((key, workspace): &(String, PathBuf)) -> String {
    if key == EVERYWHERE_KEY {
        format!("g:{EVERYWHERE}")
    } else {
        format!("g:{}", workspace.display())
    }
}

/// The flow a run is a step of, by its row's key, whether or not it has ended.
pub(crate) fn flow_key_of(r: &Run) -> Option<String> {
    match &r.cause {
        Cause::Flow { flow, .. } => Some(flow_key(flow)),
        _ => None,
    }
}

/// What hides a run from the list — its own `/hide`, its project's, its
/// flow's — and since when: the latest of them, the last time you said so.
/// `None`: it is on the list.
pub fn hidden_since(r: &Run, names: &Names, flows: &[Flow], hidden: &[Hidden]) -> Option<Timestamp> {
    let group = group_of(r, names, flows);
    let at = |k: &str| hidden.iter().find(|h| h.key == k).map(|h| h.at);
    let head = at(&head_key(&group));
    let flow = flow_key_of(r).and_then(|k| at(&k));
    [r.hidden_at, head, flow].into_iter().flatten().max()
}

/// Whether what hides a run is its project or its flow rather than its own
/// `/hide` — what bringing the run back alone cannot undo.
pub fn hidden_with(r: &Run, names: &Names, flows: &[Flow], hidden: &[Hidden]) -> Option<&'static str> {
    let group = group_of(r, names, flows);
    let has = |k: &str| hidden.iter().any(|h| h.key == k);
    if has(&head_key(&group)) {
        Some("its project")
    } else if flow_key_of(r).is_some_and(|k| has(&k)) {
        Some("its flow")
    } else {
        None
    }
}

/// A hidden run that has come to need you since it was hidden — asking,
/// blocked or failed after you said so. It comes through to the list.
pub fn comes_through(r: &Run, since: Timestamp) -> bool {
    matches!(r.state, RunState::NeedsInput | RunState::Blocked | RunState::Failed) && r.state_at > since
}

/// The flows `in` one folder (`None`: those with no `in`) as chips.
pub fn chips(flows: &[Flow], scope: Option<&Path>, now: Timestamp) -> Vec<Chip> {
    let mut out: Vec<Chip> = flows
        .iter()
        .filter(|f| f.removed_at.is_none())
        .filter(|f| f.folder.as_deref() == scope)
        .map(|f| Chip {
            name: f.name.clone(),
            status: f.status(),
            due: match (f.status(), f.next_at) {
                (FlowStatus::On, Some(t)) if t > now => Some(fmt_age(now, t)),
                (FlowStatus::On, Some(_)) => Some("due".into()),
                _ => None,
            },
        })
        .collect();
    out.sort_by(|a, b| b.needs_yes().cmp(&a.needs_yes()).then_with(|| a.name.cmp(&b.name)));
    out
}

/// The flows `in` one folder (`None`: those with no `in`), tallied from the digest.
pub fn tally(flows: &[Flow], scope: Option<&Path>, d: &Digest) -> FlowTally {
    let mut out = FlowTally::default();
    for f in flows
        .iter()
        .filter(|f| f.removed_at.is_none() && f.folder.as_deref() == scope)
    {
        out.total += 1;
        let live = |s| matches!(s, FiringStatus::Running | FiringStatus::Waiting);
        out.running += usize::from(d.firings.iter().any(|g| g.flow == f.name && live(g.status)));
        out.needs += usize::from(flow_needs(f, d));
    }
    out
}

/// Whether one flow needs you: it waits for a yes; a question is open from
/// its firing or one of its runs; a run wants you; or a run failed unseen.
pub fn flow_needs(f: &Flow, d: &Digest) -> bool {
    let mine = |i: &&DigestItem| matches!(&i.run.cause, Cause::Flow { flow, .. } if *flow == f.name);
    f.status() == FlowStatus::NeedsApproval
        || d.items.iter().filter(mine).any(|i| match i.run.state {
            RunState::NeedsInput | RunState::Blocked => true,
            RunState::Failed => i.unseen,
            _ => false,
        })
        || d.decisions.iter().any(|q| {
            firing_of(q, &d.firings).is_some_and(|g| g.flow == f.name)
                || q.run
                    .as_ref()
                    .is_some_and(|id| d.items.iter().filter(mine).any(|i| &i.run.id == id))
        })
}

/// Which flow an ended run folds into, if any.
pub fn flow_of(r: &Run) -> Option<&str> {
    if !r.state.is_terminal() {
        return None;
    }
    match &r.cause {
        Cause::Flow { flow, .. } => Some(flow),
        _ => None,
    }
}

/// A flow's row key.
pub fn flow_key(name: &str) -> String {
    format!("f:{name}")
}

/// Whether a flow has a row of its own, run or not: every flow defined now —
/// waiting for its first yes, on, paused, or changed since — until it is
/// removed. Its row is where you see it and act on it, not a header chip.
pub(crate) fn stands_on_the_list(f: &Flow) -> bool {
    f.removed_at.is_none()
}

/// Whether a flow's row is hidden — by its own `/hide` or its project's —
/// by the keys `hidden_since` reads for its runs.
pub(crate) fn flow_hidden(name: &str, flows: &[Flow], hidden: &[Hidden]) -> bool {
    let keys = [head_key(&flow_group(flows, name)), flow_key(name)];
    hidden.iter().any(|h| keys.contains(&h.key))
}

/// The group one of an agent's own sessions stands in, as `(key, folder)`:
/// the folder `rld` filed it under, as a run started in its `cwd` would be
/// (`workspace`). Where `rld` has not said, a guess: the registered folder
/// its `cwd` is in, the deepest where they nest, else the `cwd` itself.
pub(crate) fn outside_group(s: &Outside, names: &Names) -> (String, PathBuf) {
    let folder = s
        .workspace
        .clone()
        .or_else(|| names.containing(&s.cwd))
        .unwrap_or_else(|| s.cwd.clone());
    (folder.to_string_lossy().into_owned(), folder)
}

/// What hides one of an agent's own sessions — its own `/hide` or its
/// project's — and since when: the later of them. `None`: it is on the list.
pub(crate) fn outside_hidden_since(s: &Outside, names: &Names, hidden: &[Hidden]) -> Option<Timestamp> {
    let keys = [head_key(&outside_group(s, names)), s.key()];
    hidden.iter().filter(|h| keys.contains(&h.key)).map(|h| h.at).max()
}

/// A group's name: `everywhere`, or its ref or folder in words.
pub(crate) fn group_name(key: &str, names: &Names) -> String {
    if key == EVERYWHERE_KEY {
        EVERYWHERE.to_string()
    } else {
        names.group(key)
    }
}
