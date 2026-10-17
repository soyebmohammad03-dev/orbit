//! Criterion benchmarks for Phase 2: reconstruction, snapshots and diffs.
//!
//! Every dataset is built once per group in a fresh temporary directory
//! (`Durability::Normal`), outside the measured code. Full replay and
//! snapshot-assisted reconstruction are measured on the *same* database and
//! target, so their results are directly comparable. See docs/DEVELOPMENT.md.
#![allow(missing_docs)]

use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use orbit_core::projection::examples::{Counter, Inventory};
use orbit_core::{
    AppendRequest, Durability, EventStore, EventType, Metadata, ReconstructOptions, Reconstructor,
    Sequence, SnapshotPolicy, StoreConfig, StreamId, Target,
};
use serde_json::json;
use tempfile::TempDir;

/// Counter stream length for the main reconstruction benchmarks.
const LONG: u64 = 10_000;
/// Counter stream length for the small-stream comparison.
const SHORT: u64 = 100;
/// Inventory stream length for the diff benchmarks.
const INVENTORY_LEN: u64 = 2_000;
/// Distinct SKUs in the inventory dataset.
const SKUS: u64 = 50;

fn open() -> (TempDir, EventStore) {
    let dir = tempfile::tempdir().unwrap();
    let config = StoreConfig::new(dir.path().join("bench.db")).with_durability(Durability::Normal);
    (dir, EventStore::open(config).unwrap())
}

fn inc(by: u64) -> AppendRequest {
    AppendRequest::new(
        EventType::new("counter.incremented").unwrap(),
        json!({ "by": by }),
    )
}

fn counter_stream(store: &EventStore, len: u64) -> StreamId {
    let id = StreamId::new("counter").unwrap();
    store.create_stream(&id, Metadata::new()).unwrap();
    for n in 1..=len {
        store.append(&id, inc(n % 10)).unwrap();
    }
    id
}

/// Deterministic, always-valid inventory history: SKUS item additions followed
/// by a restock / reserve / ship cycle over the SKUs.
fn inventory_stream(store: &EventStore, len: u64) -> StreamId {
    let id = StreamId::new("inventory").unwrap();
    store.create_stream(&id, Metadata::new()).unwrap();
    let ty = |t: &str| EventType::new(t).unwrap();
    for n in 0..len {
        let sku = format!("SKU-{:03}", n % SKUS);
        let request = if n < SKUS {
            AppendRequest::new(
                ty("inventory.item_added"),
                json!({"sku": sku, "name": "part", "quantity": 100}),
            )
        } else {
            match (n / SKUS) % 3 {
                0 => AppendRequest::new(
                    ty("inventory.restocked"),
                    json!({"sku": sku, "quantity": 5}),
                ),
                1 => {
                    AppendRequest::new(ty("inventory.reserved"), json!({"sku": sku, "quantity": 3}))
                }
                _ => {
                    AppendRequest::new(ty("inventory.shipped"), json!({"sku": sku, "quantity": 2}))
                }
            }
        };
        store.append(&id, request).unwrap();
    }
    id
}

fn seq(n: u64) -> Sequence {
    Sequence::new(n).unwrap()
}

fn options(policy: SnapshotPolicy) -> ReconstructOptions {
    ReconstructOptions::with_snapshots(policy)
}

fn reconstruct(c: &mut Criterion) {
    let mut group = c.benchmark_group("reconstruct");
    group.sample_size(20);

    // Long stream: full replay vs. snapshot near the target vs. snapshot at the target.
    {
        let (_dir, store) = open();
        let id = counter_stream(&store, LONG);
        let counter = Reconstructor::new(&store, Counter);
        counter
            .create_snapshot(&id, Target::At(seq(9_000)), options(SnapshotPolicy::Ignore))
            .unwrap();
        group.bench_function("counter_10k/full_replay", |b| {
            b.iter(|| {
                black_box(
                    counter
                        .reconstruct(&id, Target::Latest, options(SnapshotPolicy::Ignore))
                        .unwrap(),
                )
            });
        });
        group.bench_function("counter_10k/snapshot_at_9000", |b| {
            b.iter(|| {
                black_box(
                    counter
                        .reconstruct(&id, Target::Latest, options(SnapshotPolicy::Prefer))
                        .unwrap(),
                )
            });
        });
        counter
            .create_snapshot(&id, Target::Latest, options(SnapshotPolicy::Ignore))
            .unwrap();
        group.bench_function("counter_10k/snapshot_at_head", |b| {
            b.iter(|| {
                black_box(
                    counter
                        .reconstruct(&id, Target::Latest, options(SnapshotPolicy::Prefer))
                        .unwrap(),
                )
            });
        });
        group.bench_function("counter_10k/historical_5000_full_replay", |b| {
            b.iter(|| {
                black_box(
                    counter
                        .reconstruct(&id, Target::At(seq(5_000)), options(SnapshotPolicy::Ignore))
                        .unwrap(),
                )
            });
        });
    }

    // Short stream: does a snapshot pay off at all?
    {
        let (_dir, store) = open();
        let id = counter_stream(&store, SHORT);
        let counter = Reconstructor::new(&store, Counter);
        group.bench_function("counter_100/full_replay", |b| {
            b.iter(|| {
                black_box(
                    counter
                        .reconstruct(&id, Target::Latest, options(SnapshotPolicy::Ignore))
                        .unwrap(),
                )
            });
        });
        group.bench_function("counter_100/prefer_no_snapshot_exists", |b| {
            b.iter(|| {
                black_box(
                    counter
                        .reconstruct(&id, Target::Latest, options(SnapshotPolicy::Prefer))
                        .unwrap(),
                )
            });
        });
        counter
            .create_snapshot(&id, Target::At(seq(90)), options(SnapshotPolicy::Ignore))
            .unwrap();
        group.bench_function("counter_100/snapshot_at_90", |b| {
            b.iter(|| {
                black_box(
                    counter
                        .reconstruct(&id, Target::Latest, options(SnapshotPolicy::Prefer))
                        .unwrap(),
                )
            });
        });
    }
    group.finish();
}

fn snapshots(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_create");
    group.sample_size(20);

    // Each iteration first appends one event (setup, not measured), so every
    // measured call creates a genuinely new snapshot at the new head. Each
    // benchmark gets its own database: the stream grows by one event per
    // iteration, and the cheap incremental case runs many more iterations.
    {
        let (_dir, store) = open();
        let id = counter_stream(&store, LONG);
        let counter = Reconstructor::new(&store, Counter);
        group.bench_function("counter_10k/from_full_replay", |b| {
            b.iter_batched(
                || store.append(&id, inc(1)).unwrap(),
                |_| {
                    black_box(
                        counter
                            .create_snapshot(&id, Target::Latest, options(SnapshotPolicy::Ignore))
                            .unwrap(),
                    )
                },
                BatchSize::PerIteration,
            );
        });
    }
    {
        let (_dir, store) = open();
        let id = counter_stream(&store, LONG);
        let counter = Reconstructor::new(&store, Counter);
        counter
            .create_snapshot(&id, Target::Latest, options(SnapshotPolicy::Ignore))
            .unwrap();
        group.bench_function("counter_10k/incremental_from_previous_snapshot", |b| {
            b.iter_batched(
                || store.append(&id, inc(1)).unwrap(),
                |_| {
                    black_box(
                        counter
                            .create_snapshot(&id, Target::Latest, options(SnapshotPolicy::Prefer))
                            .unwrap(),
                    )
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn diffs(c: &mut Criterion) {
    let mut group = c.benchmark_group("diff");
    group.sample_size(20);
    let (_dir, store) = open();
    let id = inventory_stream(&store, INVENTORY_LEN);
    let inventory = Reconstructor::new(&store, Inventory);
    group.bench_function("inventory_2k/1000_to_2000/full_replay", |b| {
        b.iter(|| {
            black_box(
                inventory
                    .diff(&id, seq(1_000), seq(2_000), options(SnapshotPolicy::Ignore))
                    .unwrap(),
            )
        });
    });
    inventory
        .create_snapshot(&id, Target::At(seq(1_000)), options(SnapshotPolicy::Ignore))
        .unwrap();
    group.bench_function("inventory_2k/1000_to_2000/snapshot_at_1000", |b| {
        b.iter(|| {
            black_box(
                inventory
                    .diff(&id, seq(1_000), seq(2_000), options(SnapshotPolicy::Prefer))
                    .unwrap(),
            )
        });
    });
    group.bench_function("inventory_2k/explain_1900_to_2000", |b| {
        b.iter(|| {
            black_box(
                inventory
                    .explain(&id, seq(1_900), seq(2_000), options(SnapshotPolicy::Prefer))
                    .unwrap(),
            )
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = reconstruct, snapshots, diffs
}
criterion_main!(benches);
