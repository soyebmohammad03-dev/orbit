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
cargo run -p orbit-core --example quickstart  # end-to-end example
```

CI (`.github/workflows/ci.yml`) runs `fmt --check`, `check`, `clippy`, `test`,
`doc`, `bench --no-run` and the example, with `--locked`, on Ubuntu and macOS.

## Tests

| File                        | Covers |
|-----------------------------|--------|
| `src/**` unit tests         | Identifier grammar, sequences, timestamps, digests, canonical JSON, config, golden digest vector |
| `tests/streams.rs`          | Stream creation, duplicates, metadata, missing streams |
| `tests/persistence.rs`      | Append, point and range reads, paging, empty streams, reopen, size limits, canonical storage, event ids |
| `tests/constraints.rs`      | Schema triggers and constraints, using raw SQL that bypasses ORBIT |
| `tests/concurrency.rs`      | Multi-thread and multi-connection appends, optimistic conflicts, `Busy`, rollback |
| `tests/integrity.rs`        | Tamper detection on real damaged files, malformed rows, the unkeyed-digest limitation |
| `tests/schema.rs`           | Migration bookkeeping, refusal of foreign or newer files |
| `tests/properties.rs`       | Proptest: canonical JSON round trip, append/read/reopen preserves content and order, paging, tamper detection |

Every integration test uses a real SQLite file in its own temporary directory.
No test sleeps for correctness. The `Busy` test uses a bounded 50 ms busy
timeout against a lock held explicitly by the test.

Increase property-test effort with, for example,
`PROPTEST_CASES=1000 cargo test --test properties`.

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
