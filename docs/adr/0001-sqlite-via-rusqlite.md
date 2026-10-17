# ADR 0001: SQLite through `rusqlite`, synchronous API

- Status: accepted
- Date: 2026-10-18

## Context

ORBIT needs a durable, embedded, zero-cost local store with transactions.
SQLite fits. The two mainstream Rust integrations are `sqlx` (async,
connection pool, compile-time-checked queries) and `rusqlite` (synchronous,
thin binding). The project brief lists `sqlx` as the preferred choice unless
there is a documented technical reason to pick something else.

## Decision

Use `rusqlite` with the `bundled` feature, and expose a synchronous
`EventStore` API.

## Rationale

1. **Precise transaction control.** Correct sequence allocation depends on
   taking SQLite's write lock *before* reading the stream head
   (`BEGIN IMMEDIATE`). `rusqlite` exposes this directly with
   `transaction_with_behavior(Immediate)`. With `sqlx` this needs raw
   `BEGIN IMMEDIATE` statements outside its transaction type.
2. **SQLite is synchronous anyway.** `sqlx`'s SQLite driver runs every call on
   a background worker thread. An async facade adds a thread hop and a runtime
   dependency without adding concurrency, because SQLite allows only one
   writer at a time.
3. **Deterministic SQLite version.** `bundled` compiles a known SQLite
   (with JSON functions and `STRICT` tables) on every platform, instead of
   depending on whatever the OS ships.
4. **Footprint.** No Tokio and no proc-macro query checking. That keeps
   builds lighter on low-memory machines, which is one of the project's targets.

## Consequences

- Phase 1 needs no async runtime. The Phase 4 Axum API will call the store
  through `tokio::task::spawn_blocking`, or a dedicated blocking thread pool.
- `EventStore` holds one connection behind a `Mutex`, so calls through one
  instance are serialised. Read concurrency can be added later with a small
  pool of read-only connections (WAL allows concurrent readers) without
  changing the public API.
- `rusqlite` types do not appear in the public API. Driver errors are wrapped
  in the opaque `DatabaseError`.
