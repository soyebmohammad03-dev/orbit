# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-10-18

The first tagged release, a pre-1.0 release. It contains all five planned
phases. See the README's "Release status" section for why this is not 1.0.0.

### Added — Phase 5: verification, hardening and release

- `orbit-api` `Host` allowlist (DNS-rebinding defence): only loopback names by
  default, extendable with `ORBIT_API_ALLOWED_HOSTS`. Requests without a `Host`
  are refused.
- A per-request deadline (`ORBIT_API_REQUEST_TIMEOUT_SECS`, default 30 s). It
  returns `503 request_timeout`, documented as possibly-completed for writes.
- A warning log when listening on a non-loopback address.
- `tests/hardening.rs`: Host allowlist, refusal of simple cross-site posts,
  narrow CORS preflight, a deterministic timeout test, and no local paths in
  errors.
- CI:
  - clippy with `--all-features`, plus a default-features library lint;
  - every benchmark run once in Criterion `--test` mode.

### Fixed — Phase 5

- The web pages overflowed horizontally on narrow screens. All eight pages now
  fit at 375, 768 and 1440 px, and mobile navigation wraps.
- The Replay Lab's "Execute with" list was hard-coded. It now lists the
  projections the API reports.

### Added — Phase 4: Observatory

- `crates/orbit-api`: an Axum HTTP API at `/api/v1`.
  - Endpoints for health, capabilities, overview, streams and events
    (cursor pagination, optimistic concurrency), state reconstruction, diffs,
    explanations, snapshots with validation, checkpoints, stream and store
    verification, replay runs, executions, experiments, and Criterion results.
  - A structured error envelope and an OpenAPI 3.1 document (utoipa).
  - CORS, body limits, `Server-Timing`, and localhost-only binding by default.
- `web/`: the ORBIT Observatory, in Next.js 16 App Router with strict
  TypeScript and Tailwind CSS 4. Pages: Overview, Streams, Events, Time
  Travel, Snapshots, Replay Lab, Integrity and Benchmarks, all backed by the
  real API.
- `orbit-core`:
  - the `projection::MergePatch` projection (`merge-patch@v1`, RFC 7386);
  - `EventStore::{stream_summaries, stream_summary, statistics, recent_events, latest_snapshot_sequence}`;
  - `Serialize` for `SnapshotConflict`.
- Tests: API contract and TCP tests, web unit and component tests (Vitest),
  and a Playwright end-to-end workflow. CI jobs for web checks and the
  end-to-end test.
- `scripts/seed-demo.sh`, `docs/API.md`, `docs/OBSERVATORY.md`, ADR 0008, and
  genuine screenshots.

### Changed

- The history of `main` was rewritten once, at the owner's request, to remove
  AI co-author trailers from the Phase 1 and Phase 2 commit messages. Trees,
  authors and dates are unchanged.

### Added — Phase 3: crash lab

- Fault injection: `fault::FaultPoint` at every write-path boundary, and a
  per-store hook (`EventStore::set_fault_hook`) behind the off-by-default
  `fault-injection` feature.
- Recovery tests with real child-process crashes (`abort()` inside
  transactions, `SIGKILL` during appends), and fault-injection tests for
  every point.
- Concurrency stress tests: a mixed multi-connection workload, and optimistic
  writers in separate processes, with invariant checks.
- `EventStore::verify_store` (SQLite integrity and foreign-key checks,
  protective triggers, streams, snapshots, replay runs), `list_streams`,
  `checkpoint` and `verify_stream_against` (trusted checkpoints).
- `IntegrityCategory`, `IntegrityIssue::{InvalidGenesis, CheckpointMismatch}`,
  `IntegrityReport::affected_events`, and `StoreError::class()` / `ErrorClass`.
- Schema migration 3: `replay_runs` and `replay_executions`.
- `replay::ReplayLab`: record, execute, re-execute, inspect, and `experiment`.
  `ReplayOutcome` covers exact match, reconverged, diverged (with the first
  divergence point and final diff), projection failed, unavailable, invalid
  input and integrity failure.
- A `crash_lab` benchmark suite, the `crash_lab` example, `RECOVERY.md`,
  `REPLAY.md`, and ADRs 0006 and 0007.

### Changed

- Schema version 3. Version 1 and 2 databases are upgraded in place, and
  tests cover both from real fixtures.
- `IntegrityReport` gained `affected_events`. Event 1 with a non-genesis
  `prev_digest` is now reported as `InvalidGenesis` rather than `ChainBroken`.

### Added — Phase 2: historical state reconstruction and snapshots

- `Projection` trait with typed `ProjectionError`s, `decode_payload`, and the
  example projections `Counter` and `Inventory`.
- `Reconstructor`: reconstruct state at `Target::Latest` or `Target::At(n)`
  through bounded, continuity-checked folds. Failures are all-or-nothing and
  carry their context. Serializable `ReconstructionReport` provenance.
- Snapshots: schema migration 2 (`snapshots` table, history-binding and
  immutability triggers), `ORBIT-SNAPSHOT-DIGEST-V1`, and `EventStore::{save_snapshot,
  snapshot, list_snapshots, check_snapshot}`. Saving is idempotent, and
  conflicting states are rejected.
- Snapshot-assisted reconstruction with `SnapshotPolicy::{Ignore, Prefer,
  Strict, Exact}`. Invalid snapshots are reported, and
  `compare_with_full_replay` checks equivalence.
- Structural JSON diff (RFC 6901 paths, deterministic order),
  `Reconstructor::diff` between historical positions, and per-event
  `Reconstructor::explain`.
- Phase 2 tests, including property tests and a v1 → v2 migration test from a
  real Phase 1 fixture. A `reconstruction` Criterion benchmark suite, and the
  `time_travel` example.

### Changed

- The schema version is now 2. Phase 1 databases are upgraded in place when opened.
- `StoreError` gained the snapshot variants. `ValidationError` gained
  `InvalidProjectionVersion`, `NilSnapshotId`, `SnapshotAtZero` and
  `SnapshotTooLarge`.

### Added — Phase 1: core event engine and persistence

- `orbit-core` crate with validated domain types: `StreamId`, `EventType`,
  `EventId` (UUIDv7), `SchemaVersion`, `Sequence`, `Timestamp`, `Digest`.
- SQLite-backed `EventStore`: create and inspect streams, append events
  atomically with optional optimistic concurrency (`ExpectedSequence`), read
  single events, whole streams, and pages after a sequence number.
- Schema version 1 with `STRICT` tables, constraints and triggers that keep
  streams append-only and sequences dense and hash-chained.
- Canonical JSON storage for payloads and metadata.
- `ORBIT-EVENT-DIGEST-V1` per-event SHA-256 digests chained per stream, and
  `EventStore::verify_stream` integrity reports.
- Configuration through `StoreConfig` or `ORBIT_*` environment variables.
- Unit, integration and property-based tests; Criterion benchmarks; CI.
