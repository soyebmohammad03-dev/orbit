//! Phase 3 integrity diagnostics: categories, affected events, genesis,
//! reordering, trusted checkpoints, and store-wide verification.

mod common;

use common::{TestDb, counter_stream, seqn, sid};
use orbit_core::integrity::EventDigestInput;
use orbit_core::projection::examples::Counter;
use orbit_core::replay::{ReplayLab, ReplayStart};
use orbit_core::snapshot::SnapshotProblem;
use orbit_core::{
    Digest, EventId, EventType, IntegrityCategory, IntegrityIssue, ReconstructOptions,
    Reconstructor, SchemaVersion, Sequence, StoreError, Target, Timestamp,
};
use rusqlite::params;

/// A seeded stream with the protective triggers removed so tests can damage it.
fn damaged(count: u64) -> TestDb {
    let db = TestDb::new();
    counter_stream(&db.open(), &sid("ledger"), count);
    db.raw()
        .execute_batch(
            "DROP TRIGGER events_no_update;
             DROP TRIGGER events_no_delete;
             DROP TRIGGER streams_head_only_advances;
             DROP TRIGGER snapshots_no_update;",
        )
        .unwrap();
    db
}

#[test]
fn reports_name_the_category_and_the_affected_event() {
    let db = damaged(5);
    let target = db.open().event(&sid("ledger"), seqn(3)).unwrap();
    db.raw()
        .execute(
            "UPDATE events SET payload = '{\"by\":42}' WHERE sequence = 3",
            [],
        )
        .unwrap();
    let report = db.open().verify_stream(&sid("ledger")).unwrap();
    assert_eq!(report.categories(), vec![IntegrityCategory::ModifiedEvent]);
    assert_eq!(report.issues[0].sequence(), Some(3));
    assert_eq!(report.affected_events.len(), 1);
    assert_eq!(report.affected_events[0].sequence, seqn(3));
    assert_eq!(report.affected_events[0].event_id, target.event_id);
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json["affected_events"][0]["event_id"],
        target.event_id.to_string()
    );
}

#[test]
fn invalid_genesis_link_is_its_own_category() {
    let db = damaged(3);
    db.raw()
        .execute(
            "UPDATE events SET prev_digest = ?1 WHERE sequence = 1",
            [[7u8; 32].as_slice()],
        )
        .unwrap();
    let report = db.open().verify_stream(&sid("ledger")).unwrap();
    assert!(report.issues.contains(&IntegrityIssue::InvalidGenesis {
        stored_prev: Digest::from_bytes([7; 32])
    }));
    assert!(
        report
            .categories()
            .contains(&IntegrityCategory::InvalidGenesis)
    );
    assert!(
        !report
            .categories()
            .contains(&IntegrityCategory::BrokenChain)
    );
}

#[test]
fn swapped_events_are_detected() {
    let db = damaged(4);
    // Swap the sequence numbers of events 2 and 3 (via a temporary value).
    db.raw()
        .execute_batch(
            "UPDATE events SET sequence = 100 WHERE sequence = 2;
             UPDATE events SET sequence = 2 WHERE sequence = 3;
             UPDATE events SET sequence = 3 WHERE sequence = 100;",
        )
        .unwrap();
    let report = db.open().verify_stream(&sid("ledger")).unwrap();
    let categories = report.categories();
    assert!(
        categories.contains(&IntegrityCategory::ModifiedEvent),
        "{categories:?}"
    );
    assert!(
        categories.contains(&IntegrityCategory::BrokenChain),
        "{categories:?}"
    );
    let modified: Vec<i64> = report
        .issues
        .iter()
        .filter(|i| i.category() == IntegrityCategory::ModifiedEvent)
        .filter_map(IntegrityIssue::sequence)
        .collect();
    assert_eq!(modified, vec![2, 3]);
}

#[test]
fn inconsistent_head_of_an_empty_stream_is_detected() {
    let db = TestDb::new();
    let store = db.open();
    store
        .create_stream(&sid("empty"), orbit_core::Metadata::new())
        .unwrap();
    let raw = db.raw();
    raw.execute_batch("DROP TRIGGER streams_head_only_advances")
        .unwrap();
    raw.execute(
        "UPDATE streams SET head_digest = ?1",
        [[1u8; 32].as_slice()],
    )
    .unwrap();
    let report = store.verify_stream(&sid("empty")).unwrap();
    assert_eq!(
        report.categories(),
        vec![IntegrityCategory::HeadInconsistency]
    );
}

/// The Phase 1 limitation (unkeyed digests) and the Phase 3 mitigation: a
/// consistent rewrite passes plain verification but fails against a
/// checkpoint taken earlier and kept outside the database.
#[test]
fn checkpoints_detect_a_consistently_recomputed_rewrite() {
    let db = damaged(3);
    let store = db.open();
    let checkpoint = store.checkpoint(&sid("ledger")).unwrap();
    assert_eq!(checkpoint.sequence, seqn(3));
    assert!(
        store
            .verify_stream_against(&sid("ledger"), &[checkpoint])
            .unwrap()
            .is_valid()
    );

    // Rewrite event 1 and recompute every digest and the head consistently.
    let raw = db.raw();
    let mut prev = Digest::GENESIS;
    for n in 1..=3i64 {
        let (event_id, event_type, version, recorded_at, payload, metadata): (Vec<u8>, String, i64, i64, String, String) = raw
            .query_row(
                "SELECT event_id, event_type, schema_version, recorded_at, payload, metadata FROM events WHERE sequence = ?1",
                [n],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        let payload = if n == 1 {
            r#"{"by":1000}"#.to_owned()
        } else {
            payload
        };
        let digest = EventDigestInput {
            stream_id: "ledger",
            sequence: Sequence::new(n as u64).unwrap(),
            event_id: EventId::from_uuid(uuid::Uuid::from_slice(&event_id).unwrap()).unwrap(),
            event_type: EventType::new(event_type.as_str()).unwrap().as_str(),
            schema_version: SchemaVersion::new(version as u32).unwrap(),
            recorded_at: Timestamp::from_unix_micros(recorded_at).unwrap(),
            payload: &payload,
            metadata: &metadata,
            prev_digest: prev,
        }
        .compute();
        raw.execute(
            "UPDATE events SET payload = ?1, prev_digest = ?2, digest = ?3 WHERE sequence = ?4",
            params![
                payload,
                prev.as_bytes().as_slice(),
                digest.as_bytes().as_slice(),
                n
            ],
        )
        .unwrap();
        prev = digest;
    }
    raw.execute(
        "UPDATE streams SET head_digest = ?1",
        [prev.as_bytes().as_slice()],
    )
    .unwrap();

    assert!(
        store.verify_stream(&sid("ledger")).unwrap().is_valid(),
        "self-consistent rewrite"
    );
    let report = store
        .verify_stream_against(&sid("ledger"), &[checkpoint])
        .unwrap();
    assert_eq!(
        report.categories(),
        vec![IntegrityCategory::CheckpointMismatch]
    );
    assert!(matches!(
        report.issues[0],
        IntegrityIssue::CheckpointMismatch { sequence, expected, actual: Some(_) }
            if sequence == seqn(3) && expected == checkpoint.digest
    ));
}

#[test]
fn checkpoint_beyond_truncated_history_is_reported() {
    let db = damaged(4);
    let store = db.open();
    let checkpoint = store.checkpoint(&sid("ledger")).unwrap();
    db.raw()
        .execute_batch(
            "DELETE FROM events WHERE sequence = 4;
             UPDATE streams SET head_sequence = 3, head_digest = (SELECT digest FROM events WHERE sequence = 3);",
        )
        .unwrap();
    assert!(
        store.verify_stream(&sid("ledger")).unwrap().is_valid(),
        "truncation with a rewritten head"
    );
    let report = store
        .verify_stream_against(&sid("ledger"), &[checkpoint])
        .unwrap();
    assert!(matches!(
        report.issues[..],
        [IntegrityIssue::CheckpointMismatch { actual: None, .. }]
    ));
    assert!(matches!(
        store.checkpoint(&sid("ghost")),
        Err(StoreError::StreamNotFound(_))
    ));
}

#[test]
fn verify_store_covers_sqlite_safeguards_snapshots_and_replay_runs() {
    let db = TestDb::new();
    let store = db.open();
    counter_stream(&store, &sid("a"), 6);
    counter_stream(&store, &sid("b"), 2);
    let snap = Reconstructor::new(&store, Counter)
        .create_snapshot(
            &sid("a"),
            Target::At(seqn(4)),
            ReconstructOptions::default(),
        )
        .unwrap()
        .saved
        .snapshot;
    let run = ReplayLab::new(&store, Counter)
        .record(&sid("a"), ReplayStart::Initial, Target::Latest)
        .unwrap();

    let report = store.verify_store().unwrap();
    assert!(report.is_valid(), "{report:#?}");
    assert_eq!(report.schema_version, 3);
    assert_eq!(report.sqlite_integrity, vec!["ok".to_owned()]);
    assert_eq!(report.streams.len(), 2);
    assert_eq!(
        (report.snapshots_checked, report.replay_runs_checked),
        (1, 1)
    );

    // Damage one of everything.
    let raw = db.raw();
    raw.execute_batch(
        "DROP TRIGGER events_no_update;
         DROP TRIGGER snapshots_no_update;
         DROP TRIGGER replay_runs_no_update;
         UPDATE events SET payload = '{\"by\":0}' WHERE stream_id = 'b' AND sequence = 1;",
    )
    .unwrap();
    raw.execute(
        "UPDATE snapshots SET state = '{\"value\":1}' WHERE snapshot_id = ?1",
        [snap.id.as_bytes().as_slice()],
    )
    .unwrap();
    raw.execute(
        "UPDATE replay_runs SET expected_state = '{\"value\":1}' WHERE run_id = ?1",
        [run.id.as_bytes().as_slice()],
    )
    .unwrap();
    drop(raw);

    let report = store.verify_store().unwrap();
    assert!(!report.is_valid());
    assert_eq!(
        report.missing_safeguards,
        vec![
            "events_no_update",
            "replay_runs_no_update",
            "snapshots_no_update"
        ]
    );
    let bad: Vec<&str> = report
        .invalid_streams()
        .map(|r| r.stream_id.as_str())
        .collect();
    assert_eq!(bad, vec!["b"]);
    assert_eq!(report.snapshot_issues.len(), 1);
    assert!(matches!(
        report.snapshot_issues[0].problem,
        SnapshotProblem::IntegrityMismatch { .. }
    ));
    assert_eq!(report.replay_run_issues.len(), 1);
    assert!(
        report.replay_run_issues[0]
            .problem
            .contains("record digest")
    );
    assert_eq!(
        report.sqlite_integrity,
        vec!["ok".to_owned()],
        "the file structure itself is still sound"
    );
}

#[test]
fn verify_store_reports_foreign_key_violations() {
    let db = TestDb::new();
    let store = db.open();
    counter_stream(&store, &sid("a"), 1);
    db.raw()
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TRIGGER events_insert_extends_head;
             INSERT INTO events SELECT 'ghost', sequence, randomblob(16), event_type, schema_version,
                 recorded_at, payload, metadata, prev_digest, digest FROM events;",
        )
        .unwrap();
    let report = store.verify_store().unwrap();
    assert_eq!(report.foreign_key_violations, 1);
    assert!(!report.is_valid());
}
