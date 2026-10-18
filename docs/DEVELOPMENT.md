# Development

## Prerequisites

- Stable Rust. The project was developed and tested with **rustc 1.99.1** on
  macOS (Apple Silicon), and CI runs the current stable on Linux and macOS.
  The workspace uses edition 2024, so Rust ≥ 1.85 is needed. No lower
  supported version (MSRV) has been verified.
- A C compiler. The `rusqlite` `bundled` feature compiles SQLite. On macOS
  that is Xcode Command Line Tools (`xcode-select --install`).
- Nothing else. No database server, network service or paid API is needed.

## Web prerequisites

Node.js ≥ 20.9 and npm. Tested with Node 24.21 and npm 11.19. The web app's
dependencies are pinned by `web/package-lock.json`. The end-to-end test needs
Playwright's Chromium once: `cd web && npx playwright install chromium`.

## Everyday commands

```bash
cargo build                                   # build
cargo test                                    # unit, integration, property and doc tests
cargo fmt --all                               # format
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps --open        # API docs
cargo run -p orbit-core --example quickstart  # end-to-end example (event store)
cargo run -p orbit-core --example time_travel # reconstruction, snapshots, diff
cargo run -p orbit-core --example crash_lab   # verification, checkpoints, replay, divergence
cargo run -p orbit-lab -- implementations     # lab: registered implementations
cargo run -p orbit-lab -- run --stream S --left orbit.counter --right example.counter-floor-at-zero
cargo run -p orbit-lab -- trace <id>          # per-boundary digests
cargo run --release -p orbit-lab --example large_history -- 100000   # one large run, prints sizes and time
cargo run -p orbit-lab --features reliability --bin orbit-reliability -- run event-append-interruption   # see RELIABILITY.md
```

The lab reads `data/orbit.db` and keeps its registry in `data/lab` (override
with `--db`/`ORBIT_DB_PATH` and `--lab`/`ORBIT_LAB_DIR`). See [LAB.md](LAB.md)
for every command, the result format and how to reproduce a result.

Observatory:

```bash
cargo run -p orbit-api                       # API on 127.0.0.1:7878, database data/orbit.db
cd web && npm run dev                        # UI on localhost:3000
scripts/seed-demo.sh                         # demonstration data, through the API
cd web && npm run format:check && npm run lint && npm run typecheck && npm test && npm run build
cd web && npm run test:e2e                   # starts its own API and production web server on ports 7879/3100
```

CI (`.github/workflows/ci.yml`) has three jobs:

1. **Rust** (Ubuntu and macOS): everything below, for all three crates.
2. **Web**: Prettier, ESLint with zero warnings, `tsc`, Vitest, and the
   production build.
3. **End-to-end**: builds `orbit-api` and checks that the binary contains no
   reliability child code, then installs Chromium and runs the Playwright
   workflows against a fresh event store and lab registry.

The Rust job runs, with `--locked`, on Ubuntu and macOS:

- `fmt --check` and `check`;
- `clippy --all-targets --all-features`, plus `clippy --lib` with default
  features (the production build, without fault hooks);
- `test` and `doc`;
- a check that `orbit-api`'s feature graph contains neither
  `orbit-core/fault-injection` nor `orbit-lab/reliability`;
- `bench --no-run`, all three examples and `orbit-lab implementations`.

On Ubuntu it also runs every benchmark once, in Criterion's `--test` mode. That
is a smoke test, not a measurement.

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
| `tests/fault_injection.rs`  | Every fault point: no committed state after a failure, the connection keeps working, the store verifies, reopen preserves committed data; snapshot and replay persistence faults; intermittent fault schedules |
| `tests/crash_recovery.rs`   | Child processes that `abort()` inside appends and snapshot saves, or are `SIGKILL`ed while appending; recovery on reopen |
| `tests/stress.rs`           | Mixed multi-connection workload (blind writers, optimistic writers with retry, readers, snapshotter) and optimistic writers in separate processes, with invariant checks |
| `tests/integrity_diagnostics.rs` | Categories and affected event ids, invalid genesis, swapped events, inconsistent empty head, checkpoints versus a recomputed rewrite and versus truncation, `verify_store` over damaged events, snapshots, runs and triggers, foreign-key violations |
| `tests/replay.rs`           | Recording, persistence and reopen, determinism, snapshot and reconstructed starts, refusal of damaged inputs, and every outcome: exact, reconverged, diverged (first event localised), projection failure, version mismatch, invalid input, altered expectation, damaged history and boundary, truncated history, corrupt start snapshot (warning), experiments |
| `tests/replay_properties.rs` | Proptest: replay is exact and deterministic; with a drifting projection, the first divergence is exactly the first affected event; random fault schedules never commit failed appends |
| `tests/summaries.rs`        | Stream summaries, store statistics, recent events (used by the API) |
| `orbit-api` `tests/api.rs`  | HTTP contract: every endpoint's success and error responses, validation, pagination bounds, conflicts, payload and body limits, projections, snapshots (including tampering), integrity success and failure, replay outcomes, experiments, overview, restarts, benchmarks, CORS |
| `orbit-api` `tests/server.rs` | Real TCP server: concurrent optimistic appends over HTTP (exactly one winner), no duplicate sequences, persistence across a server restart |
| `orbit-api` `tests/lab.rs`  | Lab endpoints: implementations, create/inspect/verify/reproduce, persistence across a restart, failed experiments, validation (unknown fields, kinds, ids), pagination, disabled lab, Host guard, no paths in responses, no lab tables in the event store |
| `orbit-lab` `tests/differential.rs` | Equivalent, diverged (first divergence with event and diff), reconverged and multi-segment results, empty and single-event histories, divergence at boundary 0, missing/short/damaged histories, rejections and panics, nondeterminism (detected, declared, unchecked), canonical comparison, windows, agreement with the core replay trace, reproduction (pinned range, restart, missing implementation, damaged or different history) |
| `orbit-lab` `tests/registry.rs` | Initialization and refusal of foreign or newer files, lifecycle transitions and refusals, triggers against raw SQL, persistence across restarts, pagination, concurrent registries, interrupted-run recovery with live and dead locks, identity changes, artifact verification (corrupted, missing), tampered records, atomic deduplicating writes, symlinks |
| `orbit-lab` `tests/identity.rs` | Same name and version with different implementations, fingerprint stability under equivalent configuration, every identity field changing the fingerprint, the documented code-body limitation, id validation, duplicate ids, JSON round trip, spec digest canonicalization |
| `orbit-lab` `tests/properties.rs` | Proptest: self-comparison is equivalent and its trace equals `ReplayLab::record`; the floor variant first diverges exactly where the counter first goes negative; for random histories, two variants and any window, every interval, convergence, count, trace entry and entry diff equals an independent reference (direct folds, `serde_json::Value` equality, a naive interval scan), and the result reproduces |
| `orbit-lab` `tests/timeline.rs` | Divergence at the first and the final event, entry/exit diffs and convergence events, reference perspective, metadata keys without values, nested JSON with nulls, arrays and numbers (against the reference), a 3,000-event history with 300 intervals paged through the trace, trace windows and bounds, intervals stopped by failures, identity completeness, artifact write failures |
| `orbit-lab` `tests/reliability.rs` | Every reliability scenario end to end with real child processes (aborts at each core and lab fault point, `SIGKILL`, a paused live owner, 4 processes creating, racing to start and racing to recover), persistence across reopen, reproduction without modifying the original, infrastructure failures (no child binary), v3 schema constraints, the `orbit-reliability` tool's validation, no active hook in ordinary runs |
| `orbit-api` `tests/lab.rs` (Sprint 9 additions) | The reliability catalog is listed; `kind: reliability` is rejected; no route runs scenarios; a timed-out experiment request still completes |
| `orbit-lab` `tests/reproduction.rs` | Reproduction: identical, changed result (`differs` with what changed), tampered spec and other comparison rules (`configuration_mismatch`), nondeterministic results, missing implementation, changed or short history, later appends; a real Sprint 7 registry fixture migrated, read, verified and reproduced |
| `orbit-lab` `tests/cli.rs`  | The `orbit-lab` binary: implementations, run (flags and `--spec`), list, show, verify, reproduce, error exits |
| `web/tests/unit`            | API client (URLs, bodies, error envelope, network errors, timing header), formatters, DiffView, ErrorState and EmptyState, navigation, the Overview page's loading, empty, data, error/retry and unrun-check states |
| `web/tests/e2e`             | The complete workflow in Chromium, with every step checked in the UI and through the API; a differential replay run, inspected, verified and reproduced from the Experiments page, which must fit a 375 px screen |
| `tests/reconstruction_properties.rs` | Proptest: snapshot-assisted equals full replay equals reference fold (counter and inventory, varied snapshot intervals and targets), the newest eligible snapshot is selected, deterministic diffs |

Every integration test uses a real SQLite file in its own temporary directory.
The test suite builds `orbit-core` with the `fault-injection` feature, through
a self dev-dependency in `crates/orbit-core/Cargo.toml`. Library builds do not
include it. `crash_recovery.rs` and `stress.rs` re-invoke their own test
binary as child processes, through the `crash_child_entrypoint` and
`stress_child_entrypoint` tests. Those return immediately in a normal run.
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

## Phase 3 benchmarks

```bash
cargo bench -p orbit-core --bench crash_lab
cargo bench -p orbit-core --bench crash_lab -- replay/   # filter
```

Each group uses its own database (`Durability::Normal`), built outside the
measured code.

| Benchmark | Dataset | Measures |
|-----------|---------|----------|
| `concurrency/4_connections_x_100_appends` | fresh DB per iteration (setup unmeasured) | 4 threads, each with its own connection, appending 100 events to one stream |
| `concurrency/1_connection_x_400_appends` | same | the same 400 appends from one connection |
| `optimistic/append_expected_head_success` | 100 events, growing | `append` with `Exact(head)` that succeeds |
| `optimistic/append_stale_head_conflict` | 100 events | `append` with a stale `Exact` that is rejected |
| `failure/append_committed` | 100 events, growing; hook installed, never fires | baseline append |
| `failure/append_rolled_back_before_commit` | 100 events; hook fails at `AppendBeforeCommit` | the full append work plus a rollback |
| `verify/verify_stream_10k` | 10,000 events + 10 snapshots | `verify_stream` |
| `verify/verify_store_10k_events_10_snapshots` | same | `verify_store` (includes `PRAGMA integrity_check`) |
| `replay/record_1k_from_initial` | 1,000 events | `ReplayLab::record` (verify interval + fold + trace + insert) |
| `replay/execute_1k_from_initial` | same | `execute`: exact match, persisted |
| `replay/execute_100_from_snapshot_at_900` | same + snapshot at 900 | `execute` of a run starting from the snapshot |

### Recorded measurement (Phase 3)

These are from one run on 2026-10-18: Apple M3, 8 GB RAM, macOS 27.0.1,
rustc 1.99.1, release profile, built with the `fault-injection` feature (as
all benchmarks are, through the dev-dependency). Medians:

| Benchmark | Median |
|-----------|--------|
| `concurrency/4_connections_x_100_appends` | 48.8 ms |
| `concurrency/1_connection_x_400_appends` | 32.3 ms |
| `optimistic/append_expected_head_success` | 73.0 µs |
| `optimistic/append_stale_head_conflict` | 7.33 µs |
| `failure/append_committed` | 71.3 µs |
| `failure/append_rolled_back_before_commit` | 38.7 µs |
| `verify/verify_stream_10k` | 11.4 ms |
| `verify/verify_store_10k_events_10_snapshots` | 28.1 ms |
| `replay/record_1k_from_initial` | 2.78 ms |
| `replay/execute_1k_from_initial` | 2.68 ms |
| `replay/execute_100_from_snapshot_at_900` | 397 µs |

What these numbers show:

- **More writer connections do not add write throughput.** SQLite allows one
  writer per file, so four competing connections were about 50% *slower* than
  one doing the same 400 appends. The extra time goes to lock hand-offs and
  busy waits. Concurrency here buys safety, not speed.
- A rejected optimistic append costs about a tenth of a successful one. The
  conflict is detected right after the head read, before any write.
- A failure just before commit costs less than a commit: the work is done,
  but there is no WAL commit or sync.
- `verify_store` costs about 2.5× `verify_stream` on this dataset. The
  difference is SQLite's full `integrity_check` plus snapshot validation.
- Replay costs about 2.7 µs per event, against about 0.6 µs per event for plain
  reconstruction (Phase 2 table). The difference is the interval verification
  plus serializing and hashing the state after every event, which the trace
  requires. Starting from a snapshot near the end reduces it proportionally.
- `failure/append_committed` (71 µs) is higher than Phase 1's `append/normal`
  (45 µs). These come from different benchmark binaries and runs, with
  different stream sizes and payloads, so the two numbers are not comparable.
  The cause has not been isolated.

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
- **Write paths.** Keep `self.fault(FaultPoint::…)?` calls at the documented
  boundaries, and add a point (plus a `tests/fault_injection.rs` case) for
  any new transaction.
- **Replay.** Changing the state digest, the run digest or the trace format
  needs a new domain tag. Existing runs would otherwise be reported as
  integrity failures.
- **New dependencies.** Prefer the existing stack. Record significant
  additions in an ADR.

## Configuration

The API's own variables (`ORBIT_API_ADDR`, `ORBIT_API_ALLOW_REMOTE`,
`ORBIT_API_CORS_ORIGINS`, `ORBIT_BENCHMARK_DIR`) are listed in
[API.md](API.md#running). The web app reads `NEXT_PUBLIC_ORBIT_API_URL`
(default `http://127.0.0.1:7878/api/v1`; see `web/.env.example`). It is
baked in at build time.

Store settings:

`StoreConfig::from_env()` reads:

| Variable                   | Default | Meaning |
|----------------------------|---------|---------|
| `ORBIT_DB_PATH`            | required | Database file path |
| `ORBIT_DURABILITY`         | `full`  | `full` or `normal` |
| `ORBIT_BUSY_TIMEOUT_MS`    | `5000`  | Lock wait before `Busy` (max 600000) |
| `ORBIT_MAX_PAYLOAD_BYTES`  | `1048576` | Canonical payload limit (1 to 16 MiB) |
| `ORBIT_MAX_METADATA_BYTES` | `65536` | Canonical metadata limit (1 to 16 MiB) |

Invalid values produce a `ConfigError` that names the variable.
