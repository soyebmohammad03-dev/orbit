# Architecture

Phases 1 and 2 deliver one library crate, `orbit-core`, inside a Cargo
workspace (see [ADR 0003](adr/0003-single-crate-concrete-store.md)). Phase 1
is the event store. Phase 2 adds projections, historical reconstruction,
snapshots and state diffs on top of it.

## Components

```mermaid
flowchart TB
    caller["Application / future orbit-api (Phase 4)"]
    subgraph orbit-core
        recon["reconstruct::Reconstructor&lt;P&gt;<br/>targets, snapshot selection, fold, diff, explain"]
        projection["projection<br/>Projection trait, examples"]
        diff["diff<br/>structural JSON diff"]
        store["store::EventStore<br/>events + snapshots, transactions"]
        schema["store::schema<br/>connection setup, migrations"]
        domain["domain<br/>validated types, canonical JSON"]
        integrity["integrity<br/>event digests, ChainVerifier"]
        snapshot["snapshot<br/>snapshot types, digest, problems"]
        record["record (private)<br/>raw rows → validated events"]
        config["config"]
        error["error"]
    end
    sqlite[("SQLite file<br/>WAL mode")]

    caller --> recon
    caller --> store
    recon --> projection
    recon --> diff
    recon --> store
    recon --> snapshot
    store --> domain
    store --> integrity
    store --> snapshot
    store --> record
    store --> schema
    schema --> sqlite
    store --> sqlite
    integrity --> record
    record --> domain
    projection --> domain
    store --> config
```

| Module      | Responsibility | Depends on SQLite? |
|-------------|----------------|--------------------|
| `domain`    | Identifiers, `AppendRequest`, `StoredEvent`, `Stream`, `Timestamp`, `Digest`, canonical JSON. Every value is validated on construction and deserialization. | no |
| `integrity` | `EventDigestInput::compute` (the digest construction), `ChainVerifier`, `IntegrityReport`. | no |
| `record`    | `RawEventRecord`: a row as stored, and its fully validated decoding. Shared by reads and verification. | no |
| `store`     | `EventStore`: transactions, sequence allocation, event and snapshot queries, error mapping. | yes |
| `snapshot`  | `Snapshot`, `SnapshotId`, `ProjectionKey`, the snapshot digest, and `SnapshotProblem` / `SnapshotCheck`. | no |
| `projection`| The `Projection` trait, `ProjectionError`, `decode_payload`, and the `Counter` / `Inventory` examples. | no |
| `reconstruct` | `Reconstructor<P>`: target resolution, snapshot selection and validation, the bounded fold, equivalence checks, `diff` and `explain`, and reports. | no (uses `EventStore` only) |
| `diff`      | A structural JSON diff with RFC 6901 paths. | no |
| `config`    | `StoreConfig`, `Durability`, `Limits`, `ORBIT_*` environment variables. | no |
| `error`     | Typed errors. The driver error is wrapped opaquely (`DatabaseError`). | wraps it |

Only `store` touches `rusqlite`, and no `rusqlite` type appears in the public
API. A future backend would need a new `store` implementation, and nothing else
would change.

## Append data flow

```mermaid
sequenceDiagram
    participant C as Caller
    participant S as EventStore
    participant D as domain / integrity
    participant DB as SQLite

    C->>S: append(stream, AppendRequest)
    S->>D: canonicalise payload + metadata, check limits
    S->>S: event_id (given or UUIDv7), recorded_at = now
    S->>DB: BEGIN IMMEDIATE (write lock, ≤ busy_timeout)
    S->>DB: SELECT head FROM streams
    S->>S: check ExpectedSequence, duplicate event_id
    S->>D: digest = H(fields, prev = head.digest)
    S->>DB: INSERT event (trigger: seq = head+1, prev = head digest)
    S->>DB: UPDATE streams head (trigger: +1, matches event)
    S->>DB: COMMIT (fsync per Durability)
    S-->>C: StoredEvent
```

Any error drops the transaction, which rolls it back. The exact semantics are in
[STORAGE_DESIGN.md](STORAGE_DESIGN.md#append-transaction).

## Reconstruction data flow

```mermaid
sequenceDiagram
    participant C as Caller
    participant R as Reconstructor<P>
    participant S as EventStore
    participant P as Projection

    C->>R: reconstruct(stream, Target, options)
    R->>S: head(stream), resolve target once
    alt policy Prefer / Strict / Exact
        R->>S: snapshot_candidates(≤ target, newest first)
        R->>S: check digest + event binding
        R->>R: check projection key, decode state
        Note over R: invalid → warning (Prefer) or error (Strict/Exact)
    end
    loop pages of ≤ 512 until target
        R->>S: read_after(position, limit)
        R->>R: check sequence + prev_digest continuity
        R->>P: apply(state, event)
    end
    R-->>C: Reconstruction { state, report }
```

Reconstruction holds no transaction across the fold. See
[RECONSTRUCTION.md](RECONSTRUCTION.md) and [SNAPSHOTS.md](SNAPSHOTS.md).

## Key decisions

| Decision | Record |
|----------|--------|
| SQLite through `rusqlite` with a synchronous API, not `sqlx` | [ADR 0001](adr/0001-sqlite-via-rusqlite.md) |
| Canonical JSON stored and hashed, with a per-stream hash chain | [ADR 0002](adr/0002-canonical-json-and-hash-chain.md) |
| One crate and a concrete store, no storage trait yet | [ADR 0003](adr/0003-single-crate-concrete-store.md) |
| Sequence allocation under `BEGIN IMMEDIATE` | [ADR 0004](adr/0004-sequence-allocation.md) |
| Snapshots bound to an event digest, with no long transactions during reconstruction | [ADR 0005](adr/0005-snapshot-binding-and-boundary.md) |

Smaller choices:

- **Explicit stream creation.** Appending to an unknown stream is an error,
  not an implicit create. That prevents typos from creating streams.
- **Store-assigned timestamps.** `recorded_at` comes from the store, and only
  `sequence` defines order.
- **Panic-free paths.** Library code avoids `unwrap` and `expect` on anything
  a caller or a damaged file can influence. A poisoned mutex is recovered,
  because rusqlite rolls back open transactions during unwinding.
- **`unsafe_code = "forbid"`** across the workspace.
- **A concrete error type for projections.** `ProjectionError` is a struct
  with a kind and a message, not an associated type. That keeps
  `ReconstructionError` non-generic and serializable.
- **No changes to the Phase 1 API.** Reconstruction is built on the existing
  `read_after`, `event` and `head` methods. Phase 2 only added snapshot
  methods to `EventStore`, plus migration 2.

## Extension points for later phases

- **Phase 3 (recovery, concurrency, replay).** Deterministic replay and
  divergence diagnostics can build on `compare_with_full_replay` and `explain`.
  `ChainVerifier` already separates "invalid data" from "could not verify".
  Crash testing will exercise `Durability`. Read connection pooling can go
  behind the existing API.
- **Phase 4 (API).** Domain types, reconstruction reports, diffs, snapshot
  statuses and errors' detail structs already derive `Serialize`, and
  requests have validating `Deserialize`. The Axum layer will call the store
  through `spawn_blocking`.
