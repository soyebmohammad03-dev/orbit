# Development

## Prerequisites

- Stable Rust. The project was developed and tested with **rustc 1.99.1** on
  macOS (Apple Silicon), and CI runs the current stable on Linux and macOS.
  The workspace uses edition 2024, so Rust ≥ 1.85 is needed. No lower
  supported version (MSRV) has been verified.
- A C compiler. The `rusqlite` `bundled` feature compiles SQLite. On macOS
  that is Xcode Command Line Tools (`xcode-select --install`).
- Nothing else. No database server, network service or paid API is needed.

## Everyday commands

```bash
cargo build                                   # build
cargo test                                    # unit, integration, property and doc tests
cargo fmt --all                               # format
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps --open        # API docs
cargo run -p orbit-core --example quickstart  # end-to-end example (event store)
cargo run -p orbit-core --example time_travel # reconstruction, snapshots, diff
```

CI (`.github/workflows/ci.yml`) runs `fmt --check`, `check`, `clippy`, `test`,
`doc`, `bench --no-run` and both examples, with `--locked`, on Ubuntu and macOS.

## Tests

| File                        | Covers |
|-----------------------------|--------|
| `src/**` unit tests         | Identifier grammar, sequences, timestamps, digests, canonical JSON, config, golden event and snapshot digest vectors, structural diff, example projections |
| `tests/streams.rs`          | Stream creation, duplicates, metadata, missing streams |
| `tests/persistence.rs`      | Append, point and range reads, paging, empty streams, reopen, size limits, canonical storage, event ids |
| `tests/constraints.rs`      | Schema triggers and constraints, using raw SQL that bypasses ORBIT |
| `tests/concurrency.rs`      | Multi-thread and multi-connection appends, optimistic conflicts, `Busy`, rollback |
| `tests/integrity.rs`        | Tamper detection on real damaged files, malformed rows, the unkeyed-digest limitation |
| `tests/schema.rs`           | Migration bookkeeping, refusal of foreign or newer files, v1 → v2 upgrade from a real Phase 1 fixture |
| `tests/properties.rs`       | Proptest: canonical JSON round trip, append/read/reopen preserves content and order, paging, tamper detection |
| `tests/reconstruction.rs`   | Empty streams, latest and historical targets, sequence 0, beyond-head targets, ordering, projection failures with context, no partial results, multi-page folds, broken-history detection |
| `tests/snapshots.rs`        | Creation, retrieval, listing, reopen, idempotence and conflicts, history binding, every validation outcome, irregular intervals, never-after-target, `Exact` / `Prefer` / `Strict`, corrupt-snapshot fallback, incompatible versions, full-replay equivalence, appends during snapshot creation (with a concurrent writer) |
| `tests/state_diff.rs`       | Diffs between historical positions, additions and removals, nested fields, arrays, reversed diffs, identical states, empty or invalid histories, projection errors in the interval, `explain` attribution |
| `tests/reconstruction_properties.rs` | Proptest: snapshot-assisted equals full replay equals reference fold (counter and inventory, varied snapshot intervals and targets), the newest eligible snapshot is selected, deterministic diffs |

Every integration test uses a real SQLite file in its own temporary directory.
No test sleeps for correctness. The `Busy` test uses a bounded 50 ms busy
timeout against a lock held explicitly by the test.

Increase property-test effort with, for example,
`PROPTEST_CASES=1000 cargo test --test properties`. Phase 2's property suite
was also run once with `PROPTEST_CASES=400`, and passed.

## Benchmarks

```bash
cargo bench -p orbit-core --bench event_store
cargo bench -p orbit-core --bench event_store -- read/   # filter
```

Each benchmark builds its dataset in a fresh temporary directory, outside the
measured code. The workloads are:

| Benchmark                     | Dataset | Measures |
|-------------------------------|---------|----------|
| `append/normal`               | growing stream | one append with `Durability::Normal` |
| `append/full`                 | growing stream | one append with `Durability::Full` (fsync / F_FULLFSYNC per commit) |
| `read/event_by_sequence/10k`  | 10,000 events | point lookup, pseudo-random sequence |
| `read/batch_after/100_of_10k` | 10,000 events | `read_after` page of 100 |
| `scan/read_stream/1k`         | 1,000 events  | full stream read and decode |
| `scan/verify_stream/1k`       | 1,000 events  | full integrity verification |

Payloads are about 150 bytes of canonical JSON. A full run takes about a minute.

### Recorded measurement

This is one run, recorded to give a sense of scale. It is not a performance
guarantee. Results vary with hardware, storage, thermal state and background load.

- Machine: Apple M3, 8 GB RAM, internal SSD, macOS 27.0.1
- Toolchain: rustc 1.99.1, `cargo bench` (release profile), bundled SQLite
- Date: 2026-10-18, commit of Phase 1

| Benchmark                     | Median time | Throughput |
|-------------------------------|-------------|------------|
| `append/normal`               | 44.9 µs     | ~22.3 K events/s |
| `append/full`                 | 3.60 ms     | ~278 events/s |
| `read/event_by_sequence/10k`  | 6.92 µs     | ~144 K lookups/s |
| `read/batch_after/100_of_10k` | 96.6 µs     | ~1.04 M events/s |
| `scan/read_stream/1k`         | 825 µs      | ~1.21 M events/s |
| `scan/verify_stream/1k`       | 997 µs      | ~1.00 M events/s |

The `append/full` cost is dominated by `F_FULLFSYNC`, the price of
power-loss durability on macOS. Applications that need higher write
throughput under `Full` will need batched appends. Those are not implemented yet.

## Phase 2 reconstruction benchmarks

```bash
cargo bench -p orbit-core --bench reconstruction
cargo bench -p orbit-core --bench reconstruction -- snapshot_create   # filter
```

Each group builds its datasets in fresh temporary databases
(`Durability::Normal`), outside the measured code. Full replay and
snapshot-assisted runs use the same database and target, so they are directly
comparable.

| Benchmark | Dataset | Measures |
|-----------|---------|----------|
| `reconstruct/counter_10k/full_replay` | 10,000 counter events | `Latest`, `SnapshotPolicy::Ignore` |
| `reconstruct/counter_10k/snapshot_at_9000` | same + snapshot at 9,000 | `Latest`, `Prefer` (1,000 events applied) |
| `reconstruct/counter_10k/snapshot_at_head` | same + snapshot at 10,000 | `Latest`, `Prefer` (0 events applied) |
| `reconstruct/counter_10k/historical_5000_full_replay` | 10,000 counter events | `At(5000)`, `Ignore` |
| `reconstruct/counter_100/full_replay` | 100 counter events | `Latest`, `Ignore` |
| `reconstruct/counter_100/prefer_no_snapshot_exists` | same, no snapshots | `Latest`, `Prefer` (cost of the candidate query) |
| `reconstruct/counter_100/snapshot_at_90` | same + snapshot at 90 | `Latest`, `Prefer` |
| `snapshot_create/counter_10k/from_full_replay` | 10,000 events, +1 per iteration | `create_snapshot(Latest, Ignore)` |
| `snapshot_create/counter_10k/incremental_from_previous_snapshot` | 10,000 events + snapshot, +1 per iteration | `create_snapshot(Latest, Prefer)` |
| `diff/inventory_2k/1000_to_2000/full_replay` | 2,000 inventory events, 50 SKUs | `diff(1000, 2000)`, `Ignore` |
| `diff/inventory_2k/1000_to_2000/snapshot_at_1000` | same + snapshot at 1,000 | `diff(1000, 2000)`, `Prefer` |
| `diff/inventory_2k/explain_1900_to_2000` | same | `explain(1900, 2000)`, `Prefer` |

In the `snapshot_create` benchmarks, an unmeasured setup step appends one event
before each iteration, so every iteration creates a genuinely new snapshot.
Each of these benchmarks has its own database. An earlier draft shared one
database, and the incremental benchmark grew the stream before the
full-replay one ran, which inflated its result about 5×. That run was discarded.

### Recorded measurement (Phase 2)

These are from one run, under the same conditions as the Phase 1 table above:
Apple M3, 8 GB RAM, macOS 27.0.1, rustc 1.99.1, release profile,
2026-10-18. Medians:

| Benchmark | Median |
|-----------|--------|
| `reconstruct/counter_10k/full_replay` | 6.02 ms |
| `reconstruct/counter_10k/snapshot_at_9000` | 615 µs |
| `reconstruct/counter_10k/snapshot_at_head` | 15.4 µs |
| `reconstruct/counter_10k/historical_5000_full_replay` | 3.04 ms |
| `reconstruct/counter_100/full_replay` | 72.9 µs |
| `reconstruct/counter_100/prefer_no_snapshot_exists` | 84.1 µs |
| `reconstruct/counter_100/snapshot_at_90` | 32.3 µs |
| `snapshot_create/counter_10k/from_full_replay` | 6.35 ms |
| `snapshot_create/counter_10k/incremental_from_previous_snapshot` | 67.5 µs |
| `diff/inventory_2k/1000_to_2000/full_replay` | 1.57 ms |
| `diff/inventory_2k/1000_to_2000/snapshot_at_1000` | 850 µs |
| `diff/inventory_2k/explain_1900_to_2000` | 3.16 ms |

What these numbers show:

- Full replay is linear in the events applied: about 0.6 µs per counter event,
  dominated by reading and decoding rows.
- A snapshot near the head turned a 6 ms reconstruction into 0.6 ms. A snapshot
  at the head took 15 µs, which is mostly validation: one digest, one event
  lookup and one state decode.
- On a 100-event stream, a snapshot at 90 was still about 2× faster. With *no*
  snapshot, `Prefer` cost about 15% more than `Ignore`, because of the
  candidate query. For tiny streams, `Ignore` is the cheaper choice.
- Creating a snapshot costs about one reconstruction plus a small insert.
- `explain` serializes and diffs the whole state after every event. Most of the
  3.16 ms here is reconstructing the base (900 events from the snapshot at
  1,000) plus 100 serialize-and-diff steps on a 50-SKU state.

## Changing things safely

- **Domain types.** Keep validation in the constructor and route serde
  through it (`try_from`). Add unit tests for both the accepted and the
  rejected forms.
- **Schema.** Append a new migration to `MIGRATIONS` in `store/schema.rs`.
  Never edit an existing one. Add a test in `tests/schema.rs` that opens a
  file created at the previous version.
- **Append path.** Keep every read and write of the head inside the
  `BEGIN IMMEDIATE` transaction. Run `tests/concurrency.rs` repeatedly
  (`cargo test --test concurrency -- --test-threads=1` in a loop) after
  touching it.
- **Projections.** Bump the `ProjectionKey` version whenever the same events
  could produce a different state or the state's shape changes. Otherwise
  existing snapshots will be reused incorrectly.
  `compare_with_full_replay` and `tests/reconstruction_properties.rs` are the
  safety net.
- **Reconstruction.** Keep the fold bounded (`read_after` pages), keep the
  continuity checks, and never return partial state.
- **Digest construction.** Do not change it in place. Introduce a new domain
  tag and a versioned upgrade path. The golden-vector test will fail if you
  change it by accident.
- **New dependencies.** Prefer the existing stack. Record significant
  additions in an ADR.

## Configuration

`StoreConfig::from_env()` reads:

| Variable                   | Default | Meaning |
|----------------------------|---------|---------|
| `ORBIT_DB_PATH`            | required | Database file path |
| `ORBIT_DURABILITY`         | `full`  | `full` or `normal` |
| `ORBIT_BUSY_TIMEOUT_MS`    | `5000`  | Lock wait before `Busy` (max 600000) |
| `ORBIT_MAX_PAYLOAD_BYTES`  | `1048576` | Canonical payload limit (1 to 16 MiB) |
| `ORBIT_MAX_METADATA_BYTES` | `65536` | Canonical metadata limit (1 to 16 MiB) |

Invalid values produce a `ConfigError` that names the variable.
