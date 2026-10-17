//! Appending, reading and durability across reopen.

mod common;

use common::{TestDb, meta, req, seed, sid, ty};
use orbit_core::{
    AppendRequest, Digest, EventId, EventStore, Limits, Metadata, SchemaVersion, Sequence,
    StoreConfig, StoreError, ValidationError,
};
use serde_json::json;

fn seq(n: u64) -> Sequence {
    Sequence::new(n).unwrap()
}

#[test]
fn append_assigns_dense_sequences_and_chains_digests() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("s");
    store.create_stream(&id, Metadata::new()).unwrap();

    let first = store.append(&id, req(1)).unwrap();
    let second = store.append(&id, req(2)).unwrap();
    let third = store.append(&id, req(3)).unwrap();

    assert_eq!(first.sequence, Sequence::FIRST);
    assert_eq!(second.sequence, seq(2));
    assert_eq!(third.sequence, seq(3));
    assert_eq!(first.prev_digest, Digest::GENESIS);
    assert_eq!(second.prev_digest, first.digest);
    assert_eq!(third.prev_digest, second.digest);
    assert_ne!(first.event_id, second.event_id);

    let head = store.head(&id).unwrap();
    assert_eq!(head.sequence, seq(3));
    assert_eq!(head.digest, third.digest);
}

#[test]
fn append_returns_exactly_what_is_read_back() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("s");
    store.create_stream(&id, Metadata::new()).unwrap();
    let request = AppendRequest::new(
        ty("order.placed"),
        json!({"items": [{"sku": "A-1", "qty": 2}], "total": 19.99, "note": null}),
    )
    .with_schema_version(SchemaVersion::new(3).unwrap())
    .with_metadata(meta(json!({"actor": "alice", "correlation_id": "c-9"})));

    let appended = store.append(&id, request).unwrap();
    let fetched = store.event(&id, Sequence::FIRST).unwrap();
    assert_eq!(fetched, appended);
    assert_eq!(fetched.schema_version.get(), 3);
    assert_eq!(fetched.payload["total"], json!(19.99));
    assert_eq!(fetched.metadata["actor"], json!("alice"));
}

#[test]
fn reads_are_in_sequence_order() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("s");
    seed(&store, &id, 25);
    // Interleave another stream to make sure reads are scoped by stream.
    seed(&store, &sid("other"), 5);

    let all = store.read_stream(&id).unwrap();
    assert_eq!(all.len(), 25);
    for (index, event) in all.iter().enumerate() {
        let n = index as u64 + 1;
        assert_eq!(event.sequence, seq(n));
        assert_eq!(event.payload, json!({ "n": n }));
        assert_eq!(event.stream_id, id);
    }
}

#[test]
fn read_after_pages_through_the_stream() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("s");
    seed(&store, &id, 10);

    let page = store.read_after(&id, seq(3), 4).unwrap();
    let seqs: Vec<u64> = page.iter().map(|e| e.sequence.get()).collect();
    assert_eq!(seqs, vec![4, 5, 6, 7]);

    let mut collected = Vec::new();
    let mut cursor = Sequence::ZERO;
    loop {
        let page = store.read_after(&id, cursor, 3).unwrap();
        let Some(last) = page.last() else { break };
        cursor = last.sequence;
        collected.extend(page);
    }
    assert_eq!(collected, store.read_stream(&id).unwrap());

    assert!(store.read_after(&id, seq(10), 5).unwrap().is_empty());
    assert!(store.read_after(&id, seq(1_000), 5).unwrap().is_empty());
    assert!(matches!(
        store.read_after(&id, Sequence::ZERO, 0),
        Err(StoreError::Validation(ValidationError::ZeroLimit))
    ));
}

#[test]
fn empty_stream_behaviour() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("empty");
    store.create_stream(&id, Metadata::new()).unwrap();
    assert!(store.read_stream(&id).unwrap().is_empty());
    assert!(
        store
            .read_after(&id, Sequence::ZERO, 10)
            .unwrap()
            .is_empty()
    );
    assert!(store.head(&id).unwrap().is_empty());
    assert!(matches!(
        store.event(&id, Sequence::FIRST),
        Err(StoreError::EventNotFound { sequence, .. }) if sequence == Sequence::FIRST
    ));
    assert!(matches!(
        store.event(&id, Sequence::ZERO),
        Err(StoreError::EventNotFound { .. })
    ));
    let report = store.verify_stream(&id).unwrap();
    assert!(report.is_valid());
    assert_eq!(report.events_checked, 0);
}

#[test]
fn missing_event_in_populated_stream() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("s");
    seed(&store, &id, 2);
    let err = store.event(&id, seq(3)).unwrap_err();
    assert_eq!(err.to_string(), "stream s has no event at sequence 3");
}

#[test]
fn events_survive_close_and_reopen() {
    let db = TestDb::new();
    let id = sid("durable");
    let before = {
        let store = db.open();
        seed(&store, &id, 20);
        store.read_stream(&id).unwrap()
    }; // store dropped: connection closed

    for _ in 0..3 {
        let store = db.open();
        assert_eq!(store.read_stream(&id).unwrap(), before);
        assert_eq!(store.head(&id).unwrap().digest, before[19].digest);
        assert!(store.verify_stream(&id).unwrap().is_valid());
    }

    // Appending after reopen continues the same sequence and chain.
    let store = db.open();
    let next = store.append(&id, req(21)).unwrap();
    assert_eq!(next.sequence, seq(21));
    assert_eq!(next.prev_digest, before[19].digest);
    assert!(store.verify_stream(&id).unwrap().is_valid());
}

#[test]
fn full_durability_mode_works() {
    let db = TestDb::new();
    let id = sid("full");
    {
        let store = EventStore::open(StoreConfig::new(&db.path)).unwrap();
        seed(&store, &id, 3);
    }
    let raw = db.raw();
    let mode: String = raw
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let store = EventStore::open(StoreConfig::new(&db.path)).unwrap();
    assert_eq!(store.read_stream(&id).unwrap().len(), 3);
}

#[test]
fn caller_supplied_event_ids() {
    let db = TestDb::new();
    let store = db.open();
    let a = sid("a");
    let b = sid("b");
    store.create_stream(&a, Metadata::new()).unwrap();
    store.create_stream(&b, Metadata::new()).unwrap();

    let id = EventId::generate();
    let event = store.append(&a, req(1).with_event_id(id)).unwrap();
    assert_eq!(event.event_id, id);

    // Event ids are globally unique, across streams too.
    for stream in [&a, &b] {
        let err = store.append(stream, req(2).with_event_id(id)).unwrap_err();
        assert!(
            matches!(err, StoreError::DuplicateEventId(dup) if dup == id),
            "{err}"
        );
    }
    assert_eq!(store.head(&a).unwrap().sequence, Sequence::FIRST);
    assert!(store.head(&b).unwrap().is_empty());
}

#[test]
fn payload_and_metadata_limits_are_enforced_on_canonical_size() {
    let db = TestDb::new();
    let store = EventStore::open(db.config().with_limits(Limits {
        max_payload_bytes: 16,
        max_metadata_bytes: 16,
    }))
    .unwrap();
    let id = sid("s");
    store.create_stream(&id, Metadata::new()).unwrap();

    // Canonical form `{"a":"0123456789"}` is 18 bytes.
    let too_big = AppendRequest::new(ty("t"), json!({"a": "0123456789"}));
    assert!(matches!(
        store.append(&id, too_big),
        Err(StoreError::Validation(ValidationError::PayloadTooLarge {
            actual: 18,
            max: 16
        }))
    ));
    // Whitespace in the client's formatting does not count: canonical size is 16.
    let pretty: serde_json::Value = serde_json::from_str("{ \"a\" :   \"01234567\" }").unwrap();
    assert!(
        store
            .append(&id, AppendRequest::new(ty("t"), pretty))
            .is_ok()
    );

    let big_meta = req(1).with_metadata(meta(json!({"k": "0123456789abcdef"})));
    assert!(matches!(
        store.append(&id, big_meta),
        Err(StoreError::Validation(
            ValidationError::MetadataTooLarge { .. }
        ))
    ));
    assert_eq!(store.head(&id).unwrap().sequence, Sequence::FIRST);
}

#[test]
fn payloads_are_stored_canonically() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("s");
    store.create_stream(&id, Metadata::new()).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(r#"{ "b": 2, "a": { "d": 4, "c": 3 } }"#).unwrap();
    store
        .append(&id, AppendRequest::new(ty("t"), value))
        .unwrap();
    let stored: String = db
        .raw()
        .query_row("SELECT payload FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, r#"{"a":{"c":3,"d":4},"b":2}"#);
}

#[test]
fn timestamps_are_recorded_but_not_used_for_order() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("s");
    seed(&store, &id, 3);
    // Rewind the clock on event 3 so that timestamps disagree with sequence
    // order; reads must still follow sequence order. (Bypasses the append-only
    // trigger, simulating a clock jump rather than normal operation.)
    let raw = db.raw();
    raw.execute_batch(
        "DROP TRIGGER events_no_update;
         UPDATE events SET recorded_at = 0 WHERE sequence = 3;",
    )
    .unwrap();
    let events = store.read_stream(&id).unwrap();
    let seqs: Vec<u64> = events.iter().map(|e| e.sequence.get()).collect();
    assert_eq!(seqs, vec![1, 2, 3]);
    assert!(events[2].recorded_at < events[0].recorded_at);
}
