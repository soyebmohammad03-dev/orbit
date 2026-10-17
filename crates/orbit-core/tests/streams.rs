//! Stream lifecycle: creation, metadata, existence, missing streams.

mod common;

use common::{TestDb, meta, req, sid};
use orbit_core::{Limits, Metadata, Sequence, StoreError, StreamHead, ValidationError};
use serde_json::json;

#[test]
fn create_and_inspect_stream() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("account-1");
    assert!(!store.stream_exists(&id).unwrap());

    let metadata = meta(json!({"tenant": "acme", "tags": ["a", "b"]}));
    let created = store.create_stream(&id, metadata.clone()).unwrap();
    assert_eq!(created.id, id);
    assert_eq!(created.metadata, metadata);
    assert_eq!(created.head, StreamHead::EMPTY);

    assert!(store.stream_exists(&id).unwrap());
    let loaded = store.stream(&id).unwrap();
    assert_eq!(loaded, created);
    assert_eq!(store.head(&id).unwrap(), StreamHead::EMPTY);
}

#[test]
fn duplicate_stream_is_rejected_and_original_kept() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("dup");
    store.create_stream(&id, meta(json!({"v": 1}))).unwrap();
    store.append(&id, req(1)).unwrap();

    let err = store.create_stream(&id, meta(json!({"v": 2}))).unwrap_err();
    assert!(
        matches!(err, StoreError::StreamAlreadyExists(ref s) if *s == id),
        "{err}"
    );
    assert_eq!(err.to_string(), "stream dup already exists");

    let stream = store.stream(&id).unwrap();
    assert_eq!(stream.metadata, meta(json!({"v": 1})));
    assert_eq!(stream.head.sequence, Sequence::FIRST);
}

#[test]
fn stream_ids_are_case_sensitive() {
    let db = TestDb::new();
    let store = db.open();
    store
        .create_stream(&sid("Orders"), Metadata::new())
        .unwrap();
    store
        .create_stream(&sid("orders"), Metadata::new())
        .unwrap();
    assert!(store.stream_exists(&sid("Orders")).unwrap());
    assert!(!store.stream_exists(&sid("ORDERS")).unwrap());
}

#[test]
fn operations_on_missing_stream_fail_explicitly() {
    let db = TestDb::new();
    let store = db.open();
    let missing = sid("nope");
    let is_not_found =
        |e: StoreError| matches!(e, StoreError::StreamNotFound(ref s) if *s == missing);

    assert!(is_not_found(store.stream(&missing).unwrap_err()));
    assert!(is_not_found(store.head(&missing).unwrap_err()));
    assert!(is_not_found(store.append(&missing, req(1)).unwrap_err()));
    assert!(is_not_found(
        store.event(&missing, Sequence::FIRST).unwrap_err()
    ));
    assert!(is_not_found(store.read_stream(&missing).unwrap_err()));
    assert!(is_not_found(
        store.read_after(&missing, Sequence::ZERO, 10).unwrap_err()
    ));
    assert!(is_not_found(store.verify_stream(&missing).unwrap_err()));
    // Nothing was created as a side effect.
    assert!(!store.stream_exists(&missing).unwrap());
}

#[test]
fn oversized_stream_metadata_is_rejected() {
    let db = TestDb::new();
    let store = orbit_core::EventStore::open(db.config().with_limits(Limits {
        max_payload_bytes: 1024,
        max_metadata_bytes: 32,
    }))
    .unwrap();
    let err = store
        .create_stream(&sid("big"), meta(json!({"k": "x".repeat(64)})))
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::Validation(ValidationError::MetadataTooLarge { max: 32, .. })
    ));
    assert!(!store.stream_exists(&sid("big")).unwrap());
}
