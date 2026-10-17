//! Criterion benchmarks for the core event store operations.
//!
//! Datasets are small and fixed so that a full run finishes in roughly a
//! minute on a laptop. Each benchmark builds its database once, outside the
//! measured section, in a fresh temporary directory. See docs/DEVELOPMENT.md.
#![allow(missing_docs)]

use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use orbit_core::{
    AppendRequest, Durability, EventStore, EventType, Metadata, Sequence, StoreConfig, StreamId,
};
use serde_json::json;
use tempfile::TempDir;

/// Events in the stream used by the read benchmarks.
const READ_STREAM_LEN: u64 = 10_000;
/// Events in the stream used by the full-read and verification benchmarks.
const SCAN_STREAM_LEN: u64 = 1_000;
/// Page size of the batched read benchmark.
const PAGE: usize = 100;

fn payload(n: u64) -> serde_json::Value {
    // ~150 bytes in canonical form.
    json!({
        "account": "acct-000042",
        "amount_cents": n * 7 % 100_000,
        "currency": "EUR",
        "memo": "benchmark payload of fixed shape",
        "n": n,
    })
}

fn request(n: u64) -> AppendRequest {
    AppendRequest::new(EventType::new("ledger.posted").unwrap(), payload(n))
}

fn store(durability: Durability) -> (TempDir, EventStore) {
    let dir = tempfile::tempdir().unwrap();
    let config = StoreConfig::new(dir.path().join("bench.db")).with_durability(durability);
    let store = EventStore::open(config).unwrap();
    (dir, store)
}

fn seeded(len: u64) -> (TempDir, EventStore, StreamId) {
    let (dir, store) = store(Durability::Normal);
    let id = StreamId::new("bench").unwrap();
    store.create_stream(&id, Metadata::new()).unwrap();
    for n in 1..=len {
        store.append(&id, request(n)).unwrap();
    }
    (dir, store, id)
}

fn append(c: &mut Criterion) {
    let mut group = c.benchmark_group("append");
    group.throughput(Throughput::Elements(1));

    for (name, durability) in [("normal", Durability::Normal), ("full", Durability::Full)] {
        let (_dir, store) = store(durability);
        let id = StreamId::new("bench").unwrap();
        store.create_stream(&id, Metadata::new()).unwrap();
        let mut n = 0;
        if durability == Durability::Full {
            // Every commit waits for a full flush to the device; keep it short.
            group.sample_size(10);
        }
        group.bench_function(name, |b| {
            b.iter_batched(
                || {
                    n += 1;
                    request(n)
                },
                |req| black_box(store.append(&id, req).unwrap()),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn reads(c: &mut Criterion) {
    let (_dir, store, id) = seeded(READ_STREAM_LEN);
    let mut group = c.benchmark_group("read");

    group.throughput(Throughput::Elements(1));
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    group.bench_function("event_by_sequence/10k", |b| {
        b.iter(|| {
            // Deterministic xorshift so every run touches the same sequence pattern.
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let sequence = Sequence::new(x % READ_STREAM_LEN + 1).unwrap();
            black_box(store.event(&id, sequence).unwrap())
        });
    });

    group.throughput(Throughput::Elements(PAGE as u64));
    group.bench_function("batch_after/100_of_10k", |b| {
        b.iter(|| {
            let after = Sequence::new(READ_STREAM_LEN / 2).unwrap();
            black_box(store.read_after(&id, after, PAGE).unwrap())
        });
    });
    group.finish();

    let (_dir, store, id) = seeded(SCAN_STREAM_LEN);
    let mut group = c.benchmark_group("scan");
    group.throughput(Throughput::Elements(SCAN_STREAM_LEN));
    group.bench_function("read_stream/1k", |b| {
        b.iter(|| black_box(store.read_stream(&id).unwrap()));
    });
    group.bench_function("verify_stream/1k", |b| {
        b.iter(|| {
            let report = store.verify_stream(&id).unwrap();
            assert!(report.is_valid());
            black_box(report)
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = append, reads
}
criterion_main!(benches);
