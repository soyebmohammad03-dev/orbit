//! Criterion benchmarks for Phase 3: concurrency, optimistic conflicts,
//! store verification, replay, and failure/rollback overhead.
//!
//! Every group builds its own database in a fresh temporary directory
//! (`Durability::Normal`), outside the measured code. See docs/DEVELOPMENT.md.
#![allow(missing_docs)]

use std::hint::black_box;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use orbit_core::fault::FaultPoint;
use orbit_core::projection::examples::Counter;
use orbit_core::replay::{ReplayLab, ReplayStart};
use orbit_core::{
    AppendRequest, Durability, EventStore, EventType, Metadata, ReconstructOptions, Reconstructor,
    Sequence, StoreConfig, StoreError, StreamId, Target,
};
use serde_json::json;
use tempfile::TempDir;

const VERIFY_LEN: u64 = 10_000;
const REPLAY_LEN: u64 = 1_000;
const THREADS: usize = 4;
const PER_THREAD: u64 = 100;

fn config(dir: &TempDir) -> StoreConfig {
    StoreConfig::new(dir.path().join("bench.db"))
        .with_durability(Durability::Normal)
        .with_busy_timeout(Duration::from_secs(60))
}

fn open() -> (TempDir, EventStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(config(&dir)).unwrap();
    (dir, store)
}

fn inc(by: u64) -> AppendRequest {
    AppendRequest::new(
        EventType::new("counter.incremented").unwrap(),
        json!({ "by": by }),
    )
}

fn stream() -> StreamId {
    StreamId::new("bench").unwrap()
}

fn seeded(len: u64) -> (TempDir, EventStore) {
    let (dir, store) = open();
    store.create_stream(&stream(), Metadata::new()).unwrap();
    for n in 1..=len {
        store.append(&stream(), inc(n % 10)).unwrap();
    }
    (dir, store)
}

fn concurrency(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrency");
    group.sample_size(10);
    // Setup (unmeasured): a fresh database and one connection per thread.
    // Measured: all threads appending PER_THREAD events each to one stream.
    group.bench_function("4_connections_x_100_appends", |b| {
        b.iter_batched(
            || {
                let (dir, store) = open();
                store.create_stream(&stream(), Metadata::new()).unwrap();
                let stores: Vec<_> = (0..THREADS)
                    .map(|_| Arc::new(EventStore::open(config(&dir)).unwrap()))
                    .collect();
                (dir, stores)
            },
            |(_dir, stores)| {
                let handles: Vec<_> = stores
                    .into_iter()
                    .map(|store| {
                        thread::spawn(move || {
                            for n in 0..PER_THREAD {
                                store.append(&stream(), inc(n)).unwrap();
                            }
                        })
                    })
                    .collect();
                for h in handles {
                    h.join().unwrap();
                }
            },
            BatchSize::PerIteration,
        );
    });
    group.bench_function("1_connection_x_400_appends", |b| {
        b.iter_batched(
            || {
                let (dir, store) = open();
                store.create_stream(&stream(), Metadata::new()).unwrap();
                (dir, store)
            },
            |(_dir, store)| {
                for n in 0..(THREADS as u64 * PER_THREAD) {
                    store.append(&stream(), inc(n)).unwrap();
                }
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

fn optimistic(c: &mut Criterion) {
    let mut group = c.benchmark_group("optimistic");
    {
        let (_dir, store) = seeded(100);
        let mut head = store.head(&stream()).unwrap().sequence;
        group.bench_function("append_expected_head_success", |b| {
            b.iter(|| {
                let e = store
                    .append(&stream(), inc(1).expect_sequence(head))
                    .unwrap();
                head = e.sequence;
                black_box(e)
            });
        });
    }
    {
        let (_dir, store) = seeded(100);
        group.bench_function("append_stale_head_conflict", |b| {
            b.iter(|| {
                let err = store
                    .append(&stream(), inc(1).expect_sequence(Sequence::FIRST))
                    .unwrap_err();
                assert!(matches!(err, StoreError::SequenceConflict { .. }));
                black_box(err)
            });
        });
    }
    group.finish();
}

fn failure(c: &mut Criterion) {
    let mut group = c.benchmark_group("failure");
    // Both benchmarks install a hook; only the second one fails, at the last
    // point before COMMIT, so it pays for the whole append plus a rollback.
    {
        let (_dir, store) = seeded(100);
        store.set_fault_hook(Some(Arc::new(|_| false)));
        group.bench_function("append_committed", |b| {
            b.iter(|| black_box(store.append(&stream(), inc(1)).unwrap()));
        });
    }
    {
        let (_dir, store) = seeded(100);
        store.set_fault_hook(Some(Arc::new(|p| p == FaultPoint::AppendBeforeCommit)));
        group.bench_function("append_rolled_back_before_commit", |b| {
            b.iter(|| black_box(store.append(&stream(), inc(1)).unwrap_err()));
        });
    }
    group.finish();
}

fn verification(c: &mut Criterion) {
    let mut group = c.benchmark_group("verify");
    group.sample_size(10);
    let (_dir, store) = seeded(VERIFY_LEN);
    let counter = Reconstructor::new(&store, Counter);
    for n in (1_000..=VERIFY_LEN).step_by(1_000) {
        counter
            .create_snapshot(
                &stream(),
                Target::At(Sequence::new(n).unwrap()),
                ReconstructOptions::default(),
            )
            .unwrap();
    }
    group.bench_function("verify_stream_10k", |b| {
        b.iter(|| black_box(store.verify_stream(&stream()).unwrap()));
    });
    group.bench_function("verify_store_10k_events_10_snapshots", |b| {
        b.iter(|| {
            let report = store.verify_store().unwrap();
            assert!(report.is_valid());
            black_box(report)
        });
    });
    group.finish();
}

fn replay(c: &mut Criterion) {
    let mut group = c.benchmark_group("replay");
    group.sample_size(20);
    let (_dir, store) = seeded(REPLAY_LEN);
    let lab = ReplayLab::new(&store, Counter);
    let snapshot = Reconstructor::new(&store, Counter)
        .create_snapshot(
            &stream(),
            Target::At(Sequence::new(900).unwrap()),
            ReconstructOptions::default(),
        )
        .unwrap()
        .saved
        .snapshot;
    group.bench_function("record_1k_from_initial", |b| {
        b.iter(|| {
            black_box(
                lab.record(&stream(), ReplayStart::Initial, Target::Latest)
                    .unwrap(),
            )
        });
    });
    let full = lab
        .record(&stream(), ReplayStart::Initial, Target::Latest)
        .unwrap();
    group.bench_function("execute_1k_from_initial", |b| {
        b.iter(|| black_box(lab.execute(full.id).unwrap()));
    });
    let tail = lab
        .record(
            &stream(),
            ReplayStart::Snapshot {
                snapshot_id: snapshot.id,
            },
            Target::Latest,
        )
        .unwrap();
    group.bench_function("execute_100_from_snapshot_at_900", |b| {
        b.iter(|| black_box(lab.execute(tail.id).unwrap()));
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = concurrency, optimistic, failure, verification, replay
}
criterion_main!(benches);
