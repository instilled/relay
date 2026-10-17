//! The read pool: reads run on read-only connections beside the writer, so a
//! slow query never stalls a write, and a read connection refuses to write.

use rl_ledger::{Ledger, meta};
use std::sync::mpsc;

fn open(dir: &std::path::Path) -> Ledger {
    Ledger::open(&dir.join("ledger.db")).unwrap()
}

#[test]
fn a_write_goes_through_while_a_read_is_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let l = open(dir.path());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();

    // A read that takes a real snapshot, says so, then hangs on the channel —
    // occupying one reader connection for the duration.
    let reader = {
        let l = l.clone();
        std::thread::spawn(move || {
            l.call_read_blocking(move |c| {
                let n: i64 = c.query_row("SELECT count(*) FROM runs", [], |r| r.get(0))?;
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(n)
            })
            .unwrap()
        })
    };
    started_rx.recv().unwrap();

    // With a read still in flight, a write must complete — not queue behind it.
    l.call_blocking(|c| meta::set(c, "k", "v")).unwrap();
    assert_eq!(l.call_blocking(|c| meta::get(c, "k")).unwrap(), Some("v".to_string()));

    release_tx.send(()).unwrap();
    assert_eq!(reader.join().unwrap(), 0);
}

#[test]
fn a_read_connection_refuses_a_write() {
    let dir = tempfile::tempdir().unwrap();
    let l = open(dir.path());
    let wrote = l.call_read_blocking(|c| Ok(c.execute("INSERT INTO meta (key, value) VALUES ('x', 'y')", [])));
    // The call itself succeeds; the write inside it is refused by SQLite.
    assert!(wrote.unwrap().is_err(), "a read-only connection must not write");
    assert_eq!(l.call_blocking(|c| meta::get(c, "x")).unwrap(), None);
}

#[test]
fn in_memory_reads_fall_back_to_the_writer() {
    // No pool for an in-memory ledger; call_read routes to the writer thread
    // so it still sees what was written.
    let l = Ledger::open_in_memory().unwrap();
    l.call_blocking(|c| meta::set(c, "k", "v")).unwrap();
    assert_eq!(
        l.call_read_blocking(|c| meta::get(c, "k")).unwrap(),
        Some("v".to_string())
    );
}
