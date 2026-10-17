//! Snapshot persistence, validation, selection and snapshot-assisted reconstruction.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use common::{TestDb, at, counter_stream, inc, seqn, sid, triangle};
use orbit_core::projection::examples::{Counter, CounterState};
use orbit_core::reconstruct::{SnapshotStatus, StartPoint};
use orbit_core::snapshot::{SnapshotCheck, SnapshotProblem};
use orbit_core::{
    Digest, EventStore, NewSnapshot, Projection, ProjectionError, ProjectionKey,
    ReconstructOptions, ReconstructionError, Reconstructor, Sequence, SnapshotId, SnapshotPolicy,
    StoreError, StoredEvent, StreamId, Target, ValidationError,
};
use serde_json::json;

/// Same behaviour as `Counter` but a different version: its snapshots are incompatible.
struct CounterV2;

impl Projection for CounterV2 {
    type State = CounterState;
    fn key(&self) -> ProjectionKey {
        ProjectionKey::new("counter", 2).unwrap()
    }
    fn initial_state(&self) -> CounterState {
        CounterState::default()
    }
    fn apply(
        &self,
        state: CounterState,
        event: &StoredEvent,
    ) -> Result<CounterState, ProjectionError> {
        Counter.apply(state, event)
    }
}

fn prefer() -> ReconstructOptions {
    ReconstructOptions::with_snapshots(SnapshotPolicy::Prefer)
}

fn snapshot_at(r: &Reconstructor<'_, Counter>, id: &StreamId, n: u64) -> SnapshotId {
    r.create_snapshot(id, at(n), prefer())
        .unwrap()
        .saved
        .snapshot
        .id
}

/// Drops the immutability trigger so tests can damage snapshot rows.
fn unlock_snapshots(db: &TestDb) {
    db.raw()
        .execute_batch("DROP TRIGGER snapshots_no_update")
        .unwrap();
}

#[test]
fn create_retrieve_and_list() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 10);
    let counter = Reconstructor::new(&store, Counter);

    let created = counter.create_snapshot(&id, at(4), prefer()).unwrap();
    assert!(created.saved.created);
    let snap = &created.saved.snapshot;
    assert_eq!(snap.stream_id, id);
    assert_eq!(snap.sequence, seqn(4));
    assert_eq!(snap.projection, Counter.key());
    assert_eq!(snap.state, json!({"value": triangle(4)}));
    assert_eq!(snap.event_digest, store.event(&id, seqn(4)).unwrap().digest);
    assert_eq!(created.report.reconstructed, seqn(4));

    assert_eq!(&store.snapshot(snap.id).unwrap(), snap);

    counter
        .create_snapshot(&id, Target::Latest, prefer())
        .unwrap();
    counter.create_snapshot(&id, at(2), prefer()).unwrap();
    let listed: Vec<u64> = store
        .list_snapshots(&id)
        .unwrap()
        .iter()
        .map(|s| s.sequence.get())
        .collect();
    assert_eq!(listed, vec![2, 4, 10]);
    assert!(matches!(
        store.check_snapshot(snap.id).unwrap(),
        SnapshotCheck::Intact { .. }
    ));
}

#[test]
fn snapshots_survive_reopen_and_remain_usable() {
    let db = TestDb::new();
    let id = sid("c");
    let snap_id = {
        let store = db.open();
        counter_stream(&store, &id, 8);
        snapshot_at(&Reconstructor::new(&store, Counter), &id, 6)
    };
    let store = db.open();
    let r = Reconstructor::new(&store, Counter)
        .reconstruct(&id, Target::Latest, prefer())
        .unwrap();
    assert_eq!(r.state.value, triangle(8));
    assert_eq!(
        r.report.start,
        StartPoint::Snapshot {
            snapshot_id: snap_id,
            sequence: seqn(6)
        }
    );
    assert_eq!(r.report.events_applied, 2);
}

#[test]
fn saving_is_idempotent_and_conflicts_are_detected() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 5);
    let counter = Reconstructor::new(&store, Counter);
    let first = counter.create_snapshot(&id, at(5), prefer()).unwrap().saved;
    let again = counter
        .create_snapshot(
            &id,
            at(5),
            ReconstructOptions::with_snapshots(SnapshotPolicy::Ignore),
        )
        .unwrap()
        .saved;
    assert!(!again.created);
    assert_eq!(again.snapshot.id, first.snapshot.id);
    assert_eq!(store.list_snapshots(&id).unwrap().len(), 1);

    let err = store
        .save_snapshot(NewSnapshot {
            stream_id: id.clone(),
            sequence: seqn(5),
            event_digest: first.snapshot.event_digest,
            projection: Counter.key(),
            state: json!({"value": -1}),
        })
        .unwrap_err();
    assert!(
        matches!(&err, StoreError::SnapshotConflict(c) if c.existing == first.snapshot.id),
        "{err}"
    );
    // A different projection version at the same sequence is a separate snapshot.
    Reconstructor::new(&store, CounterV2)
        .create_snapshot(&id, at(5), prefer())
        .unwrap();
    assert_eq!(store.list_snapshots(&id).unwrap().len(), 2);
}

#[test]
fn snapshots_cannot_claim_unapplied_history() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 3);
    let real = store.event(&id, seqn(2)).unwrap().digest;
    let offer = |sequence: Sequence, event_digest: Digest| NewSnapshot {
        stream_id: id.clone(),
        sequence,
        event_digest,
        projection: Counter.key(),
        state: json!({"value": 3}),
    };

    assert!(matches!(
        store.save_snapshot(offer(seqn(2), Digest::from_bytes([9; 32]))),
        Err(StoreError::SnapshotHistoryMismatch { .. })
    ));
    assert!(matches!(
        store.save_snapshot(offer(seqn(4), real)),
        Err(StoreError::SnapshotHistoryMismatch { .. })
    ));
    assert!(matches!(
        store.save_snapshot(offer(Sequence::ZERO, Digest::GENESIS)),
        Err(StoreError::Validation(ValidationError::SnapshotAtZero))
    ));
    assert!(matches!(
        store.save_snapshot(NewSnapshot {
            stream_id: sid("ghost"),
            ..offer(seqn(1), real)
        }),
        Err(StoreError::StreamNotFound(_))
    ));
    assert!(store.list_snapshots(&id).unwrap().is_empty());

    // Snapshotting position 0 through the reconstructor is rejected too.
    let err = Reconstructor::new(&store, Counter)
        .create_snapshot(&id, at(0), prefer())
        .unwrap_err();
    assert!(
        matches!(
            err,
            ReconstructionError::Store(StoreError::Validation(ValidationError::SnapshotAtZero))
        ),
        "{err}"
    );

    // The database enforces the binding even for raw writes.
    let err = db
        .raw()
        .execute(
            "INSERT INTO snapshots VALUES (?1, 'c', 2, 'counter', 1, ?2, '{}', ?2, 0)",
            rusqlite::params![[1u8; 16].as_slice(), [9u8; 32].as_slice()],
        )
        .unwrap_err();
    assert!(err.to_string().contains("existing event"), "{err}");
}

#[test]
fn missing_snapshots_are_explicit() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 2);
    let ghost = SnapshotId::generate();
    assert!(matches!(store.snapshot(ghost), Err(StoreError::SnapshotNotFound(g)) if g == ghost));
    assert!(matches!(
        store.check_snapshot(ghost),
        Err(StoreError::SnapshotNotFound(_))
    ));
    let counter = Reconstructor::new(&store, Counter);
    let err = counter
        .reconstruct(
            &id,
            Target::Latest,
            ReconstructOptions::with_snapshots(SnapshotPolicy::Exact(ghost)),
        )
        .unwrap_err();
    assert!(
        matches!(
            err,
            ReconstructionError::Store(StoreError::SnapshotNotFound(_))
        ),
        "{err}"
    );
    assert!(matches!(
        store.list_snapshots(&sid("nope")),
        Err(StoreError::StreamNotFound(_))
    ));
}

#[test]
fn validation_distinguishes_every_failure_mode() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 6);
    let counter = Reconstructor::new(&store, Counter);
    let usable = snapshot_at(&counter, &id, 1);
    let tampered = snapshot_at(&counter, &id, 2);
    let undecodable_json = snapshot_at(&counter, &id, 3);
    let history = snapshot_at(&counter, &id, 4);
    let incompatible = Reconstructor::new(&store, CounterV2)
        .create_snapshot(&id, at(5), prefer())
        .unwrap()
        .saved
        .snapshot
        .id;
    // Valid digest and history, but a state that is not a `CounterState`.
    let wrong_shape = store
        .save_snapshot(NewSnapshot {
            stream_id: id.clone(),
            sequence: seqn(6),
            event_digest: store.event(&id, seqn(6)).unwrap().digest,
            projection: Counter.key(),
            state: json!({"value": "six"}),
        })
        .unwrap()
        .snapshot
        .id;

    unlock_snapshots(&db);
    let raw = db.raw();
    raw.execute(
        "UPDATE snapshots SET state = '{\"value\":999}' WHERE snapshot_id = ?1",
        [tampered.as_bytes().as_slice()],
    )
    .unwrap();
    raw.execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    raw.execute(
        "UPDATE snapshots SET state = '{not json' WHERE snapshot_id = ?1",
        [undecodable_json.as_bytes().as_slice()],
    )
    .unwrap();
    raw.execute_batch(
        "DROP TRIGGER events_no_update;
         UPDATE events SET digest = zeroblob(32) WHERE sequence = 4;",
    )
    .unwrap();
    drop(raw);

    let status = |snapshot: SnapshotId| counter.check_snapshot(snapshot).unwrap();
    assert!(matches!(status(usable), SnapshotStatus::Usable { .. }));
    assert!(matches!(
        status(tampered),
        SnapshotStatus::Unusable {
            problem: SnapshotProblem::IntegrityMismatch { .. },
            sequence: 2
        }
    ));
    assert!(matches!(
        status(undecodable_json),
        SnapshotStatus::Unusable {
            problem: SnapshotProblem::Malformed { .. },
            ..
        }
    ));
    assert!(matches!(
        status(history),
        SnapshotStatus::Unusable {
            problem: SnapshotProblem::HistoryMismatch {
                actual: Some(_),
                ..
            },
            ..
        }
    ));
    assert!(matches!(
        status(incompatible),
        SnapshotStatus::Unusable {
            problem: SnapshotProblem::Incompatible { .. },
            ..
        }
    ));
    assert!(matches!(
        status(wrong_shape),
        SnapshotStatus::Unusable {
            problem: SnapshotProblem::Undecodable { .. },
            ..
        }
    ));
    // Listing a malformed row is an explicit error, not a panic or a silent skip.
    assert!(matches!(
        store.list_snapshots(&id),
        Err(StoreError::CorruptSnapshot { .. })
    ));
}

#[test]
fn selection_uses_newest_snapshot_at_or_before_target_with_irregular_intervals() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 25);
    let counter = Reconstructor::new(&store, Counter);
    let ids: Vec<(u64, SnapshotId)> = [3, 7, 8, 20]
        .into_iter()
        .map(|n| (n, snapshot_at(&counter, &id, n)))
        .collect();
    let id_at = |n: u64| ids.iter().find(|(s, _)| *s == n).unwrap().1;

    for (target, expected_start) in [
        (2, None),
        (3, Some(3)),
        (6, Some(3)),
        (7, Some(7)),
        (8, Some(8)),
        (19, Some(8)),
        (20, Some(20)),
        (25, Some(20)),
    ] {
        let r = counter.reconstruct(&id, at(target), prefer()).unwrap();
        assert_eq!(r.state.value, triangle(target), "target {target}");
        match expected_start {
            None => assert_eq!(r.report.start, StartPoint::Initial),
            Some(n) => {
                assert_eq!(
                    r.report.start,
                    StartPoint::Snapshot {
                        snapshot_id: id_at(n),
                        sequence: seqn(n)
                    },
                    "target {target}"
                );
                assert_eq!(r.report.events_applied, target - n);
            }
        }
        assert!(r.report.snapshot_warnings.is_empty());
    }
}

#[test]
fn snapshot_after_target_is_never_used() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 10);
    let counter = Reconstructor::new(&store, Counter);
    let late = snapshot_at(&counter, &id, 10);
    let r = counter.reconstruct(&id, at(5), prefer()).unwrap();
    assert_eq!(r.report.start, StartPoint::Initial);
    assert_eq!(r.state.value, triangle(5));
    let err = counter
        .reconstruct(
            &id,
            at(5),
            ReconstructOptions::with_snapshots(SnapshotPolicy::Exact(late)),
        )
        .unwrap_err();
    assert!(
        matches!(
            &err,
            ReconstructionError::Snapshot {
                problem: SnapshotProblem::BeyondTarget { .. },
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn exact_snapshot_policy() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    let other = sid("other");
    counter_stream(&store, &id, 10);
    counter_stream(&store, &other, 3);
    let counter = Reconstructor::new(&store, Counter);
    let older = snapshot_at(&counter, &id, 4);
    snapshot_at(&counter, &id, 8);
    let foreign = snapshot_at(&counter, &other, 2);

    let r = counter
        .reconstruct(
            &id,
            at(9),
            ReconstructOptions::with_snapshots(SnapshotPolicy::Exact(older)),
        )
        .unwrap();
    assert_eq!(
        r.report.start,
        StartPoint::Snapshot {
            snapshot_id: older,
            sequence: seqn(4)
        }
    );
    assert_eq!(r.report.events_applied, 5);
    assert_eq!(r.state.value, triangle(9));

    // Exact is never silently ignored, not even for target 0.
    let err = counter
        .reconstruct(
            &id,
            at(0),
            ReconstructOptions::with_snapshots(SnapshotPolicy::Exact(older)),
        )
        .unwrap_err();
    assert!(
        matches!(
            &err,
            ReconstructionError::Snapshot {
                problem: SnapshotProblem::BeyondTarget { .. },
                ..
            }
        ),
        "{err}"
    );

    let err = counter
        .reconstruct(
            &id,
            at(9),
            ReconstructOptions::with_snapshots(SnapshotPolicy::Exact(foreign)),
        )
        .unwrap_err();
    assert!(
        matches!(
            &err,
            ReconstructionError::Snapshot {
                problem: SnapshotProblem::WrongStream { .. },
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn corrupt_snapshots_are_skipped_observably_or_rejected_strictly() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 12);
    let counter = Reconstructor::new(&store, Counter);
    let good = snapshot_at(&counter, &id, 5);
    let bad = snapshot_at(&counter, &id, 10);
    unlock_snapshots(&db);
    db.raw()
        .execute(
            "UPDATE snapshots SET state = '{\"value\":0}' WHERE snapshot_id = ?1",
            [bad.as_bytes().as_slice()],
        )
        .unwrap();

    // Prefer: falls back to the older valid snapshot, and says so.
    let r = counter.reconstruct(&id, Target::Latest, prefer()).unwrap();
    assert_eq!(r.state.value, triangle(12));
    assert_eq!(
        r.report.start,
        StartPoint::Snapshot {
            snapshot_id: good,
            sequence: seqn(5)
        }
    );
    assert_eq!(r.report.snapshot_warnings.len(), 1);
    assert_eq!(r.report.snapshot_warnings[0].snapshot, bad.to_string());
    assert!(matches!(
        r.report.snapshot_warnings[0].problem,
        SnapshotProblem::IntegrityMismatch { .. }
    ));

    // Strict: refuses.
    let err = counter
        .reconstruct(
            &id,
            Target::Latest,
            ReconstructOptions::with_snapshots(SnapshotPolicy::Strict),
        )
        .unwrap_err();
    assert!(
        matches!(err, ReconstructionError::Snapshot { sequence: 10, .. }),
        "{err}"
    );

    // Exact on the corrupt one: refuses.
    assert!(
        counter
            .reconstruct(
                &id,
                Target::Latest,
                ReconstructOptions::with_snapshots(SnapshotPolicy::Exact(bad))
            )
            .is_err()
    );

    // Every candidate corrupt: full replay, with every rejection reported.
    db.raw()
        .execute(
            "UPDATE snapshots SET state = '{\"value\":1}' WHERE snapshot_id = ?1",
            [good.as_bytes().as_slice()],
        )
        .unwrap();
    let r = counter.reconstruct(&id, Target::Latest, prefer()).unwrap();
    assert_eq!(r.state.value, triangle(12));
    assert_eq!(r.report.start, StartPoint::Initial);
    let rejected: Vec<i64> = r
        .report
        .snapshot_warnings
        .iter()
        .map(|w| w.sequence)
        .collect();
    assert_eq!(rejected, vec![10, 5]);
}

#[test]
fn many_unusable_candidates_are_paged_through() {
    // More rejected candidates than one selection page (8).
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 30);
    let counter = Reconstructor::new(&store, Counter);
    let good = snapshot_at(&counter, &id, 2);
    for n in 10..=29 {
        snapshot_at(&counter, &id, n);
    }
    unlock_snapshots(&db);
    db.raw()
        .execute(
            "UPDATE snapshots SET state = '{\"value\":0}' WHERE sequence >= 10",
            [],
        )
        .unwrap();
    let r = counter.reconstruct(&id, Target::Latest, prefer()).unwrap();
    assert_eq!(r.state.value, triangle(30));
    assert_eq!(
        r.report.start,
        StartPoint::Snapshot {
            snapshot_id: good,
            sequence: seqn(2)
        }
    );
    assert_eq!(r.report.snapshot_warnings.len(), 20);
}

#[test]
fn incompatible_versions_are_never_selected() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 6);
    Reconstructor::new(&store, CounterV2)
        .create_snapshot(&id, at(5), prefer())
        .unwrap();
    let r = Reconstructor::new(&store, Counter)
        .reconstruct(&id, Target::Latest, prefer())
        .unwrap();
    assert_eq!(r.report.start, StartPoint::Initial);
    assert!(r.report.snapshot_warnings.is_empty());
}

#[test]
fn full_replay_and_snapshot_reconstruction_are_equivalent() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 40);
    let counter = Reconstructor::new(&store, Counter);
    for n in [10, 25, 33] {
        snapshot_at(&counter, &id, n);
    }
    for target in [at(0), at(9), at(10), at(26), at(40), Target::Latest] {
        let report = counter
            .compare_with_full_replay(&id, target, SnapshotPolicy::Prefer)
            .unwrap();
        assert!(report.equivalent, "{target:?}: {:?}", report.differences);
        assert_eq!(report.full.start, StartPoint::Initial);
        assert_eq!(report.full.reconstructed, report.assisted.reconstructed);
    }
    let report = counter
        .compare_with_full_replay(&id, Target::Latest, SnapshotPolicy::Prefer)
        .unwrap();
    assert!(
        matches!(report.assisted.start, StartPoint::Snapshot { sequence, .. } if sequence == seqn(33))
    );
    assert_eq!(report.assisted.events_applied, 7);
    assert_eq!(report.full.events_applied, 40);
}

#[test]
fn equivalence_check_exposes_a_wrong_snapshot() {
    // A snapshot whose state is wrong but whose digest is consistent (as from a
    // buggy, non-deterministic projection) passes integrity checks; comparing
    // against full replay is how it gets caught.
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 4);
    store
        .save_snapshot(NewSnapshot {
            stream_id: id.clone(),
            sequence: seqn(3),
            event_digest: store.event(&id, seqn(3)).unwrap().digest,
            projection: Counter.key(),
            state: json!({"value": 1000}),
        })
        .unwrap();
    let report = Reconstructor::new(&store, Counter)
        .compare_with_full_replay(&id, Target::Latest, SnapshotPolicy::Prefer)
        .unwrap();
    assert!(!report.equivalent);
    assert_eq!(report.differences.len(), 1);
    assert_eq!(report.differences[0].path, "/value");
}

#[test]
fn appends_around_snapshot_creation() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 5);
    let counter = Reconstructor::new(&store, Counter);
    let created = counter
        .create_snapshot(&id, Target::Latest, prefer())
        .unwrap();
    assert_eq!(created.saved.snapshot.sequence, seqn(5));
    for _ in 0..3 {
        store.append(&id, inc(1)).unwrap();
    }
    // The snapshot still describes exactly position 5 and stays usable.
    assert!(matches!(
        store.check_snapshot(created.saved.snapshot.id).unwrap(),
        SnapshotCheck::Intact { .. }
    ));
    let r = counter.reconstruct(&id, Target::Latest, prefer()).unwrap();
    assert_eq!(r.report.reconstructed, seqn(8));
    assert_eq!(r.report.events_applied, 3);
    assert_eq!(r.state.value, triangle(5) + 3);
}

#[test]
fn snapshots_taken_while_another_connection_appends_are_consistent() {
    let db = TestDb::new();
    let id = sid("busy");
    counter_stream(&db.open(), &id, 1);

    let done = Arc::new(AtomicBool::new(false));
    let writer = {
        let store = EventStore::open(db.config()).unwrap();
        let id = id.clone();
        let done = Arc::clone(&done);
        thread::spawn(move || {
            for _ in 0..300 {
                store.append(&id, inc(1)).unwrap();
            }
            done.store(true, Ordering::SeqCst);
        })
    };

    let store = db.open();
    let counter = Reconstructor::new(&store, Counter);
    let mut taken = Vec::new();
    loop {
        let finished = done.load(Ordering::SeqCst);
        let created = counter
            .create_snapshot(&id, Target::Latest, prefer())
            .unwrap();
        taken.push(created.saved.snapshot);
        if finished {
            break;
        }
    }
    writer.join().unwrap();

    assert_eq!(store.head(&id).unwrap().sequence, seqn(301));
    for snapshot in &taken {
        // Each snapshot's state is exactly the full-replay state at its sequence.
        let full = counter
            .reconstruct(
                &id,
                Target::At(snapshot.sequence),
                ReconstructOptions::with_snapshots(SnapshotPolicy::Ignore),
            )
            .unwrap();
        assert_eq!(snapshot.state, serde_json::to_value(full.state).unwrap());
        assert!(matches!(
            store.check_snapshot(snapshot.id).unwrap(),
            SnapshotCheck::Intact { .. }
        ));
    }
    let report = counter
        .compare_with_full_replay(&id, Target::Latest, SnapshotPolicy::Strict)
        .unwrap();
    assert!(report.equivalent);
}
