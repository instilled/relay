//! Tags set through the ledger ride on the report's `tags` overlay.

use rl_proto::methods::Report;

#[tokio::test]
async fn a_tagged_source_rides_on_the_report() {
    let led = rl_ledger::Ledger::open_in_memory().unwrap();
    led.call(|c| rl_ledger::tags::set(c, "claude", "dev")).await.unwrap();
    let tags = led.call_read(rl_ledger::tags::list).await.unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].tag, "dev");

    let report = Report {
        tags,
        ..Report::default()
    };
    assert_eq!(report.tags[0].source, "claude");
    // `tags` is optional on the wire.
    let back: Report = serde_json::from_str(r#"{"needs_you":[],"live":[],"happened":[]}"#).unwrap();
    assert!(back.tags.is_empty());
}
