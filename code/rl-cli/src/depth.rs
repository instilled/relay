//! How deep the report tells you what happened, by how long you were away.

use jiff::SignedDuration;
use rl_proto::config::ReportConfig;

/// More assistant calls than this and the report asks first, unless
/// `report.summary: auto`.
pub const ASK_OVER: u32 = 3;

/// What the report says: the headline alone, a written brief, a deep read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Headline,
    Brief,
    Deep,
}

/// How deep to go for this long away: up to `brief_after` the headline,
/// past it a brief, past `deep_after` a deep read.
pub fn depth(away: SignedDuration, r: &ReportConfig) -> Depth {
    if away > r.deep_after() {
        Depth::Deep
    } else if away > r.brief_after() {
        Depth::Brief
    } else {
        Depth::Headline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: i64) -> SignedDuration {
        SignedDuration::from_hours(n)
    }
    fn m(n: i64) -> SignedDuration {
        SignedDuration::from_mins(n)
    }

    #[test]
    fn depth_is_the_headline_up_to_two_hours() {
        let r = ReportConfig::default();
        assert_eq!(depth(m(5), &r), Depth::Headline);
        assert_eq!(depth(h(2), &r), Depth::Headline);
    }

    #[test]
    fn depth_is_brief_past_two_hours() {
        let r = ReportConfig::default();
        assert_eq!(depth(h(2) + m(1), &r), Depth::Brief);
        assert_eq!(depth(h(48), &r), Depth::Brief);
    }

    #[test]
    fn depth_is_deep_past_two_days() {
        let r = ReportConfig::default();
        assert_eq!(depth(h(48) + m(1), &r), Depth::Deep);
    }

    #[test]
    fn depth_follows_configured_thresholds() {
        let r = ReportConfig {
            brief_after: Some("30m".into()),
            ..ReportConfig::default()
        };
        assert_eq!(depth(m(30), &r), Depth::Headline);
        assert_eq!(depth(m(31), &r), Depth::Brief);
    }
}
