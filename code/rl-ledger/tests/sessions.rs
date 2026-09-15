use jiff::Timestamp;
use rl_ledger::session_io::{self, Direction};
use rl_ledger::sessions::{self, SessionRow};
use rl_ledger::{Ledger, runs};
use rl_proto::types::{Run, RunKind};

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn seed(l: &Ledger, id: &str) {
    let r = Run::new(
        id,
        RunKind::Command,
        "t",
        vec!["t".into()],
        "/w",
        "/w",
        ts("2026-09-08T10:00:00Z"),
    );
    l.call_blocking(move |c| runs::insert(c, &r)).unwrap();
}

#[test]
fn sessions_roundtrip_and_open_list() {
    let l = Ledger::open_in_memory().unwrap();
    seed(&l, "aaaaaaaa");
    seed(&l, "bbbbbbbb");
    let row = SessionRow {
        run: "aaaaaaaa".into(),
        socket: "/s/a.sock".into(),
        exit_file: "/r/a.exit".into(),
        log: "/l/a.log".into(),
        pid: 10,
        holder_pid: 9,
        tty: true,
        cols: 80,
        rows: 24,
        started_at: ts("2026-09-08T10:00:00Z"),
        attached_at: None,
        closed_at: None,
    };
    let b = SessionRow {
        run: "bbbbbbbb".into(),
        tty: false,
        ..row.clone()
    };
    l.call_blocking({
        let (row, b) = (row.clone(), b.clone());
        move |c| {
            sessions::insert(c, &row)?;
            sessions::insert(c, &b)
        }
    })
    .unwrap();
    assert_eq!(l.call_blocking(|c| sessions::get(c, "aaaaaaaa")).unwrap(), Some(row));
    assert_eq!(l.call_blocking(|c| sessions::list_open(c)).unwrap().len(), 2);

    let t = ts("2026-09-08T10:01:00Z");
    l.call_blocking(move |c| {
        sessions::set_attached(c, "aaaaaaaa", t)?;
        sessions::set_size(c, "aaaaaaaa", 120, 40)?;
        sessions::set_closed(c, "bbbbbbbb", t)
    })
    .unwrap();
    let a = l.call_blocking(|c| sessions::get(c, "aaaaaaaa")).unwrap().unwrap();
    assert_eq!((a.attached_at, a.cols, a.rows), (Some(t), 120, 40));
    let open = l.call_blocking(|c| sessions::list_open(c)).unwrap();
    assert_eq!(open.iter().map(|s| s.run.as_str()).collect::<Vec<_>>(), ["aaaaaaaa"]);
}

#[test]
fn session_io_sequences_and_tail() {
    let l = Ledger::open_in_memory().unwrap();
    seed(&l, "aaaaaaaa");
    let t = ts("2026-09-08T10:00:00Z");
    let seqs = l
        .call_blocking(move |c| {
            Ok((
                session_io::append(c, "aaaaaaaa", Direction::Out, b"hello ", t)?,
                session_io::append(c, "aaaaaaaa", Direction::In, b"x\n", t)?,
                session_io::append(c, "aaaaaaaa", Direction::Out, b"world\n", t)?,
            ))
        })
        .unwrap();
    assert_eq!(seqs, (1, 2, 3));
    assert_eq!(l.call_blocking(|c| session_io::last_seq(c, "aaaaaaaa")).unwrap(), 3);
    assert_eq!(l.call_blocking(|c| session_io::last_seq(c, "nope")).unwrap(), 0);

    let after1 = l.call_blocking(|c| session_io::read(c, "aaaaaaaa", 1, 10)).unwrap();
    assert_eq!(after1.iter().map(|c| c.seq).collect::<Vec<_>>(), [2, 3]);
    assert_eq!(after1[0].direction, Direction::In);

    assert_eq!(
        l.call_blocking(|c| session_io::tail(c, "aaaaaaaa", 1024)).unwrap(),
        b"hello world\n"
    );
    assert_eq!(
        l.call_blocking(|c| session_io::tail(c, "aaaaaaaa", 4)).unwrap(),
        b"rld\n"
    );

    // Notes are neither output bytes nor part of the tail.
    l.call_blocking(move |c| session_io::append(c, "aaaaaaaa", Direction::Note, b"[relay] gap", t))
        .unwrap();
    assert_eq!(l.call_blocking(|c| session_io::out_bytes(c, "aaaaaaaa")).unwrap(), 12);
    assert_eq!(l.call_blocking(|c| session_io::out_bytes(c, "nope")).unwrap(), 0);
    assert_eq!(
        l.call_blocking(|c| session_io::tail(c, "aaaaaaaa", 1024)).unwrap(),
        b"hello world\n"
    );
    let notes = l.call_blocking(|c| session_io::read(c, "aaaaaaaa", 3, 10)).unwrap();
    assert_eq!(notes[0].direction, Direction::Note);
}
