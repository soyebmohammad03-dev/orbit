//! Historical state diffs and per-event provenance, using the inventory projection.

mod common;

use common::{TestDb, counter_stream, inc, inventory, seed_with, seqn, sid, ty};
use orbit_core::projection::examples::{Counter, Inventory};
use orbit_core::{
    AppendRequest, Change, ChangeKind, Metadata, ReconstructOptions, ReconstructionError,
    ReconstructionReport, Reconstructor, Sequence, SnapshotPolicy, Target,
};
use serde_json::json;

fn warehouse(store: &orbit_core::EventStore) -> orbit_core::StreamId {
    let id = sid("warehouse-1");
    seed_with(
        store,
        &id,
        vec![
            inventory("inventory.warehouse_named", json!({"name": "North"})), // 1
            inventory(
                "inventory.item_added",
                json!({"sku": "A-1", "name": "Bolt", "quantity": 10}),
            ), // 2
            inventory(
                "inventory.item_added",
                json!({"sku": "B-2", "name": "Nut", "quantity": 4}),
            ), // 3
            inventory("inventory.reserved", json!({"sku": "A-1", "quantity": 3})), // 4
            inventory("inventory.shipped", json!({"sku": "A-1", "quantity": 2})), // 5
            inventory("inventory.discontinued", json!({"sku": "B-2"})),       // 6
            inventory("inventory.warehouse_named", json!({"name": "North-East"})), // 7
        ],
    );
    id
}

fn modified(path: &str, from: serde_json::Value, to: serde_json::Value) -> Change {
    Change {
        path: path.into(),
        kind: ChangeKind::Modified { from, to },
    }
}

#[test]
fn diff_between_two_historical_positions() {
    let db = TestDb::new();
    let store = db.open();
    let id = warehouse(&store);
    let inv = Reconstructor::new(&store, Inventory);

    let d = inv
        .diff(&id, seqn(3), seqn(7), ReconstructOptions::default())
        .unwrap();
    assert_eq!(d.from, seqn(3));
    assert_eq!(d.to, seqn(7));
    let seqs: Vec<u64> = d.events.iter().map(|e| e.sequence.get()).collect();
    assert_eq!(seqs, vec![4, 5, 6, 7]);
    assert_eq!(
        d.changes,
        vec![
            Change {
                path: "/discontinued/0".into(),
                kind: ChangeKind::Added {
                    value: json!("B-2")
                },
            },
            modified("/items/A-1/on_hand", json!(10), json!(7)),
            modified("/items/A-1/reserved", json!(0), json!(1)),
            Change {
                path: "/items/B-2".into(),
                kind: ChangeKind::Removed {
                    value: json!({"name": "Nut", "on_hand": 4, "reserved": 0})
                },
            },
            modified("/shipped_total", json!(0), json!(2)),
            modified("/warehouse", json!("North"), json!("North-East")),
        ]
    );
    assert_eq!(d.base.reconstructed, seqn(3));
}

#[test]
fn diff_from_the_initial_state_shows_additions() {
    let db = TestDb::new();
    let store = db.open();
    let id = warehouse(&store);
    let d = Reconstructor::new(&store, Inventory)
        .diff(&id, Sequence::ZERO, seqn(2), ReconstructOptions::default())
        .unwrap();
    assert_eq!(
        d.changes,
        vec![
            Change {
                path: "/items/A-1".into(),
                kind: ChangeKind::Added {
                    value: json!({"name": "Bolt", "on_hand": 10, "reserved": 0})
                },
            },
            modified("/warehouse", json!(null), json!("North")),
        ]
    );
}

#[test]
fn reversed_diff_describes_going_back_in_time() {
    let db = TestDb::new();
    let store = db.open();
    let id = warehouse(&store);
    let inv = Reconstructor::new(&store, Inventory);
    let forward = inv
        .diff(&id, seqn(5), seqn(6), ReconstructOptions::default())
        .unwrap();
    let backward = inv
        .diff(&id, seqn(6), seqn(5), ReconstructOptions::default())
        .unwrap();
    assert_eq!(forward.events, backward.events);
    assert_eq!(forward.changes.len(), backward.changes.len());
    for (f, b) in forward.changes.iter().zip(&backward.changes) {
        assert_eq!(f.path, b.path);
        match (&f.kind, &b.kind) {
            (ChangeKind::Added { value: x }, ChangeKind::Removed { value: y })
            | (ChangeKind::Removed { value: x }, ChangeKind::Added { value: y }) => {
                assert_eq!(x, y)
            }
            (ChangeKind::Modified { from, to }, ChangeKind::Modified { from: f2, to: t2 }) => {
                assert_eq!((from, to), (t2, f2));
            }
            other => panic!("not inverse: {other:?}"),
        }
    }
}

#[test]
fn identical_states_have_no_changes() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    // Event 2 increments by zero: an event happened, but the state did not change.
    seed_with(&store, &id, vec![inc(5), inc(0)]);
    let counter = Reconstructor::new(&store, Counter);
    let same_position = counter
        .diff(&id, seqn(2), seqn(2), ReconstructOptions::default())
        .unwrap();
    assert!(same_position.changes.is_empty());
    assert!(same_position.events.is_empty());
    let no_effect = counter
        .diff(&id, seqn(1), seqn(2), ReconstructOptions::default())
        .unwrap();
    assert!(no_effect.changes.is_empty());
    assert_eq!(no_effect.events.len(), 1);
}

#[test]
fn diff_on_empty_history_and_invalid_positions() {
    let db = TestDb::new();
    let store = db.open();
    let empty = sid("empty");
    store.create_stream(&empty, Metadata::new()).unwrap();
    let counter = Reconstructor::new(&store, Counter);
    let d = counter
        .diff(
            &empty,
            Sequence::ZERO,
            Sequence::ZERO,
            ReconstructOptions::default(),
        )
        .unwrap();
    assert!(d.changes.is_empty());

    for (from, to) in [(0, 1), (1, 0)] {
        let err = counter
            .diff(&empty, seqn(from), seqn(to), ReconstructOptions::default())
            .unwrap_err();
        assert!(
            matches!(err, ReconstructionError::TargetBeyondHead { .. }),
            "{err}"
        );
    }
    let err = counter
        .diff(
            &sid("ghost"),
            Sequence::ZERO,
            Sequence::ZERO,
            ReconstructOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, ReconstructionError::Store(_)), "{err}");
}

#[test]
fn projection_errors_inside_the_interval_are_reported() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    seed_with(
        &store,
        &id,
        vec![
            inc(1),
            AppendRequest::new(ty("counter.squared"), json!({})),
            inc(1),
        ],
    );
    let counter = Reconstructor::new(&store, Counter);
    let err = counter
        .diff(&id, seqn(1), seqn(3), ReconstructOptions::default())
        .unwrap_err();
    assert!(
        matches!(&err, ReconstructionError::Projection(f) if f.sequence == seqn(2)),
        "{err}"
    );
    assert!(
        counter
            .explain(&id, seqn(0), seqn(3), ReconstructOptions::default())
            .is_err()
    );
}

#[test]
fn diffs_are_deterministic_and_snapshot_independent() {
    let db = TestDb::new();
    let store = db.open();
    let id = warehouse(&store);
    let inv = Reconstructor::new(&store, Inventory);
    let plain = inv
        .diff(
            &id,
            seqn(2),
            seqn(7),
            ReconstructOptions::with_snapshots(SnapshotPolicy::Ignore),
        )
        .unwrap();
    let again = inv
        .diff(
            &id,
            seqn(2),
            seqn(7),
            ReconstructOptions::with_snapshots(SnapshotPolicy::Ignore),
        )
        .unwrap();
    // Everything except the measured duration is identical.
    assert_eq!(
        (&again.changes, &again.events),
        (&plain.changes, &plain.events)
    );
    assert_eq!(
        ReconstructionReport {
            elapsed_micros: 0,
            ..again.base
        },
        ReconstructionReport {
            elapsed_micros: 0,
            ..plain.base.clone()
        }
    );
    inv.create_snapshot(&id, Target::At(seqn(2)), ReconstructOptions::default())
        .unwrap();
    let assisted = inv
        .diff(&id, seqn(2), seqn(7), ReconstructOptions::default())
        .unwrap();
    assert_eq!(assisted.changes, plain.changes);
    assert_eq!(assisted.events, plain.events);
    assert_eq!(
        assisted.base.events_applied, 0,
        "base came from the snapshot"
    );
    // Serialized form is stable too.
    assert_eq!(
        serde_json::to_string(&assisted.changes).unwrap(),
        serde_json::to_string(&plain.changes).unwrap()
    );
}

#[test]
fn explain_attributes_each_change_to_its_event() {
    let db = TestDb::new();
    let store = db.open();
    let id = warehouse(&store);
    let explanation = Reconstructor::new(&store, Inventory)
        .explain(&id, seqn(3), seqn(6), ReconstructOptions::default())
        .unwrap();
    let summary: Vec<(u64, &str, Vec<&str>)> = explanation
        .transitions
        .iter()
        .map(|t| {
            (
                t.event.sequence.get(),
                t.event.event_type.as_str(),
                t.changes.iter().map(|c| c.path.as_str()).collect(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                4,
                "inventory.reserved",
                vec!["/items/A-1/on_hand", "/items/A-1/reserved"]
            ),
            (
                5,
                "inventory.shipped",
                vec!["/items/A-1/reserved", "/shipped_total"]
            ),
            (
                6,
                "inventory.discontinued",
                vec!["/discontinued/0", "/items/B-2"]
            ),
        ]
    );

    let empty = Reconstructor::new(&store, Inventory)
        .explain(&id, seqn(7), seqn(7), ReconstructOptions::default())
        .unwrap();
    assert!(empty.transitions.is_empty());
    let err = Reconstructor::new(&store, Inventory)
        .explain(&id, seqn(5), seqn(4), ReconstructOptions::default())
        .unwrap_err();
    assert!(
        matches!(err, ReconstructionError::InvalidRange { .. }),
        "{err}"
    );
}

#[test]
fn large_interval_diff_crosses_page_boundaries() {
    let db = TestDb::new();
    let store = db.open();
    let id = sid("c");
    counter_stream(&store, &id, 1_100);
    let d = Reconstructor::new(&store, Counter)
        .diff(&id, seqn(100), seqn(1_100), ReconstructOptions::default())
        .unwrap();
    assert_eq!(d.events.len(), 1_000);
    assert_eq!(d.events.first().unwrap().sequence, seqn(101));
    assert_eq!(d.events.last().unwrap().sequence, seqn(1_100));
    assert_eq!(
        d.changes,
        vec![modified(
            "/value",
            json!(common::triangle(100)),
            json!(common::triangle(1_100))
        )]
    );
}
