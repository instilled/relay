//! End to end over the stored-lead path: a posted lead is read back from the
//! ledger and assembled into the report under its band.

use rl_proto::lead::{Band, Lead, LeadStatus};
use std::time::Duration;

#[tokio::test]
async fn a_posted_lead_appears_in_the_report_under_its_band() {
    let ledger = rl_ledger::Ledger::open_in_memory().unwrap();
    let now = jiff::Timestamp::now();
    let lead = Lead {
        source: "feedback".into(),
        key: "triage".into(),
        band: Band::NeedsYou,
        title: "5 to triage".into(),
        summary: None,
        scope: None,
        action: Some("echo triage".into()),
        status: LeadStatus::Open,
        at: now,
        expires_at: None,
    };
    let posted = lead.clone();
    ledger.call(move |c| rl_ledger::leads::post(c, &posted)).await.unwrap();
    let stored = ledger.call(move |c| rl_ledger::leads::list(c, now)).await.unwrap();
    assert_eq!(stored.len(), 1);

    let since = now - Duration::from_secs(3600);
    let report = rl_daemon::report::assemble(vec![], stored, None, since, now);
    assert_eq!(report.needs_you.len(), 1);
    assert_eq!(report.needs_you[0].key, "triage");
    assert_eq!(report.needs_you[0].title, "5 to triage");
    assert!(report.live.is_empty() && report.happened.is_empty());
}
