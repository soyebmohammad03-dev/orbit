//! Integrity verification against real, deliberately damaged database files.
//!
//! Damage is applied through a raw SQLite connection with the protective
//! triggers dropped, simulating corruption or out-of-band modification.

mod common;

use common::{TestDb, seed, sid};
use orbit_core::integrity::EventDigestInput;
use orbit_core::{
    Digest, EventId, EventType, IntegrityIssue, SchemaVersion, Sequence, StoreError, StreamId,
    Timestamp,
};
use rusqlite::params;

fn seq(n: u64) -> Sequence {
    Sequence::new(n).unwrap()
}

fn damaged(count: u64) -> (TestDb, StreamId) {
    let db = TestDb::new();
    let id = sid("ledger");
    seed(&db.open(), &id, count);
    db.raw()
        .execute_batch(
            "DROP TRIGGER events_no_update;
             DROP TRIGGER events_no_delete;
             DROP TRIGGER streams_head_only_advances;",
        )
        .unwrap();
    (db, id)
}

#[test]
fn untouched_stream_is_valid() {
    let (db, id) = damaged(10);
    let report = db.open().verify_stream(&id).unwrap();
    assert!(report.is_valid());
    assert_eq!(report.events_checked, 10);
    assert_eq!(report.head.sequence, seq(10));
    assert!(report.issues.is_empty());
}

#[test]
fn modified_payload_is_pinpointed() {
    let (db, id) = damaged(5);
    db.raw()
        .execute(
            "UPDATE events SET payload = '{\"n\":999}' WHERE sequence = 3",
            [],
        )
        .unwrap();
    let report = db.open().verify_stream(&id).unwrap();
    assert!(!report.is_valid());
    assert_eq!(report.stream_id, id);
    assert_eq!(report.issues.len(), 1, "{:?}", report.issues);
    assert!(matches!(
        report.issues[0],
        IntegrityIssue::DigestMismatch { sequence, .. } if sequence == seq(3)
    ));
}

#[test]
fn modified_metadata_type_version_or_timestamp_is_detected() {
    for statement in [
        "UPDATE events SET metadata = '{\"actor\":\"mallory\"}' WHERE sequence = 2",
        "UPDATE events SET event_type = 'counter.decremented' WHERE sequence = 2",
        "UPDATE events SET schema_version = 2 WHERE sequence = 2",
        "UPDATE events SET recorded_at = recorded_at + 1 WHERE sequence = 2",
    ] {
        let (db, id) = damaged(3);
        db.raw().execute(statement, []).unwrap();
        let report = db.open().verify_stream(&id).unwrap();
        assert_eq!(
            report.issues,
            vec![match &report.issues[..] {
                [
                    IntegrityIssue::DigestMismatch {
                        stored, computed, ..
                    },
                ] => {
                    IntegrityIssue::DigestMismatch {
                        sequence: seq(2),
                        stored: *stored,
                        computed: *computed,
                    }
                }
                other => panic!("{statement}: unexpected issues {other:?}"),
            }],
            "{statement}"
        );
    }
}

#[test]
fn replaced_digest_breaks_the_chain() {
    let (db, id) = damaged(4);
    db.raw()
        .execute(
            "UPDATE events SET digest = ?1 WHERE sequence = 2",
            [[0xAB_u8; 32].as_slice()],
        )
        .unwrap();
    let report = db.open().verify_stream(&id).unwrap();
    let kinds: Vec<_> = report
        .issues
        .iter()
        .map(|issue| match issue {
            IntegrityIssue::DigestMismatch { sequence, .. } => ("digest", sequence.get()),
            IntegrityIssue::ChainBroken { sequence, .. } => ("chain", sequence.get()),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(kinds, vec![("digest", 2), ("chain", 3)]);
}

#[test]
fn deleted_middle_event_is_detected() {
    let (db, id) = damaged(5);
    db.raw()
        .execute("DELETE FROM events WHERE sequence = 3", [])
        .unwrap();
    let report = db.open().verify_stream(&id).unwrap();
    assert_eq!(report.events_checked, 4);
    assert!(report.issues.contains(&IntegrityIssue::SequenceGap {
        expected: seq(3),
        found: 4
    }));
    assert!(
        report.issues.iter().any(
            |i| matches!(i, IntegrityIssue::ChainBroken { sequence, .. } if *sequence == seq(4))
        )
    );
}

#[test]
fn truncated_tail_is_detected_through_the_head() {
    let (db, id) = damaged(5);
    db.raw()
        .execute("DELETE FROM events WHERE sequence >= 4", [])
        .unwrap();
    let report = db.open().verify_stream(&id).unwrap();
    assert_eq!(report.issues.len(), 1);
    assert!(matches!(
        report.issues[0],
        IntegrityIssue::HeadMismatch { recorded_sequence, observed_sequence, .. }
            if recorded_sequence == seq(5) && observed_sequence == seq(3)
    ));
}

#[test]
fn malformed_rows_are_reported_not_panicked_on() {
    let (db, id) = damaged(3);
    let raw = db.raw();
    raw.execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    raw.execute(
        "UPDATE events SET payload = '{broken' WHERE sequence = 2",
        [],
    )
    .unwrap();
    raw.execute(
        "UPDATE events SET event_id = X'0102' WHERE sequence = 3",
        [],
    )
    .unwrap();
    drop(raw);

    let store = db.open();
    // Ordinary reads surface a typed error naming the record.
    match store.read_stream(&id).unwrap_err() {
        StoreError::CorruptRecord {
            stream, sequence, ..
        } => {
            assert_eq!(stream, "ledger");
            assert_eq!(sequence, 2);
        }
        other => panic!("unexpected {other}"),
    }
    assert!(matches!(
        store.event(&id, seq(3)),
        Err(StoreError::CorruptRecord { sequence: 3, .. })
    ));
    assert!(store.event(&id, seq(1)).is_ok());

    // Verification reports both rows and keeps going.
    let report = store.verify_stream(&id).unwrap();
    let malformed: Vec<i64> = report
        .issues
        .iter()
        .filter_map(|i| match i {
            IntegrityIssue::MalformedRecord { sequence, .. } => Some(*sequence),
            _ => None,
        })
        .collect();
    assert_eq!(malformed, vec![2, 3]);
}

#[test]
fn issue_reports_serialize_for_future_apis() {
    let (db, id) = damaged(2);
    db.raw()
        .execute("UPDATE events SET payload = '[]' WHERE sequence = 1", [])
        .unwrap();
    let report = db.open().verify_stream(&id).unwrap();
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["issues"][0]["kind"], "digest_mismatch");
    assert_eq!(json["issues"][0]["sequence"], 1);
    assert_eq!(json["total_issues"], 1);
}

/// Documents the limitation stated in STORAGE_DESIGN.md: digests are
/// unkeyed, so someone who can write the file and recompute every digest
/// produces a database that verifies as valid.
#[test]
fn consistent_rewrite_with_recomputed_digests_is_not_detected() {
    let (db, id) = damaged(3);
    let raw = db.raw();
    let rows: Vec<EventColumns> = raw
        .prepare(
            "SELECT sequence, event_id, event_type, schema_version, recorded_at, payload, metadata
             FROM events ORDER BY sequence",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    let mut prev = Digest::GENESIS;
    for (sequence, event_id, event_type, version, recorded_at, payload, metadata) in rows {
        let payload = if sequence == 1 {
            r#"{"n":-1000}"#.to_owned()
        } else {
            payload
        };
        let digest = EventDigestInput {
            stream_id: "ledger",
            sequence: Sequence::new(sequence as u64).unwrap(),
            event_id: EventId::from_uuid(uuid_from(&event_id)).unwrap(),
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
                sequence
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

    let store = db.open();
    assert!(store.verify_stream(&id).unwrap().is_valid());
    assert_eq!(store.event(&id, seq(1)).unwrap().payload["n"], -1000);
}

/// sequence, event_id, event_type, schema_version, recorded_at, payload, metadata
type EventColumns = (i64, Vec<u8>, String, i64, i64, String, String);

fn uuid_from(bytes: &[u8]) -> uuid::Uuid {
    uuid::Uuid::from_slice(bytes).unwrap()
}
