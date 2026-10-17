# Storage design

The persistence layer is in `crates/orbit-core/src/store/`. The schema and
migrations are in `store/schema.rs`.

## Connection setup

Each `EventStore` opens one SQLite connection with the following settings:

| Setting              | Value                                      | Why |
|----------------------|--------------------------------------------|-----|
| `journal_mode`       | `WAL`                                      | Readers do not block the writer. Commits are append-to-log. Opening fails if WAL cannot be enabled. |
| `synchronous`        | `FULL` (default) or `NORMAL`               | See [Durability](#durability). |
| `fullfsync`          | `ON` with `Durability::Full`               | macOS only. Makes SQLite use `F_FULLFSYNC`, which flushes the drive cache. Plain `fsync` does not on macOS. Ignored elsewhere. |
| `foreign_keys`       | `ON`                                       | Enforces `events.stream_id → streams`. |
| `busy_timeout`       | 5 s (configurable, max 600 s)              | How long to wait for another connection's lock before returning `StoreError::Busy`. |
| `application_id`     | `0x4F524254` ("ORBT")                      | Marks ORBIT files. Foreign files are refused. |
| `user_version`       | schema version (currently `1`)             | Migration bookkeeping. |

SQLite is compiled from source (`rusqlite` `bundled` feature), so every
platform uses the same SQLite version, with JSON functions and `STRICT` tables.

## Schema (version 1)

```sql
CREATE TABLE streams (
    stream_id     TEXT    NOT NULL PRIMARY KEY CHECK (length(stream_id) BETWEEN 1 AND 128),
    created_at    INTEGER NOT NULL CHECK (created_at >= 0),
    metadata      TEXT    NOT NULL CHECK (json_valid(metadata) AND json_type(metadata) = 'object'),
    head_sequence INTEGER NOT NULL DEFAULT 0 CHECK (head_sequence >= 0),
    head_digest   BLOB    NOT NULL CHECK (length(head_digest) = 32)
) STRICT;

CREATE TABLE events (
    stream_id      TEXT    NOT NULL REFERENCES streams (stream_id) ON DELETE RESTRICT,
    sequence       INTEGER NOT NULL CHECK (sequence >= 1),
    event_id       BLOB    NOT NULL UNIQUE CHECK (length(event_id) = 16),
    event_type     TEXT    NOT NULL CHECK (length(event_type) BETWEEN 1 AND 128),
    schema_version INTEGER NOT NULL CHECK (schema_version BETWEEN 1 AND 4294967295),
    recorded_at    INTEGER NOT NULL CHECK (recorded_at >= 0),
    payload        TEXT    NOT NULL CHECK (json_valid(payload)),
    metadata       TEXT    NOT NULL CHECK (json_valid(metadata) AND json_type(metadata) = 'object'),
    prev_digest    BLOB    NOT NULL CHECK (length(prev_digest) = 32),
    digest         BLOB    NOT NULL CHECK (length(digest) = 32),
    PRIMARY KEY (stream_id, sequence)
) STRICT;
```

Timestamps are stored as integer microseconds since the Unix epoch (UTC).
Event ids are stored as 16 raw bytes, and digests as 32 raw bytes.

### Indexes

- `streams` primary key on `stream_id`, for head lookups.
- `events` primary key on `(stream_id, sequence)`. It serves every read:
  point lookups, ascending range scans for `read_after`, and verification.
  It also makes duplicate sequence numbers impossible.
- `UNIQUE (event_id)`, for global uniqueness and the duplicate-id check.

No other indexes exist. Phase 2 may add some for snapshot lookups.

### Triggers: the database defends its own invariants

These triggers fire for *any* writer, not only ORBIT:

| Trigger                       | Rejects |
|-------------------------------|---------|
| `events_insert_extends_head`  | an insert unless `sequence = head_sequence + 1` and `prev_digest = head_digest` of its stream |
| `events_no_update`            | any `UPDATE` of `events` |
| `events_no_delete`            | any `DELETE` from `events` |
| `streams_head_only_advances`  | changes to `stream_id`, `created_at` or `metadata`; a head that does not advance by exactly one; a head that does not match an existing event's sequence and digest |
| `streams_no_delete`           | any `DELETE` from `streams` |

The test suite (`tests/constraints.rs`) checks each of these with raw SQL.
They are guardrails against bugs and accidental edits. They are not a security
boundary: anyone who can write the file can drop them.

## Migrations

`MIGRATIONS` is an ordered list of SQL scripts. Script `i` moves the schema
from version `i` to `i + 1`. On open:

1. `BEGIN IMMEDIATE`, so concurrent openers serialise.
2. Read `application_id` and `user_version`. Refuse the file if it has a
   foreign `application_id`, if it is unversioned but contains other tables,
   or if `user_version` is newer than this build supports
   (`UnsupportedSchemaVersion`).
3. Apply the next script, set `application_id` and `user_version`, and commit.
4. Repeat until current.

Each migration is atomic. A failure leaves the file at the previous version.
Released migrations are never edited. Changes go in new scripts.

## Append transaction

```text
validate + canonicalise (no lock)
BEGIN IMMEDIATE                         -- take the write lock, wait ≤ busy_timeout
  SELECT head_sequence, head_digest FROM streams WHERE stream_id = ?
  -- missing → StreamNotFound
  -- ExpectedSequence::Exact(n) and n ≠ head → SequenceConflict
  -- caller-supplied event_id already present → DuplicateEventId
  sequence ← head + 1                    -- overflow → SequenceExhausted
  digest   ← ORBIT-EVENT-DIGEST-V1(…, prev = head_digest)
  INSERT INTO events (…)
  UPDATE streams SET head = (sequence, digest) WHERE stream_id = ? AND head_sequence = head
COMMIT
```

Any error before `COMMIT` drops the transaction, which rolls back every
statement in it. `tests/concurrency.rs` injects a failure into the head update,
which runs *after* the insert, and checks that the inserted row is gone.

`create_stream` uses the same `BEGIN IMMEDIATE` pattern with
`INSERT … ON CONFLICT DO NOTHING`.

Reads (`event`, `read_stream`, `read_after`, `verify_stream`) run inside a
deferred read transaction. In WAL mode that gives a consistent snapshot of the
stream row and its events, even while another connection is appending.

## Concurrency behaviour

- SQLite allows **one writer at a time per database file**. `BEGIN IMMEDIATE`
  takes that lock before the head is read, so two writers can never read the
  same head and both succeed. The primary key and the insert trigger enforce the
  same rule independently.
- Within one process, an `EventStore` is `Send + Sync`. Its connection sits
  behind a `Mutex`, so calls through one instance are serialised. Several
  instances, including instances in other processes, may open the same file.
  SQLite's file locks coordinate them.
- A writer that cannot get the lock within `busy_timeout` receives
  `StoreError::Busy`, with nothing written. It is safe to retry.
- Under `ExpectedSequence::Exact`, among racing writers that read the same
  head, exactly one succeeds and the others get `SequenceConflict`
  (`tests/concurrency.rs::racing_optimistic_writers_exactly_one_wins`).
- WAL requires all processes to be on the same host. **Do not place the
  database on a network file system.**

## Durability

These are the guarantees that follow from SQLite's documented behaviour, and
nothing beyond them:

- **`Durability::Full` (default).** `synchronous = FULL`, plus
  `fullfsync = ON` on macOS. When `append` returns `Ok`, the WAL has been
  synced. The event survives a process crash, and it survives an OS crash or
  power loss **if the storage device honours flush requests**. Hardware or
  firmware that acknowledges flushes it has not performed can still lose
  recently committed data.
- **`Durability::Normal`.** `synchronous = NORMAL`. Committed events survive
  a process crash. An OS crash or power loss may roll back the most recent
  commits, but the database stays consistent and is not corrupted.
- Neither mode protects against media failure, file deletion, or copying the
  file while a writer is active. Back up with SQLite's backup API or
  `VACUUM INTO`, not with `cp`.
- Crash recovery testing (killing processes mid-commit, fault injection) is
  planned for Phase 3. Phase 1 relies on SQLite's guarantees and does not
  independently verify them.

## Integrity verification

### Construction (`ORBIT-EVENT-DIGEST-V1`)

```text
digest = SHA-256(
    lp("ORBIT-EVENT-DIGEST-V1")
 || lp(stream_id)            UTF-8
 || be_u64(sequence)
 || event_id                 16 bytes
 || lp(event_type)           UTF-8
 || be_u32(schema_version)
 || be_i64(recorded_at)      µs since Unix epoch
 || lp(payload)              canonical JSON as stored
 || lp(metadata)             canonical JSON as stored
 || prev_digest              32 bytes; 32 zero bytes for sequence 1
)
lp(x) = be_u64(byte_length(x)) || x
```

The domain tag separates ORBIT event digests from any other SHA-256 use. It
will change if the construction changes. The encoding is injective, because
variable-length fields are length-prefixed and the rest are fixed-width in a
fixed order. A golden test vector in `integrity.rs` pins the construction. It
was cross-checked against an independent Python `hashlib` implementation.

The digest covers the **stored text** of payload and metadata, so verification
never re-serializes JSON.

### `EventStore::verify_stream`

Verification reads one consistent snapshot in pages of 1,000 rows and checks:

| Check                                       | Issue reported     |
|---------------------------------------------|--------------------|
| Each row decodes into a valid event         | `MalformedRecord`  |
| Sequences are 1, 2, 3, … without gaps       | `SequenceGap`      |
| Stored digest = recomputed digest           | `DigestMismatch`   |
| `prev_digest` = previous row's stored digest (or genesis) | `ChainBroken` |
| Stream head = last row's (sequence, digest) | `HeadMismatch`     |

The chain is followed through *stored* digests, so altering one event produces
exactly one `DigestMismatch`, not a cascade. The result is an
`IntegrityReport` with `stream_id`, `events_checked`, the recorded `head`, up to
1,000 `issues` (each naming its sequence number), and `total_issues`. The
outcomes are kept apart:

- **valid**: `Ok(report)` with `report.is_valid()`;
- **invalid data**: `Ok(report)` listing the issues;
- **verification could not run** (I/O, lock timeout, missing stream):
  `Err(StoreError)`.

Ordinary reads also validate every row and return `CorruptRecord { stream,
sequence, reason }` instead of panicking. They do **not** recompute digests.
Only verification does.

### What it detects

- Accidental corruption or ad-hoc modification of any hashed field of any
  event: payload, metadata, type, schema version, timestamp, id, sequence.
- Replacement of an event's digest (the next event's `prev_digest` no longer matches).
- Deletion or reordering of events inside a stream.
- Truncation of a stream's tail, provided the stream row's head was not also rewritten.

### What it cannot guarantee

- **The digests are unkeyed.** Anyone who can write the database file can
  change events, recompute every digest, and update the head. The result
  verifies as valid. `tests/integrity.rs::consistent_rewrite_with_recomputed_digests_is_not_detected`
  demonstrates this. Detecting that needs a secret key (an HMAC) or digests
  anchored outside the database. Neither exists yet.
- Stream rows are not hashed. `created_at` and stream `metadata` are protected
  only by triggers. Deleting an entire stream (stream row and all its events)
  is not detected by per-stream verification.
- Verification checks a stream against itself. It does not prove that the
  data is what a client originally sent.
