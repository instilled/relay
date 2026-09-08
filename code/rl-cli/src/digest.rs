//! Grouping and text rendering of the digest, shared by `rl since` and the
//! since-you-left screen.

use crate::rank;
use crate::render::{fmt_clock, state_label};
use jiff::Timestamp;
use relay_core::time::fmt_age;
use relay_proto::methods::{Digest, DigestItem};
use relay_proto::types::RunState;

pub struct Group<'a> {
    pub key: String,
    pub items: Vec<&'a DigestItem>,
}

/// Attention-ranked items, grouped by group key in order of first appearance.
pub fn group<'a, I>(items: I) -> Vec<Group<'a>>
where
    I: IntoIterator<Item = &'a DigestItem>,
{
    let mut items: Vec<&DigestItem> = items.into_iter().collect();
    items.sort_by(|a, b| {
        rank::score(&a.run)
            .cmp(&rank::score(&b.run))
            .then_with(|| a.run.group_key().cmp(&b.run.group_key()))
            .then_with(|| b.at.cmp(&a.at))
    });
    let mut groups: Vec<Group> = Vec::new();
    for it in items {
        let key = it.run.group_key();
        match groups.iter_mut().find(|g| g.key == key) {
            Some(g) => g.items.push(it),
            None => groups.push(Group { key, items: vec![it] }),
        }
    }
    groups
}

/// What the since screen shows: news since the boundary, and anything unseen.
pub fn since_items(d: &Digest) -> Vec<&DigestItem> {
    d.items.iter().filter(|i| i.at >= d.since || i.unseen).collect()
}

/// `(need you, finished)` counts.
pub fn counts<'a>(items: impl IntoIterator<Item = &'a DigestItem>) -> (usize, usize) {
    let mut need = 0;
    let mut fin = 0;
    for i in items {
        if i.run.state.needs_attention() {
            need += 1;
        } else if i.run.state == RunState::Finished {
            fin += 1;
        }
    }
    (need, fin)
}

pub fn headline(since: Timestamp, need: usize, fin: usize, now: Timestamp) -> String {
    let mut s = format!("since {} ({} ago)", fmt_clock(since, now), fmt_age(since, now));
    if need > 0 {
        s.push_str(&format!(" · {need} need{} you", if need == 1 { "s" } else { "" }));
    }
    if fin > 0 {
        s.push_str(&format!(" · {fin} finished"));
    }
    s
}

/// `rl since` output.
pub fn text(d: &Digest, now: Timestamp) -> String {
    let items = since_items(d);
    if items.is_empty() {
        return format!(
            "nothing since {} ({} ago)\n",
            fmt_clock(d.since, now),
            fmt_age(d.since, now)
        );
    }
    let (need, fin) = counts(items.iter().copied());
    let mut out = headline(d.since, need, fin, now);
    out.push('\n');
    for g in group(items) {
        out.push('\n');
        out.push_str(&g.key);
        out.push('\n');
        for i in g.items {
            let mark = if i.unseen { '▸' } else { ' ' };
            out.push_str(&format!(
                "  {mark} {}  {:<12} {:>4}  {}\n",
                i.run.id,
                state_label(i.run.state, i.run.confidence),
                fmt_age(i.at, now),
                i.summary
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_proto::types::{Confidence, Run, RunKind};

    fn item(id: &str, state: RunState, group: &str, at: &str, unseen: bool, summary: &str) -> DigestItem {
        let t: Timestamp = at.parse().unwrap();
        let mut r = Run::new(id, RunKind::Agent, format!("title {id}"), vec![], "/w", "/w", t);
        r.state = state;
        r.confidence = Confidence::Exact;
        r.ref_ = Some(group.into());
        DigestItem {
            run: r,
            summary: summary.into(),
            at: t,
            unseen,
            joined_by: None,
        }
    }

    #[test]
    fn groups_rank_and_render() {
        let d = Digest {
            since: "2026-09-08T09:12:00Z".parse().unwrap(),
            items: vec![
                item(
                    "done1",
                    RunState::Finished,
                    "notion:FL-123",
                    "2026-09-08T11:00:00Z",
                    false,
                    "exit 0 · 41m03s",
                ),
                item("old00", RunState::Idle, "relay", "2026-09-08T08:00:00Z", false, "idle"),
                item(
                    "ask00",
                    RunState::NeedsInput,
                    "notion:FL-123",
                    "2026-09-08T11:48:00Z",
                    true,
                    "permission: Bash",
                ),
                item(
                    "fail0",
                    RunState::Failed,
                    "nightly",
                    "2026-09-08T10:00:00Z",
                    true,
                    "exit 2 · 3m",
                ),
            ],
        };
        let now: Timestamp = "2026-09-08T12:00:00Z".parse().unwrap();
        let shown = since_items(&d);
        assert_eq!(shown.len(), 3, "the old idle run is not news");
        let groups = group(shown);
        assert_eq!(
            groups.iter().map(|g| g.key.as_str()).collect::<Vec<_>>(),
            ["notion:FL-123", "nightly"]
        );
        assert_eq!(
            groups[0].items.iter().map(|i| i.run.id.as_str()).collect::<Vec<_>>(),
            ["ask00", "done1"]
        );
        let t = text(&d, now);
        assert!(t.starts_with("since "), "{t}");
        assert!(t.contains("2 need you · 1 finished"), "{t}");
        assert!(t.contains("  ▸ ask00  needs_input   12m  permission: Bash"), "{t}");
        assert!(t.contains("    done1  finished       1h  exit 0 · 41m03s"), "{t}");
    }
}
