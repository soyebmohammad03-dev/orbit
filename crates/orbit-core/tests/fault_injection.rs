//! Deterministic fault injection at every write-path boundary.
//!
//! A hook makes the store fail at a named point exactly as a real error there
//! would. These tests check that no failed operation leaves committed state
//! behind, that the store keeps working afterwards, and that everything
//! committed survives reopening. (Process crashes are in `crash_recovery.rs`.)

mod common;

use std::sync::{Arc, Mutex};

use common::{TestDb, at, counter_stream, inc, seqn, sid, triangle};
use orbit_core::fault::{FaultHook, FaultPoint};
use orbit_core::projection::examples::Counter;
use orbit_core::replay::{ReplayLab, ReplayStart};
use orbit_core::{
    EventStore, Metadata, ReconstructOptions, ReconstructionError, Reconstructor, Sequence,
    StoreError, Target,
};

fn fail_at(point: FaultPoint) -> FaultHook {
    Arc::new(move |p| p == point)
}

/// Fails at `point` exactly once, then lets everything through.
fn fail_once_at(point: FaultPoint) -> FaultHook {
    let fired = Mutex::new(false);
    Arc::new(move |p| {
        let mut fired = fired.lock().unwrap();
        if p == point && !*fired {
            *fired = true;
            true
        } else {
            false
        }
    })
}

fn event_rows(db: &TestDb) -> i64 {
    db.raw()
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap()
}

fn assert_store_valid(store: &EventStore) {
    let report = store.verify_store().unwrap();
    assert!(report.is_valid(), "{report:#?}");
}

const APPEND_POINTS: [FaultPoint; 5] = [
    FaultPoint::AppendBeforeBegin,
    FaultPoint::AppendAfterBegin,
    FaultPoint::AppendAfterSequenceAllocated,
    FaultPoint::AppendAfterInsert,
    FaultPoint::AppendBeforeCommit,
];

#[test]
fn hooks_visit_the_documented_points_in_order() {
    let db = TestDb::new();
    let store = db.open();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    store.set_fault_hook(Some(Arc::new(move |p| {
        log.lock().unwrap().push(p);
        false
    })));
    let id = sid("s");
    store.create_stream(&id, Metadata::new()).unwrap();
    store.append(&id, inc(1)).unwrap();
    Reconstructor::new(&store, Counter)
        .create_snapshot(&id, Target::Latest, ReconstructOptions::default())
        .unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            FaultPoint::CreateStreamBeforeCommit,
            FaultPoint::AppendBeforeBegin,
            FaultPoint::AppendAfterBegin,
            FaultPoint::AppendAfterSequenceAllocated,
            FaultPoint::AppendAfterInsert,
            FaultPoint::AppendBeforeCommit,
            FaultPoint::SnapshotBeforeBegin,
            FaultPoint::SnapshotAfterValidation,
            FaultPoint::SnapshotBeforeCommit,
        ]
    );
}

#[test]
fn failed_append_at_any_point_leaves_no_committed_state() {
    for point in APPEND_POINTS {
        let db = TestDb::new();
        let id = sid("s");
        let before = {
            let store = db.open();
            counter_stream(&store, &id, 3);
            let before = store.read_stream(&id).unwrap();

            store.set_fault_hook(Some(fail_at(point)));
            let err = store.append(&id, inc(4)).unwrap_err();
            assert!(
                matches!(err, StoreError::InjectedFault(p) if p == point),
                "{point}: {err}"
            );
            // Nothing visible through the store or the raw file.
            assert_eq!(store.head(&id).unwrap().sequence, seqn(3), "{point}");
            assert_eq!(store.read_stream(&id).unwrap(), before, "{point}");
            assert_eq!(event_rows(&db), 3, "{point}");
            assert_store_valid(&store);

            // The same connection keeps working once the fault is gone.
            store.set_fault_hook(None);
            let next = store.append(&id, inc(4)).unwrap();
            assert_eq!(next.sequence, seqn(4), "{point}");
            assert_eq!(next.prev_digest, before[2].digest, "{point}");
            before
        };
        // Everything committed survives reopening; nothing else appears.
        let store = db.open();
        let events = store.read_stream(&id).unwrap();
        assert_eq!(events.len(), 4, "{point}");
        assert_eq!(&events[..3], &before[..], "{point}");
        assert_store_valid(&store);
    }
}

#[test]
fn optimistic_append_after_a_fault_sees_the_unchanged_head() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("s");
    counter_stream(&store, &id, 2);
    store.set_fault_hook(Some(fail_once_at(FaultPoint::AppendBeforeCommit)));
    assert!(store.append(&id, inc(3).expect_sequence(seqn(2))).is_err());
    // The retry with the same expectation succeeds: the failed attempt did not move the head.
    let retried = store.append(&id, inc(3).expect_sequence(seqn(2))).unwrap();
    assert_eq!(retried.sequence, seqn(3));
}

#[test]
fn failed_stream_creation_leaves_no_stream() {
    let db = TestDb::new();
    let store = db.open();
    store.set_fault_hook(Some(fail_at(FaultPoint::CreateStreamBeforeCommit)));
    let id = sid("s");
    assert!(matches!(
        store.create_stream(&id, Metadata::new()),
        Err(StoreError::InjectedFault(
            FaultPoint::CreateStreamBeforeCommit
        ))
    ));
    assert!(!store.stream_exists(&id).unwrap());
    store.set_fault_hook(None);
    store.create_stream(&id, Metadata::new()).unwrap();
}

#[test]
fn failed_snapshot_persistence_leaves_no_snapshot() {
    for point in [
        FaultPoint::SnapshotBeforeBegin,
        FaultPoint::SnapshotAfterValidation,
        FaultPoint::SnapshotBeforeCommit,
    ] {
        let db = TestDb::new();
        let store = db.open();
        let id = sid("s");
        counter_stream(&store, &id, 5);
        let counter = Reconstructor::new(&store, Counter);

        store.set_fault_hook(Some(fail_at(point)));
        let err = counter
            .create_snapshot(&id, at(5), ReconstructOptions::default())
            .unwrap_err();
        assert!(
            matches!(err, ReconstructionError::Store(StoreError::InjectedFault(p)) if p == point),
            "{point}: {err}"
        );
        assert!(store.list_snapshots(&id).unwrap().is_empty(), "{point}");
        // Reconstruction is unaffected and falls back to full replay.
        let r = counter
            .reconstruct(&id, Target::Latest, ReconstructOptions::default())
            .unwrap();
        assert_eq!(r.state.value, triangle(5));

        store.set_fault_hook(None);
        let created = counter
            .create_snapshot(&id, at(5), ReconstructOptions::default())
            .unwrap();
        assert!(created.saved.created, "{point}");
        drop(store);
        let store = db.open();
        assert_eq!(store.list_snapshots(&id).unwrap().len(), 1, "{point}");
        assert_store_valid(&store);
    }
}

#[test]
fn failed_replay_persistence_leaves_no_run_or_execution() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("s");
    counter_stream(&store, &id, 4);
    let lab = ReplayLab::new(&store, Counter);

    store.set_fault_hook(Some(fail_once_at(FaultPoint::ReplayBeforeCommit)));
    assert!(
        lab.record(&id, ReplayStart::Initial, Target::Latest)
            .is_err()
    );
    assert!(lab.runs(&id).unwrap().is_empty());

    let run = lab
        .record(&id, ReplayStart::Initial, Target::Latest)
        .unwrap();
    store.set_fault_hook(Some(fail_once_at(FaultPoint::ReplayBeforeCommit)));
    assert!(lab.execute(run.id).is_err());
    assert!(store.replay_executions(run.id).unwrap().is_empty());
    lab.execute(run.id).unwrap();
    assert_eq!(store.replay_executions(run.id).unwrap().len(), 1);
}

#[test]
fn intermittent_faults_never_corrupt_history() {
    // Every third write attempt fails at a rotating point; the committed
    // history must stay dense, chained and exactly the successful appends.
    let db = TestDb::new();
    let store = db.open();
    let id = sid("s");
    store.create_stream(&id, Metadata::new()).unwrap();
    let attempt = Arc::new(Mutex::new(0u64));
    let counter = Arc::clone(&attempt);
    store.set_fault_hook(Some(Arc::new(move |p| {
        if !APPEND_POINTS.contains(&p) {
            return false;
        }
        let n = *counter.lock().unwrap();
        // Rotate through the points across attempts; fail one in three attempts.
        n.is_multiple_of(3) && APPEND_POINTS[usize::try_from(n / 3 % 5).unwrap()] == p
    })));

    let mut committed = Vec::new();
    for n in 0..60u64 {
        *attempt.lock().unwrap() = n;
        match store.append(&id, inc(n)) {
            Ok(event) => committed.push((event.sequence, n)),
            Err(StoreError::InjectedFault(_)) => {}
            Err(other) => panic!("unexpected {other}"),
        }
    }
    store.set_fault_hook(None);
    assert_eq!(committed.len(), 40);
    let events = db.open().read_stream(&id).unwrap();
    let stored: Vec<(Sequence, u64)> = events
        .iter()
        .map(|e| (e.sequence, e.payload["by"].as_u64().unwrap()))
        .collect();
    assert_eq!(stored, committed);
    assert_store_valid(&store);
}
