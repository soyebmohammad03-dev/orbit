//! Property-based tests over generated event sequences, using real SQLite files.

mod common;

use common::{TestDb, sid};
use orbit_core::domain::canonical::to_canonical_string;
use orbit_core::{AppendRequest, EventType, IntegrityIssue, Metadata, Sequence};
use proptest::prelude::*;
use serde_json::{Map, Value};

fn json_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>()
            .prop_filter("JSON has no NaN/inf", |f| f.is_finite())
            .prop_map(Value::from),
        ".{0,12}".prop_map(Value::String),
    ]
}

fn json_value() -> impl Strategy<Value = Value> {
    json_leaf().prop_recursive(3, 24, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
            prop::collection::btree_map(".{0,6}", inner, 0..5)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

fn metadata() -> impl Strategy<Value = Metadata> {
    prop::collection::btree_map("[a-z_]{1,8}", json_leaf(), 0..4)
        .prop_map(|m| m.into_iter().collect::<Map<String, Value>>())
}

fn event_type() -> impl Strategy<Value = EventType> {
    "[a-z][a-z0-9._:-]{0,20}".prop_map(|s| EventType::new(s).unwrap())
}

fn requests(max: usize) -> impl Strategy<Value = Vec<AppendRequest>> {
    prop::collection::vec(
        (event_type(), json_value(), metadata(), 1u32..5).prop_map(|(ty, payload, meta, v)| {
            AppendRequest::new(ty, payload)
                .with_metadata(meta)
                .with_schema_version(orbit_core::SchemaVersion::new(v).unwrap())
        }),
        0..max,
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn canonical_json_round_trips_and_is_idempotent(value in json_value()) {
        let text = to_canonical_string(&value);
        let reparsed: Value = serde_json::from_str(&text).unwrap();
        prop_assert_eq!(&reparsed, &value);
        prop_assert_eq!(to_canonical_string(&reparsed), text);
    }

    #[test]
    fn append_then_read_preserves_contents_order_and_survives_reopen(reqs in requests(24)) {
        let db = TestDb::new();
        let id = sid("prop");
        let appended = {
            let store = db.open();
            store.create_stream(&id, Metadata::new()).unwrap();
            reqs.iter()
                .map(|r| store.append(&id, r.clone()).unwrap())
                .collect::<Vec<_>>()
        };

        for (index, (event, request)) in appended.iter().zip(&reqs).enumerate() {
            prop_assert_eq!(event.sequence.get(), index as u64 + 1);
            prop_assert_eq!(&event.payload, &request.payload);
            prop_assert_eq!(&event.metadata, &request.metadata);
            prop_assert_eq!(&event.event_type, &request.event_type);
        }

        let store = db.open();
        let read = store.read_stream(&id).unwrap();
        prop_assert_eq!(&read, &appended);
        let head = store.head(&id).unwrap();
        prop_assert_eq!(head.sequence.get(), reqs.len() as u64);
        let report = store.verify_stream(&id).unwrap();
        prop_assert!(report.is_valid(), "{:?}", report);
    }

    #[test]
    fn paged_reads_equal_full_read(count in 0u64..40, page in 1usize..10) {
        let db = TestDb::new();
        let store = db.open();
        let id = sid("pages");
        common::seed(&store, &id, count);
        let mut cursor = Sequence::ZERO;
        let mut collected = Vec::new();
        loop {
            let batch = store.read_after(&id, cursor, page).unwrap();
            prop_assert!(batch.len() <= page);
            let Some(last) = batch.last() else { break };
            cursor = last.sequence;
            collected.extend(batch);
        }
        prop_assert_eq!(collected, store.read_stream(&id).unwrap());
    }

    #[test]
    fn any_single_payload_modification_is_detected(
        count in 1u64..20,
        target_seed in any::<u64>(),
        replacement in json_value(),
    ) {
        let db = TestDb::new();
        let id = sid("tamper");
        common::seed(&db.open(), &id, count);
        let target = target_seed % count + 1;
        let original = format!("{{\"n\":{target}}}");
        let replacement = to_canonical_string(&replacement);
        prop_assume!(replacement != original);

        let raw = db.raw();
        raw.execute_batch("DROP TRIGGER events_no_update").unwrap();
        raw.execute(
            "UPDATE events SET payload = ?1 WHERE sequence = ?2",
            rusqlite::params![replacement, target as i64],
        ).unwrap();

        let report = db.open().verify_stream(&id).unwrap();
        prop_assert_eq!(report.issues.len(), 1);
        let is_expected = matches!(
            report.issues[0],
            IntegrityIssue::DigestMismatch { sequence, .. } if sequence.get() == target
        );
        prop_assert!(is_expected, "{:?}", report.issues);
    }
}
