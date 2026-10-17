# ADR 0003: One library crate, concrete store, no storage trait yet

- Status: accepted
- Date: 2026-10-18

## Context

The brief suggested possible `orbit-core`, `orbit-storage` and `orbit-engine`
crates, and a storage trait "if it genuinely improves testability or enables
future storage implementations".

## Decision

Use a Cargo workspace with a single library crate, `crates/orbit-core`.
Inside it, modules separate the domain (`domain`), persistence (`store`),
integrity (`integrity`), configuration (`config`) and errors (`error`).
`EventStore` is a concrete SQLite-backed type. There is no trait.

## Rationale

- There is exactly one storage implementation, and the tests intentionally
  run against it on real files. A trait with one implementor adds indirection
  without adding testability.
- Integrity verification (`ChainVerifier`) is already storage-agnostic. It
  consumes raw records, so it can be reused by any future backend.
- The workspace layout lets Phase 2 (snapshots and reconstruction) and Phase 4
  (`orbit-api`, an Axum server) add crates without moving code.

## Consequences

- If a second backend or a test double becomes necessary, extract an
  `EventStore` trait from the public methods. They already use domain types only.
