//! Historical reconstruction against real SQLite files.

mod common;

use common::{TestDb, at, counter_stream, dec, inc, reset, seed_with, seqn, sid, triangle, ty};
use orbit_core::projection::ProjectionErrorKind;
use orbit_core::projection::examples::Counter;
use orbit_core::reconstruct::StartPoint;
use orbit_core::{
    AppendRequest, Digest, Metadata, ReconstructOptions, ReconstructionError, Reconstructor,
    Sequence, StoreError, Target,
};
use serde_json::json;

fn ignore_snapshots() -> ReconstructOptions {
    ReconstructOptions::with_snapshots(orbit_core::SnapshotPolicy::Ignore)
}

#[test]
fn empty_stream_reconstructs_to_initial_state() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("empty");
    store.create_stream(&id, Metadata::new()).unwrap();
    let counter = Reconstructor::new(&store, Counter);

    for target in [Target::Latest, Target::At(Sequence::ZERO)] {
        let r = counter
            .reconstruct(&id, target, ReconstructOptions::default())
            .unwrap();
        assert_eq!(r.state.value, 0);
        assert_eq!(r.report.reconstructed, Sequence::ZERO);
        assert_eq!(r.report.reconstructed_digest, Digest::GENESIS);
        assert_eq!(r.report.events_applied, 0);
        assert_eq!(r.report.start, StartPoint::Initial);
        assert_eq!(r.report.head_at_start, Sequence::ZERO);
    }
    let err = counter
        .reconstruct(&id, at(1), ReconstructOptions::default())
        .unwrap_err();
    assert!(
        matches!(err, ReconstructionError::TargetBeyondHead { target, head, .. }
            if target == Sequence::FIRST && head == Sequence::ZERO),
        "{err}"
    );
}

#[test]
fn latest_and_historical_states() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 20);
    let counter = Reconstructor::new(&store, Counter);

    let latest = counter
        .reconstruct(&id, Target::Latest, ReconstructOptions::default())
        .unwrap();
    assert_eq!(latest.state.value, triangle(20));
    assert_eq!(latest.report.reconstructed, seqn(20));
    assert_eq!(latest.report.head_at_start, seqn(20));
    assert_eq!(latest.report.events_applied, 20);
    assert_eq!(
        latest.report.reconstructed_digest,
        store.head(&id).unwrap().digest
    );
    assert_eq!(latest.report.requested, Target::Latest);

    for n in [0, 1, 2, 7, 19, 20] {
        let r = counter
            .reconstruct(&id, at(n), ReconstructOptions::default())
            .unwrap();
        assert_eq!(r.state.value, triangle(n), "at {n}");
        assert_eq!(r.report.reconstructed, seqn(n));
        assert_eq!(r.report.events_applied, n);
        let expected_digest = if n == 0 {
            Digest::GENESIS
        } else {
            store.event(&id, seqn(n)).unwrap().digest
        };
        assert_eq!(r.report.reconstructed_digest, expected_digest);
    }

    let err = counter
        .reconstruct(&id, at(21), ReconstructOptions::default())
        .unwrap_err();
    assert!(
        matches!(err, ReconstructionError::TargetBeyondHead { .. }),
        "{err}"
    );
}

#[test]
fn missing_stream_is_an_error() {
    let db = TestDb::new();
    let store = db.open();
    let counter = Reconstructor::new(&store, Counter);
    let err = counter
        .reconstruct(&sid("ghost"), Target::Latest, ReconstructOptions::default())
        .unwrap_err();
    assert!(
        matches!(
            err,
            ReconstructionError::Store(StoreError::StreamNotFound(_))
        ),
        "{err}"
    );
}

#[test]
fn events_are_applied_in_sequence_order() {
    // Order-sensitive history: a reset in the middle makes any reordering visible.
    let db = TestDb::new();
    let store = db.open();
    let id = sid("ordered");
    seed_with(&store, &id, vec![inc(5), inc(5), reset(), inc(2), dec(7)]);
    let counter = Reconstructor::new(&store, Counter);
    let r = counter
        .reconstruct(
            &id,
            Target::Latest,
            ignore_snapshots().recording_events(true),
        )
        .unwrap();
    assert_eq!(r.state.value, -5);
    let order: Vec<u64> = r.report.applied.iter().map(|e| e.sequence.get()).collect();
    assert_eq!(order, vec![1, 2, 3, 4, 5]);
    assert_eq!(r.report.applied[2].event_type.as_str(), "counter.reset");
    let at_three = counter.reconstruct(&id, at(3), ignore_snapshots()).unwrap();
    assert_eq!(at_three.state.value, 0);
    assert!(
        at_three.report.applied.is_empty(),
        "recording is off by default"
    );
}

#[test]
fn projection_failure_is_reported_with_context_and_no_partial_state() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("bad");
    seed_with(
        &store,
        &id,
        vec![
            inc(1),
            inc(2),
            AppendRequest::new(ty("counter.multiplied"), json!({"by": 3})),
            inc(4),
        ],
    );
    let bad = store.event(&id, seqn(3)).unwrap();
    let counter = Reconstructor::new(&store, Counter);

    for target in [Target::Latest, at(3), at(4)] {
        match counter.reconstruct(&id, target, ReconstructOptions::default()) {
            Err(ReconstructionError::Projection(failure)) => {
                assert_eq!(failure.stream, id);
                assert_eq!(failure.projection.to_string(), "counter@v1");
                assert_eq!(failure.sequence, seqn(3));
                assert_eq!(failure.event_id, bad.event_id);
                assert_eq!(failure.event_type.as_str(), "counter.multiplied");
                assert_eq!(failure.source.kind, ProjectionErrorKind::UnsupportedEvent);
                assert!(failure.to_string().contains("sequence 3"), "{failure}");
            }
            other => panic!("expected a projection error, got {other:?}"),
        }
    }
    // History before the bad event is still reconstructible.
    assert_eq!(
        counter
            .reconstruct(&id, at(2), ReconstructOptions::default())
            .unwrap()
            .state
            .value,
        3
    );
    // Snapshot creation cannot paper over it either.
    assert!(
        counter
            .create_snapshot(&id, Target::Latest, ReconstructOptions::default())
            .is_err()
    );
    assert!(store.list_snapshots(&id).unwrap().is_empty());
}

#[test]
fn invalid_payload_is_a_projection_error() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("payload");
    seed_with(
        &store,
        &id,
        vec![AppendRequest::new(
            ty("counter.incremented"),
            json!({"by": "one"}),
        )],
    );
    let err = Reconstructor::new(&store, Counter)
        .reconstruct(&id, Target::Latest, ReconstructOptions::default())
        .unwrap_err();
    assert!(
        matches!(&err, ReconstructionError::Projection(f) if f.source.kind == ProjectionErrorKind::InvalidPayload),
        "{err}"
    );
}

#[test]
fn reconstruction_spans_many_pages() {
    // More events than one read page (512), to exercise bounded paging.
    let db = TestDb::new();
    let store = db.open();
    let id = sid("long");
    counter_stream(&store, &id, 1_300);
    let counter = Reconstructor::new(&store, Counter);
    for n in [511, 512, 513, 1_024, 1_300] {
        let r = counter.reconstruct(&id, at(n), ignore_snapshots()).unwrap();
        assert_eq!(r.state.value, triangle(n), "at {n}");
        assert_eq!(r.report.events_applied, n);
    }
}

#[test]
fn broken_history_is_detected_during_the_fold() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("broken");
    counter_stream(&store, &id, 6);
    db.raw()
        .execute_batch(
            "DROP TRIGGER events_no_update;
             DROP TRIGGER events_no_delete;
             UPDATE events SET prev_digest = zeroblob(32) WHERE sequence = 4;",
        )
        .unwrap();
    let counter = Reconstructor::new(&store, Counter);
    let err = counter
        .reconstruct(&id, Target::Latest, ignore_snapshots())
        .unwrap_err();
    assert!(
        matches!(&err, ReconstructionError::HistoryInconsistent { sequence, .. } if *sequence == seqn(4)),
        "{err}"
    );
    // Positions before the damage are unaffected.
    assert_eq!(
        counter
            .reconstruct(&id, at(3), ignore_snapshots())
            .unwrap()
            .state
            .value,
        6
    );

    db.raw()
        .execute("DELETE FROM events WHERE sequence = 2", [])
        .unwrap();
    let err = counter
        .reconstruct(&id, at(3), ignore_snapshots())
        .unwrap_err();
    assert!(
        matches!(&err, ReconstructionError::HistoryInconsistent { sequence, .. } if *sequence == seqn(2)),
        "{err}"
    );
}

#[test]
fn latest_is_resolved_once_at_the_start() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("moving");
    counter_stream(&store, &id, 3);
    let counter = Reconstructor::new(&store, Counter);
    let before = counter
        .reconstruct(&id, Target::Latest, ReconstructOptions::default())
        .unwrap();
    store.append(&id, inc(100)).unwrap();
    let after = counter
        .reconstruct(&id, Target::Latest, ReconstructOptions::default())
        .unwrap();
    assert_eq!(before.report.reconstructed, seqn(3));
    assert_eq!(after.report.reconstructed, seqn(4));
    assert_eq!(after.state.value, before.state.value + 100);
    // An explicit historical target ignores later appends entirely.
    let pinned = counter
        .reconstruct(&id, at(3), ReconstructOptions::default())
        .unwrap();
    assert_eq!(pinned.state, before.state);
}

#[test]
fn reports_serialize_for_future_apis() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("s");
    counter_stream(&store, &id, 2);
    let r = Reconstructor::new(&store, Counter)
        .reconstruct(&id, Target::Latest, ReconstructOptions::default())
        .unwrap();
    let json = serde_json::to_value(&r.report).unwrap();
    assert_eq!(json["stream_id"], "s");
    assert_eq!(json["requested"], json!({"kind": "latest"}));
    assert_eq!(json["reconstructed"], 2);
    assert_eq!(json["start"], json!({"kind": "initial"}));
    assert_eq!(json["events_applied"], 2);
    assert_eq!(json["projection"], json!({"id": "counter", "version": 1}));
}
