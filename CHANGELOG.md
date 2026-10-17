# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
