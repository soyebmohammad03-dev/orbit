# Recovery, concurrency and integrity guarantees

This document states what ORBIT guarantees when operations fail, when the
process crashes, and when several writers compete. It also covers what
integrity verification can and cannot prove. Each guarantee is tied to the
tests that exercise it.

## Atomic operations and transaction boundaries

Every write is a single SQLite transaction. Each one takes the write lock up
front with `BEGIN IMMEDIATE`, and is either committed in full or not at all.

| Operation | Transaction | Writes |
|-----------|-------------|--------|
| `create_stream` | `BEGIN IMMEDIATE` … `COMMIT` | one `streams` row |
| `append` | `BEGIN IMMEDIATE` … `COMMIT` | one `events` row and the stream head |
| `save_snapshot` / `create_snapshot` | `BEGIN IMMEDIATE` … `COMMIT` (the reconstruction runs before it, outside any transaction) | one `snapshots` row |
| `ReplayLab::record` | `BEGIN IMMEDIATE` … `COMMIT` (the fold runs before it) | one `replay_runs` row |
| `ReplayLab::execute` | `BEGIN IMMEDIATE` … `COMMIT` | one `replay_executions` row |

Reads (`event`, `read_stream`, `read_after`, `verify_stream`,
`verify_stream_against`, `verify_store`, snapshot reads) each run in one
deferred read transaction. In WAL mode that is a consistent snapshot of the
database that never blocks writers. Reconstruction and replay read in pages of
independent transactions. They are still consistent, because events at or
below a fixed target are immutable (see [RECONSTRUCTION.md](RECONSTRUCTION.md)).

The database enforces the same invariants itself, through constraints and
triggers ([STORAGE_DESIGN.md](STORAGE_DESIGN.md)). A bug in ORBIT or a foreign
writer cannot, for example, commit a duplicate or skipped sequence number
while those triggers exist.

## What a failure leaves behind

Any error inside a write transaction drops the transaction, and SQLite rolls
it back. **A failed operation never leaves committed state.** This covers
validation errors, conflicts, `Busy`, SQL errors and injected faults.

### Fault injection (how this is tested)

With the `fault-injection` cargo feature, a per-store hook
(`EventStore::set_fault_hook`) is consulted at named boundaries
(`orbit_core::fault::FaultPoint`):

| Point | Where |
|-------|-------|
| `CreateStreamBeforeCommit` | stream row inserted, before `COMMIT` |
| `AppendBeforeBegin` | after validation, before `BEGIN IMMEDIATE` |
| `AppendAfterBegin` | write lock held, nothing read |
| `AppendAfterSequenceAllocated` | head read, sequence and digest computed, nothing written |
| `AppendAfterInsert` | event inserted, head not yet advanced |
| `AppendBeforeCommit` | event inserted and head advanced, before `COMMIT` |
| `SnapshotBeforeBegin` / `SnapshotAfterValidation` / `SnapshotBeforeCommit` | the snapshot write path |
| `ReplayBeforeCommit` | replay run or execution inserted, before `COMMIT` |

When the hook returns `true`, the operation fails there with
`StoreError::InjectedFault(point)`, exactly as a real error at that spot would.
The feature is **off by default**. In that case the points compile to nothing,
and there is no global state either way. The test suite enables the feature
through a self dev-dependency.

`tests/fault_injection.rs` checks that, for every point:

- the error is reported;
- head, events and raw row counts are unchanged;
- `verify_store` passes;
- the same connection works afterwards;
- everything committed survives reopening.

`tests/replay_properties.rs` adds a property test over random fault schedules.

### Process crashes

`tests/crash_recovery.rs` runs scenarios in **real child processes**:

- **Abort inside an operation.** The hook calls `std::process::abort()` at
  `AppendAfterBegin`, `AppendAfterInsert`, `AppendBeforeCommit` and
  `SnapshotBeforeCommit`. There are no destructors and no rollback, and a WAL
  file is left behind.
- **`SIGKILL`.** The parent kills a child that appends continuously. Each
  append is acknowledged on stdout only after it returned.

The parent then reopens the database, which runs SQLite's WAL recovery, and
asserts:

- `verify_store` is valid;
- every committed or acknowledged event is present;
- no uncommitted event is present;
- sequences are dense;
- the next append lands at the right position.

**Not simulated: operating-system crashes and power loss.** Those need a VM
or a fault-injecting block device. Durability under them rests on SQLite's
documented guarantees and on `Durability::Full` (`synchronous = FULL`, plus
`F_FULLFSYNC` on macOS). That holds only if the storage device honours flush
requests. Under `Durability::Normal`, the last commits may be lost on power
loss, though the database stays consistent.

### Recovery procedure

1. Reopen the database. SQLite recovers or discards the WAL automatically, and
   ORBIT applies any pending migrations.
2. Run `EventStore::verify_store()`. It covers SQLite's `PRAGMA
   integrity_check`, `PRAGMA foreign_key_check`, the presence of ORBIT's
   protective triggers, every stream's chain and head, every snapshot's digest
   and history binding, and every replay run's record digest.
3. If `report.is_valid()` is false, treat the listed items as integrity
   violations. Do not retry. Investigate using the report's categories,
   sequences and event ids. Snapshots are derived data, so a damaged snapshot
   can be deleted and rebuilt from events.

### Error classes

`StoreError::class()` separates recoverable failures from integrity
violations:

| Class | Errors | Reaction |
|-------|--------|----------|
| `Retryable` | `Busy`, `InjectedFault` | retry |
| `Conflict` | `SequenceConflict`, `DuplicateEventId`, `StreamAlreadyExists`, `SnapshotConflict` | re-read, decide |
| `InvalidRequest` | `Validation`, `Config`, `SequenceExhausted` | fix the request |
| `NotFound` | stream, event, snapshot, replay run | — |
| `Integrity` | `CorruptRecord`, `CorruptSnapshot`, `CorruptReplayRecord`, `SnapshotHistoryMismatch` | stop, verify the store |
| `Incompatible` | foreign file, newer schema | use a matching build |
| `Database` | other SQLite errors | inspect |

## Concurrency semantics

These levels are distinct, and ORBIT makes claims only about the first three.

- **Thread safety.** `EventStore` is `Send + Sync`. Its single connection is
  behind a mutex, so calls through one instance are serialised.
- **Multiple connections and processes on one host.** Several `EventStore`s,
  in one process or many, may open the same file. SQLite's file locks
  serialise writers database-wide. Readers never block in WAL mode. A writer
  that cannot get the lock within `busy_timeout` gets `Busy`, with nothing
  written.
- **Within a stream.** Sequence numbers are allocated under the write lock, so
  they are never duplicated or skipped. `ExpectedSequence::Exact` is checked
  under the same lock. Of several writers expecting the same head, exactly one
  succeeds and the rest get `SequenceConflict { expected, actual }`.
- **Distributed consistency.** None. There is no replication, consensus, or
  multi-host access. Network file systems are unsupported, because WAL needs
  shared memory on one host.

Stress tests (`tests/stress.rs`, `tests/concurrency.rs`, `tests/snapshots.rs`)
run concurrent blind writers, optimistic writers with retry, readers and a
snapshotter on separate connections, plus optimistic writers in **separate
processes**. They check these invariants:

- streams are dense, with no duplicate sequence numbers;
- event ids are unique;
- every acknowledged append is stored exactly once, and nothing unacknowledged is;
- every attempt is either a success or a correctly reported conflict;
- a successful optimistic append lands at `expected + 1`;
- heads never move backwards for a reader;
- an event never changes once observed;
- each process's appends keep its program order;
- the history is identical after reopening;
- every snapshot's state equals full replay at its sequence, and its event
  digest is the stored event's digest.

## Integrity threat model

| Party | Detected? |
|-------|-----------|
| Accidental corruption (bit rot, buggy tooling, partial edits) | Yes. Event digests, the chain, heads, snapshot digests and run digests, plus SQLite's own `integrity_check`. |
| Someone editing rows by hand without recomputing digests | Yes, with category, sequence and event id. |
| Someone dropping ORBIT's triggers | Yes. Reported as `missing_safeguards`. |
| Someone who can write the file and recomputes every unkeyed digest consistently | **No**, by plain verification alone. The result is a self-consistent history. **Yes, relative to a trusted checkpoint**, below. |
| Someone who also controls every place checkpoints are kept | No. Detecting this needs keys and signatures, which are not implemented. |

### Diagnostics

`verify_stream` and `verify_store` report each issue with an
`IntegrityCategory`:

- `modified_event`: a hashed field or the digest itself was altered;
- `broken_chain`;
- `invalid_genesis`: event 1 does not link to 32 zero bytes;
- `sequence_continuity`: missing, duplicated or reordered sequences;
- `head_inconsistency`: for example, a truncated tail;
- `checkpoint_mismatch`;
- `malformed_record`.

Each issue carries its sequence. `IntegrityReport::affected_events` lists the
event ids of the decodable events involved.

### Checkpoints as a trust anchor

`EventStore::checkpoint(stream)` returns `{ sequence, digest }` for the
current head. Store it somewhere the database writer cannot modify: another
system, a signed log, a release note. Later,
`verify_stream_against(stream, &[checkpoint])` reports `CheckpointMismatch`
unless the event at that sequence still has exactly that digest. Because each
digest chains everything before it, this detects any rewrite of history up to
the checkpoint, even one with consistently recomputed digests, and any
truncation below it. The demonstration is
`tests/integrity_diagnostics.rs::checkpoints_detect_a_consistently_recomputed_rewrite`.
A checkpoint proves nothing about events appended after it.
