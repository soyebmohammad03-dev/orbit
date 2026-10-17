//! Concurrent appends, optimistic concurrency and transaction rollback.

mod common;

use std::collections::HashSet;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use common::{TestDb, req, sid};
use orbit_core::{EventStore, Metadata, Sequence, StoreError};

fn assert_dense_and_valid(store: &EventStore, id: &orbit_core::StreamId, expected: u64) {
    let events = store.read_stream(id).unwrap();
    assert_eq!(events.len() as u64, expected);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.sequence.get(), index as u64 + 1);
    }
    let ids: HashSet<_> = events.iter().map(|e| e.event_id).collect();
    assert_eq!(ids.len() as u64, expected);
    let report = store.verify_stream(id).unwrap();
    assert!(report.is_valid(), "{report:?}");
    assert_eq!(report.events_checked, expected);
}

#[test]
fn threads_sharing_one_store_never_duplicate_sequences() {
    const THREADS: u64 = 8;
    const PER_THREAD: u64 = 40;
    let db = TestDb::new();
    let store = Arc::new(db.open());
    let id = sid("shared");
    store.create_stream(&id, Metadata::new()).unwrap();

    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let store = Arc::clone(&store);
            let id = id.clone();
            thread::spawn(move || {
                for n in 0..PER_THREAD {
                    store.append(&id, req(t * 1_000 + n)).unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_dense_and_valid(&store, &id, THREADS * PER_THREAD);
}

#[test]
fn independent_connections_never_duplicate_sequences() {
    const WRITERS: u64 = 4;
    const PER_WRITER: u64 = 40;
    let db = TestDb::new();
    let id = sid("multi-conn");
    db.open().create_stream(&id, Metadata::new()).unwrap();

    let barrier = Arc::new(Barrier::new(WRITERS as usize));
    let handles: Vec<_> = (0..WRITERS)
        .map(|w| {
            // Each writer has its own connection, as separate processes would.
            let store =
                EventStore::open(db.config().with_busy_timeout(Duration::from_secs(30))).unwrap();
            let id = id.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for n in 0..PER_WRITER {
                    store.append(&id, req(w * 1_000 + n)).unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_dense_and_valid(&db.open(), &id, WRITERS * PER_WRITER);
}

#[test]
fn stale_expected_sequence_is_rejected() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("occ");
    store.create_stream(&id, Metadata::new()).unwrap();

    store
        .append(&id, req(1).expect_sequence(Sequence::ZERO))
        .unwrap();
    store
        .append(&id, req(2).expect_sequence(Sequence::FIRST))
        .unwrap();

    let err = store
        .append(&id, req(3).expect_sequence(Sequence::FIRST))
        .unwrap_err();
    match err {
        StoreError::SequenceConflict {
            stream,
            expected,
            actual,
        } => {
            assert_eq!(stream, id);
            assert_eq!(expected, Sequence::FIRST);
            assert_eq!(actual.get(), 2);
        }
        other => panic!("expected conflict, got {other}"),
    }
    // Expecting a head beyond the real one is also a conflict.
    assert!(matches!(
        store.append(&id, req(3).expect_sequence(Sequence::new(9).unwrap())),
        Err(StoreError::SequenceConflict { .. })
    ));
    assert_eq!(store.head(&id).unwrap().sequence.get(), 2);
}

#[test]
fn racing_optimistic_writers_exactly_one_wins() {
    const WRITERS: usize = 6;
    let db = TestDb::new();
    let id = sid("race");
    db.open().create_stream(&id, Metadata::new()).unwrap();

    let barrier = Arc::new(Barrier::new(WRITERS));
    let handles: Vec<_> = (0..WRITERS)
        .map(|w| {
            let store =
                EventStore::open(db.config().with_busy_timeout(Duration::from_secs(30))).unwrap();
            let id = id.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let head = store.head(&id).unwrap().sequence;
                barrier.wait();
                store.append(&id, req(w as u64).expect_sequence(head))
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(winners, 1);
    for result in &results {
        if let Err(err) = result {
            assert!(
                matches!(err, StoreError::SequenceConflict { actual, .. } if *actual == Sequence::FIRST),
                "{err}"
            );
        }
    }
    assert_eq!(db.open().head(&id).unwrap().sequence, Sequence::FIRST);
}

#[test]
fn held_write_lock_yields_typed_busy_error() {
    let db = TestDb::new();
    let id = sid("busy");
    let store = EventStore::open(db.config().with_busy_timeout(Duration::from_millis(50))).unwrap();
    store.create_stream(&id, Metadata::new()).unwrap();

    let raw = db.raw();
    raw.execute_batch("BEGIN IMMEDIATE").unwrap();
    let err = store.append(&id, req(1)).unwrap_err();
    assert!(matches!(err, StoreError::Busy), "{err}");
    raw.execute_batch("ROLLBACK").unwrap();

    // The failed attempt left nothing behind and the store is usable again.
    assert!(store.head(&id).unwrap().is_empty());
    assert_eq!(store.append(&id, req(1)).unwrap().sequence, Sequence::FIRST);
}

#[test]
fn failure_after_insert_rolls_back_the_whole_append() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("rollback");
    store.create_stream(&id, Metadata::new()).unwrap();
    store.append(&id, req(1)).unwrap();

    // Inject a failure into the head update, which runs after the event insert.
    db.raw()
        .execute_batch(
            "CREATE TRIGGER inject_failure BEFORE UPDATE ON streams
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .unwrap();
    let err = store.append(&id, req(2)).unwrap_err();
    assert!(err.to_string().contains("injected failure"), "{err}");

    let count: i64 = db
        .raw()
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "the inserted event must have been rolled back");
    assert_eq!(store.head(&id).unwrap().sequence, Sequence::FIRST);

    db.raw()
        .execute_batch("DROP TRIGGER inject_failure")
        .unwrap();
    assert_eq!(store.append(&id, req(2)).unwrap().sequence.get(), 2);
    assert!(store.verify_stream(&id).unwrap().is_valid());
}
