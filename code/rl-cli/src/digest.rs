//! What the digest says, shared by the panel and `rl since`: what is news
//! since the boundary, and what a run raised.

use rl_proto::methods::{Digest, DigestItem};
use rl_proto::types::Signal;

/// What a run raised, by kind: `flint.product.new-feedback ×5`, a kind on
/// its own when it came once.
pub fn signals(emitted: &[Signal]) -> String {
    emitted
        .iter()
        .map(|s| {
            if s.count > 1 {
                format!("{} ×{}", s.kind, s.count)
            } else {
                s.kind.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// What the catch-up counts as news: what moved since the boundary, and
/// anything not looked at yet.
pub fn since_items(d: &Digest) -> Vec<&DigestItem> {
    d.items.iter().filter(|i| i.at >= d.since || i.unseen).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use rl_proto::types::{Run, RunKind};

    #[test]
    fn news_is_what_moved_since_or_was_not_looked_at() {
        let item = |id: &str, at: &str, unseen: bool| {
            let t: Timestamp = at.parse().unwrap();
            DigestItem {
                run: Run::new(id, RunKind::Command, id, vec![], "/w", "/w", t),
                summary: String::new(),
                at: t,
                unseen,
                joined_by: None,
                emitted: vec![],
                cwd: None,
                recap: None,
            }
        };
        let d = Digest {
            since: "2026-09-08T09:00:00Z".parse().unwrap(),
            items: vec![
                item("moved", "2026-09-08T10:00:00Z", false),
                item("old", "2026-09-08T08:00:00Z", false),
                item("unseen", "2026-09-08T08:00:00Z", true),
            ],
            decisions: vec![],
            checkouts: vec![],
            lost: vec![],
            hidden: vec![],
            order: vec![],
            firings: Vec::new(),
            outside: vec![],
        };
        let ids: Vec<&str> = since_items(&d).iter().map(|i| i.run.id.as_str()).collect();
        assert_eq!(ids, ["moved", "unseen"]);
    }
}
