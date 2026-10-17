# Event model

This document describes the domain model implemented in
`crates/orbit-core/src/domain/`.

## Streams

A **stream** is an ordered, append-only log of events, identified by a
`StreamId`.

| Field        | Type          | Notes                                              |
|--------------|---------------|----------------------------------------------------|
| `id`         | `StreamId`    | Primary identity. Case-sensitive.                  |
| `created_at` | `Timestamp`   | Set by the store at creation. Informational only.  |
| `metadata`   | JSON object   | Supplied at creation. Immutable afterwards.        |
| `head`       | `StreamHead`  | `{ sequence, digest }` of the last event.          |

**Lifecycle.** You create a stream explicitly with `create_stream`. Appending to a
stream that does not exist fails with `StreamNotFound`; streams are never
created implicitly. Creating a stream that already exists fails with
`StreamAlreadyExists` and leaves the original untouched. Streams cannot be
deleted or renamed, and their metadata cannot change. A stream owns its
events: events reference the stream through a foreign key, and neither can be
deleted.

## Events

| Field            | Type            | Assigned by | Notes                                         |
|------------------|-----------------|-------------|-----------------------------------------------|
| `event_id`       | `EventId`       | store or caller | UUID; ORBIT generates v7. Globally unique.  |
| `stream_id`      | `StreamId`      | caller      | Owning stream.                                |
| `sequence`       | `Sequence`      | store       | Position in the stream; authoritative order.  |
| `event_type`     | `EventType`     | caller      | For example `account.opened`.                 |
| `schema_version` | `SchemaVersion` | caller      | Payload schema version, ≥ 1, defaults to 1.   |
| `recorded_at`    | `Timestamp`     | store       | UTC, microsecond precision. Informational.    |
| `payload`        | JSON value      | caller      | Any JSON value. Stored canonically.           |
| `metadata`       | JSON object     | caller      | Defaults to `{}`. Stored canonically.         |
| `prev_digest`    | `Digest`        | store       | Digest of event `sequence - 1`, or genesis.   |
| `digest`         | `Digest`        | store       | `ORBIT-EVENT-DIGEST-V1` over the fields above.|

An `AppendRequest` carries the caller-supplied fields, plus an
`ExpectedSequence` precondition and an optional caller-chosen `event_id`.
The store returns the resulting `StoredEvent`.

## Identifiers and validation

`StreamId` and `EventType` share one grammar: 1–128 ASCII bytes from
`[A-Za-z0-9._:-]`, starting with a letter or digit. The grammar is
deliberately narrow. It keeps identifiers safe in URLs, logs and file names,
which Phase 4 will rely on, and it can be widened later without breaking
existing data.

`EventId` rejects the nil UUID. A caller-supplied id that already exists
anywhere in the store is rejected with `DuplicateEventId`. This gives callers a
basis for idempotent retries.

All domain types validate on construction **and** on deserialization
(`#[serde(try_from = ...)]`). A value of these types is therefore always
well-formed.

## Sequence numbers and ordering

- The first event in a stream has sequence **1**.
- An empty stream has head sequence **0** (`Sequence::ZERO`) and head digest
  `Digest::GENESIS` (32 zero bytes).
- Sequences are **dense**. Event `n + 1` exists only if event `n` exists.
- The maximum is `i64::MAX`, so every value fits in a SQLite integer.
  Appending beyond it fails with `SequenceExhausted`.
- **Order is defined only by `sequence`.** `recorded_at` comes from the wall
  clock. It can go backwards, two events can share it, and it is never used for
  ordering. Event ids are UUIDv7, which are roughly time-ordered, but that is
  not an ordering guarantee either.

Every read returns events in ascending sequence order. `read_after(stream,
n, limit)` returns events with `sequence > n`, so the last sequence returned
serves as a resume cursor.

## Optimistic concurrency

`ExpectedSequence::Any` (the default) always appends at the current head.
`ExpectedSequence::Exact(n)` appends only if the head is exactly `n` when
the write lock is held. `Exact(Sequence::ZERO)` means "only if the stream is
empty". A mismatch returns `SequenceConflict { expected, actual }` and writes
nothing.

## Payloads, metadata and size limits

Payloads may be any JSON value. Metadata must be a JSON object. Both are
converted to **canonical JSON** before size checks and storage:

- no insignificant whitespace;
- object keys sorted bytewise (UTF-8);
- strings escaped like `serde_json` does (`"`, `\` and control characters);
- numbers rendered by `serde_json`: integers as plain decimal, floats in the
  shortest form that round-trips.

Limits apply to the canonical byte length. The defaults are **1 MiB** for a
payload and **64 KiB** for metadata. They are configurable up to a hard cap of
16 MiB (see `StoreConfig`). Oversized input is rejected with
`PayloadTooLarge` or `MetadataTooLarge` before any lock is taken.

Integers outside the `i64`/`u64` range become `f64` during parsing, which loses
precision. Callers who need exact large numbers should encode them as JSON strings.

## Integrity metadata

Each event stores `prev_digest` and `digest`. Together they form a SHA-256
hash chain per stream, and the stream row stores the head digest. The exact
construction and what it can and cannot detect are specified in
[STORAGE_DESIGN.md](STORAGE_DESIGN.md#integrity-verification).
