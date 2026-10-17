//! Property tests: snapshots never change reconstructed results.

mod common;

use std::collections::BTreeSet;

use common::{TestDb, at, dec, inc, inventory, reset, seed_with, sid};
use orbit_core::diff::diff;
use orbit_core::projection::examples::{Counter, Inventory, InventoryState};
use orbit_core::reconstruct::StartPoint;
use orbit_core::{
    AppendRequest, Metadata, Projection, ReconstructOptions, Reconstructor, SnapshotPolicy,
    StoredEvent,
};
use proptest::prelude::*;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
enum CounterOp {
    Inc(u64),
    Dec(u64),
    Reset,
}

fn counter_ops(max: usize) -> impl Strategy<Value = Vec<CounterOp>> {
    prop::collection::vec(
        prop_oneof![
            4 => (0u64..1_000).prop_map(CounterOp::Inc),
            3 => (0u64..1_000).prop_map(CounterOp::Dec),
            1 => Just(CounterOp::Reset),
        ],
        0..max,
    )
}

fn to_request(op: &CounterOp) -> AppendRequest {
    match op {
        CounterOp::Inc(n) => inc(*n),
        CounterOp::Dec(n) => dec(*n),
        CounterOp::Reset => reset(),
    }
}

fn reference_counter(ops: &[CounterOp]) -> i64 {
    ops.iter().fold(0, |v, op| match op {
        CounterOp::Inc(n) => v + i64::try_from(*n).unwrap(),
        CounterOp::Dec(n) => v - i64::try_from(*n).unwrap(),
        CounterOp::Reset => 0,
    })
}

/// Random inventory commands; only those the projection accepts are appended,
/// so the stream is always a valid history.
fn inventory_commands() -> impl Strategy<Value = Vec<(u8, u8, u64)>> {
    prop::collection::vec((0u8..6, 0u8..4, 0u64..20), 0..50)
}

fn command(kind: u8, sku: u8, quantity: u64) -> AppendRequest {
    let sku = format!("SKU-{sku}");
    match kind {
        0 => inventory(
            "inventory.item_added",
            json!({"sku": sku, "name": format!("item {sku}"), "quantity": quantity}),
        ),
        1 => inventory(
            "inventory.restocked",
            json!({"sku": sku, "quantity": quantity}),
        ),
        2 => inventory(
            "inventory.reserved",
            json!({"sku": sku, "quantity": quantity}),
        ),
        3 => inventory(
            "inventory.shipped",
            json!({"sku": sku, "quantity": quantity}),
        ),
        4 => inventory("inventory.discontinued", json!({"sku": sku})),
        _ => inventory(
            "inventory.warehouse_named",
            json!({"name": format!("W{quantity}")}),
        ),
    }
}

/// Pretends a request was stored, to test whether the projection accepts it.
fn as_event(request: &AppendRequest) -> StoredEvent {
    StoredEvent {
        event_id: orbit_core::EventId::generate(),
        stream_id: sid("x"),
        sequence: orbit_core::Sequence::FIRST,
        event_type: request.event_type.clone(),
        schema_version: request.schema_version,
        recorded_at: orbit_core::Timestamp::from_unix_micros(0).unwrap(),
        payload: request.payload.clone(),
        metadata: Metadata::new(),
        prev_digest: orbit_core::Digest::GENESIS,
        digest: orbit_core::Digest::GENESIS,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, ..ProptestConfig::default() })]

    #[test]
    fn counter_snapshots_never_change_results(
        ops in counter_ops(60),
        snapshot_picks in prop::collection::vec(any::<u64>(), 0..6),
        target_picks in prop::collection::vec(any::<u64>(), 1..6),
    ) {
        let db = TestDb::new();
        let store = db.open();
        let id = sid("p");
        seed_with(&store, &id, ops.iter().map(to_request).collect());
        let n = ops.len() as u64;
        let counter = Reconstructor::new(&store, Counter);

        let snapshot_seqs: BTreeSet<u64> = if n == 0 {
            BTreeSet::new()
        } else {
            snapshot_picks.iter().map(|p| p % n + 1).collect()
        };
        for s in &snapshot_seqs {
            counter.create_snapshot(&id, at(*s), ReconstructOptions::default()).unwrap();
        }

        for pick in target_picks {
            let target = pick % (n + 1);
            let expected = reference_counter(&ops[..usize::try_from(target).unwrap()]);
            let full = counter
                .reconstruct(&id, at(target), ReconstructOptions::with_snapshots(SnapshotPolicy::Ignore))
                .unwrap();
            let assisted = counter.reconstruct(&id, at(target), ReconstructOptions::default()).unwrap();
            prop_assert_eq!(full.state.value, expected);
            prop_assert_eq!(assisted.state.value, expected);
            prop_assert_eq!(full.report.reconstructed_digest, assisted.report.reconstructed_digest);

            // The newest snapshot at or before the target is the one used, never a later one.
            let best = snapshot_seqs.range(..=target).next_back().copied();
            match (best, &assisted.report.start) {
                (None, StartPoint::Initial) => prop_assert_eq!(assisted.report.events_applied, target),
                (Some(b), StartPoint::Snapshot { sequence, .. }) => {
                    prop_assert_eq!(sequence.get(), b);
                    prop_assert_eq!(assisted.report.events_applied, target - b);
                }
                (best, start) => prop_assert!(false, "expected {:?}, started from {:?}", best, start),
            }
        }
    }

    #[test]
    fn inventory_snapshots_never_change_results(
        commands in inventory_commands(),
        interval in 1u64..8,
        target_pick in any::<u64>(),
    ) {
        let db = TestDb::new();
        let store = db.open();
        let id = sid("inv");
        store.create_stream(&id, Metadata::new()).unwrap();

        // Reference fold, outside the store; keep only accepted commands.
        let mut reference = vec![Inventory.initial_state()];
        for (kind, sku, quantity) in commands {
            let request = command(kind, sku, quantity);
            let current: InventoryState = reference.last().unwrap().clone();
            if let Ok(next) = Inventory.apply(current, &as_event(&request)) {
                store.append(&id, request).unwrap();
                reference.push(next);
            }
        }
        let n = (reference.len() - 1) as u64;
        let inv = Reconstructor::new(&store, Inventory);
        let mut s = interval;
        while s <= n {
            inv.create_snapshot(&id, at(s), ReconstructOptions::default()).unwrap();
            s += interval;
        }

        let target = target_pick % (n + 1);
        let expected = &reference[usize::try_from(target).unwrap()];
        let report = inv
            .compare_with_full_replay(&id, at(target), SnapshotPolicy::Strict)
            .unwrap();
        prop_assert!(report.equivalent, "{:?}", report.differences);
        let assisted = inv.reconstruct(&id, at(target), ReconstructOptions::default()).unwrap();
        prop_assert_eq!(&assisted.state, expected);

        // Diff between the initial and target states is deterministic and
        // empty exactly when the states are equal.
        let a = serde_json::to_value(&reference[0]).unwrap();
        let b = serde_json::to_value(expected).unwrap();
        let d1 = inv.diff(&id, orbit_core::Sequence::ZERO, assisted.report.reconstructed, ReconstructOptions::default()).unwrap();
        let d2 = inv.diff(&id, orbit_core::Sequence::ZERO, assisted.report.reconstructed, ReconstructOptions::default()).unwrap();
        prop_assert_eq!(&d1.changes, &d2.changes);
        prop_assert_eq!(&d1.changes, &diff(&a, &b));
        prop_assert_eq!(d1.changes.is_empty(), a == b);
    }

    #[test]
    fn diff_is_deterministic_and_empty_only_for_equal_values(
        a in json_value(),
        b in json_value(),
    ) {
        prop_assert_eq!(diff(&a, &b), diff(&a, &b));
        prop_assert!(diff(&a, &a).is_empty());
        prop_assert_eq!(diff(&a, &b).is_empty(), a == b);
        // Every reported path is unique.
        let paths: BTreeSet<String> = diff(&a, &b).into_iter().map(|c| c.path).collect();
        prop_assert_eq!(paths.len(), diff(&a, &b).len());
    }
}

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-5i64..5).prop_map(Value::from),
        "[ab]{0,2}".prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 20, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map("[a-c]{1}", inner, 0..4)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}
