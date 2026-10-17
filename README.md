# ORBIT

**ORBIT is a persistent event store being built toward a time-travel
debugging platform.** It records what happened to a system as ordered,
append-only, integrity-checked event streams. Later phases will reconstruct,
compare and replay historical state from those streams.

> **Status: Phase 1 of 5 is complete. That phase is the core event engine and
> persistence.** It is pre-release, the API may change, and it is not
> production-ready.

## Problem

When a system misbehaves, its current state rarely explains how it got there.
Logs are lossy, unordered across components, and easy to change after the
fact. An event store keeps every state change as an immutable, ordered fact.
With that foundation you can answer "what did this look like at step N, and
what changed?" Answering that reliably requires a trustworthy foundation: dense
ordering that never depends on wall clocks, atomic durable appends, and a way
to detect stored history that has been damaged or altered. Phase 1 builds that
foundation.

## Implemented in Phase 1

- **Streams.** Explicit creation with immutable metadata, existence checks,
  and head inspection.
- **Append-only events.** Atomic appends assign dense sequence numbers
  (1, 2, 3, …). An empty stream has head 0.
- **Optimistic concurrency.** `ExpectedSequence::Exact(n)` rejects stale
  writers with a typed `SequenceConflict`.
- **Reads.** A single event by sequence, a whole stream, or pages of events
  after a sequence, always in sequence order.
- **Durable SQLite persistence.** WAL mode, configurable durability (`Full`
  by default, including `F_FULLFSYNC` on macOS), and versioned migrations.
  Constraints and triggers enforce the invariants in the database itself.
- **Integrity verification.** Payloads are stored as canonical JSON. Each event
  carries a SHA-256 digest chained to the previous event's. `verify_stream`
  reports digest mismatches, chain breaks, gaps, malformed rows and head
  mismatches, each with its sequence number.
- **Validation and typed errors.** Every input is validated at the boundary.
  No panics on bad input or damaged files.
- **Configuration** in code or through `ORBIT_*` environment variables.
- **Tests and benchmarks.** 27 unit, 47 integration (including property-based)
  and 2 doc tests (one compiles this README's example). A Criterion benchmark suite.

## Architecture

```text
crates/orbit-core
├── domain     validated ids, Stream, AppendRequest, StoredEvent, canonical JSON
├── store      EventStore (public API, transactions) + schema/migrations (SQLite)
├── integrity  digest construction, chain verification, IntegrityReport
├── config     StoreConfig, Durability, Limits, env loading
└── error      StoreError, ValidationError
```

Only `store` knows about SQLite. Domain and integrity logic are
storage-agnostic. For component diagrams and the append data flow, see
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Technology

Rust (stable, edition 2024) · SQLite (bundled) through `rusqlite` · `serde` /
`serde_json` · `sha2` · `uuid` (v7) · `chrono` · `thiserror` · `tracing`.
Tests use `proptest` and `tempfile`. Benchmarks use `criterion`. ORBIT needs
no services, network access or paid APIs. Why `rusqlite` rather than `sqlx` is
explained in [ADR 0001](docs/adr/0001-sqlite-via-rusqlite.md).

## Getting started

Prerequisites: stable Rust (tested with 1.99.1) and a C compiler (on macOS,
`xcode-select --install`).

```bash
git clone <repository-url> ORBIT && cd ORBIT
cargo build
cargo test
cargo run -p orbit-core --example quickstart
```

### Minimal example

```rust,no_run
use orbit_core::{AppendRequest, EventStore, EventType, Metadata, Sequence, StoreConfig, StreamId};
use serde_json::json;

fn main() -> Result<(), orbit_core::StoreError> {
    let store = EventStore::open(StoreConfig::new("orbit.db"))?;

    let account = StreamId::new("account-42")?;
    store.create_stream(&account, Metadata::new())?;

    let opened = EventType::new("account.opened")?;
    let event = store.append(
        &account,
        AppendRequest::new(opened, json!({"owner": "alice"})).expect_sequence(Sequence::ZERO),
    )?;
    assert_eq!(event.sequence, Sequence::FIRST);

    for event in store.read_after(&account, Sequence::ZERO, 100)? {
        println!("#{} {} {}", event.sequence, event.event_type, event.payload);
    }

    let report = store.verify_stream(&account)?;
    assert!(report.is_valid());
    Ok(())
}
```

## Tests, lints and benchmarks

```bash
cargo test                                             # all tests
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
cargo bench -p orbit-core --bench event_store          # ~1 minute
```

[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) maps each test to what it covers. It
also has the benchmark workloads and one recorded measurement, labelled with its
hardware and conditions.

## Repository layout

```text
.
├── Cargo.toml                 workspace manifest (shared lints)
├── crates/orbit-core/
│   ├── src/                   library source
│   ├── tests/                 integration and property tests (real SQLite files)
│   ├── benches/event_store.rs Criterion benchmarks
│   └── examples/quickstart.rs runnable example
├── docs/
│   ├── ARCHITECTURE.md  EVENT_MODEL.md  STORAGE_DESIGN.md  DEVELOPMENT.md
│   └── adr/                   architecture decision records
└── .github/workflows/ci.yml   fmt, check, clippy, test, doc, bench build (Linux + macOS)
```

## Current limitations

- Single-event appends only. There is no atomic multi-event batch append yet.
- One connection per `EventStore`. Calls through one instance are serialised,
  and SQLite allows one writer per file.
- Digests are **unkeyed**. They detect corruption and naive edits, but not an
  attacker who rewrites the file and recomputes every hash. See
  [STORAGE_DESIGN.md](docs/STORAGE_DESIGN.md#integrity-verification).
- There is no stream listing, deletion or archival, and no global
  (cross-stream) ordering.
- Durability rests on SQLite's guarantees. Crash and fault-injection testing
  is planned for Phase 3.
- Local files only. The database must not be on a network file system.
- Synchronous API. The future async server will call it through `spawn_blocking`.

## Roadmap

| Phase | Scope | Status |
|-------|-------|--------|
| 1 | Core event engine and persistence | **done** |
| 2 | Historical state reconstruction and snapshots | planned |
| 3 | Crash recovery, concurrency control, integrity auditing, deterministic replay and divergence diagnostics | planned |
| 4 | Backend API (Axum) and web observability frontend | planned |
| 5 | Test hardening, reproducible benchmarks, documentation, release | planned |

Everything outside Phase 1 is planned work and does not exist yet.

## Contributing, security, license

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).
Licensed under the [MIT License](LICENSE).
