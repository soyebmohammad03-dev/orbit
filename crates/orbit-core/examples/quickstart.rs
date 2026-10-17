//! Minimal end-to-end use of the ORBIT event store.
//!
//! ```sh
//! cargo run -p orbit-core --example quickstart
//! ```
//!
//! Writes `quickstart.db` in a temporary directory, appends a few events,
//! reads them back, and verifies the stream's integrity.

use orbit_core::{
    AppendRequest, EventStore, EventType, Metadata, Sequence, StoreConfig, StoreError, StreamId,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("quickstart.db");
    let store = EventStore::open(StoreConfig::new(&path))?;

    let account = StreamId::new("account-42")?;
    let mut metadata = Metadata::new();
    metadata.insert("tenant".into(), json!("acme"));
    store.create_stream(&account, metadata)?;

    let opened = EventType::new("account.opened")?;
    let deposited = EventType::new("funds.deposited")?;
    store.append(
        &account,
        AppendRequest::new(opened, json!({"owner": "alice"})).expect_sequence(Sequence::ZERO),
    )?;
    let last = store.append(
        &account,
        AppendRequest::new(deposited.clone(), json!({"amount_cents": 2_500}))
            .expect_sequence(Sequence::FIRST),
    )?;

    // A writer working from a stale head is rejected instead of silently interleaving.
    let stale =
        AppendRequest::new(deposited, json!({"amount_cents": 1})).expect_sequence(Sequence::FIRST);
    match store.append(&account, stale) {
        Err(StoreError::SequenceConflict {
            expected, actual, ..
        }) => {
            println!("rejected stale append: expected head {expected}, actual head {actual}");
        }
        other => return Err(format!("expected a conflict, got {other:?}").into()),
    }

    drop(store);
    let store = EventStore::open(StoreConfig::new(&path))?;
    for event in store.read_stream(&account)? {
        println!(
            "#{} {} v{} {} digest={}",
            event.sequence, event.event_type, event.schema_version, event.payload, event.digest
        );
    }
    let head = store.head(&account)?;
    assert_eq!(head.digest, last.digest);

    let report = store.verify_stream(&account)?;
    println!(
        "verified {} events: {}",
        report.events_checked,
        if report.is_valid() {
            "valid"
        } else {
            "INVALID"
        }
    );
    Ok(())
}
