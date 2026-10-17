# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
