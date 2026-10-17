# Snapshots

A snapshot is a stored projection state, tied to the exact history position it
was built from. Snapshots are **derived data**. The event stream is always the
source of truth, any snapshot can be rebuilt by replay, and a snapshot is only
used after it has been validated.

## What a snapshot contains

| Field | Meaning |
|-------|---------|
| `id` | `SnapshotId` (UUIDv7) |
| `stream_id`, `sequence` | The stream, and the last event applied to produce the state (≥ 1) |
| `projection` | `ProjectionKey { id, version }` of the projection that produced it |
| `event_digest` | The digest of the event at `sequence` that was actually applied |
| `state` | The state as canonical JSON (at most 16 MiB) |
| `state_digest` | `ORBIT-SNAPSHOT-DIGEST-V1` over all of the above, plus `created_at` |
| `created_at` | Informational |

### Digest (`ORBIT-SNAPSHOT-DIGEST-V1`)

```text
state_digest = SHA-256(
    lp("ORBIT-SNAPSHOT-DIGEST-V1")
 || snapshot_id (16 bytes) || lp(stream_id) || be_u64(sequence)
 || lp(projection_id) || be_u32(projection_version)
 || event_digest (32 bytes) || be_i64(created_at_µs) || lp(state canonical JSON)
)
lp(x) = be_u64(len(x)) || x
```

It uses the same length-prefixed, domain-separated construction as event
digests. A golden vector in `snapshot.rs` pins it. That vector was
cross-checked against an independent Python `hashlib` implementation.

## Storage (schema migration 2)

```sql
CREATE TABLE snapshots (
    snapshot_id        BLOB    NOT NULL PRIMARY KEY CHECK (length(snapshot_id) = 16),
    stream_id          TEXT    NOT NULL REFERENCES streams (stream_id) ON DELETE RESTRICT,
    sequence           INTEGER NOT NULL CHECK (sequence >= 1),
    projection_id      TEXT    NOT NULL CHECK (length(projection_id) BETWEEN 1 AND 128),
    projection_version INTEGER NOT NULL CHECK (projection_version BETWEEN 1 AND 4294967295),
    event_digest       BLOB    NOT NULL CHECK (length(event_digest) = 32),
    state              TEXT    NOT NULL CHECK (json_valid(state)),
    state_digest       BLOB    NOT NULL CHECK (length(state_digest) = 32),
    created_at         INTEGER NOT NULL CHECK (created_at >= 0),
    UNIQUE (stream_id, projection_id, projection_version, sequence)
) STRICT;
```

- **Trigger `snapshots_insert_matches_history`.** An insert is rejected unless
  an event exists at `(stream_id, sequence)` with exactly `event_digest`. A
  snapshot can therefore never claim history that is not in the store, even
  for raw writes.
- **Trigger `snapshots_no_update`.** Snapshots are immutable.
- **Deletion is allowed.** Snapshots are derived, and ORBIT never relies on a
  particular one existing. There is no pruning API yet.
- **Uniqueness.** At most one snapshot per (stream, projection id, version,
  sequence). The unique index also serves the selection query
  `… WHERE stream_id = ? AND projection_id = ? AND projection_version = ?
  AND sequence <= ? ORDER BY sequence DESC`.

## Creating snapshots and the consistency boundary

`Reconstructor::create_snapshot(stream, target, options)`:

1. Reconstructs `target`. `Latest` is resolved to the head once, at the
   start. This uses `options`, so by default the reconstruction may itself start
   from an older valid snapshot.
2. Saves the state with `EventStore::save_snapshot`. It records the
   reconstructed position and the digest of the event actually applied there.

`save_snapshot` runs in one short `BEGIN IMMEDIATE` transaction. It checks that
the stream exists, that `sequence ≤ head`, and that the stored event at
`sequence` still has `event_digest` (otherwise `SnapshotHistoryMismatch`).
Then it inserts the row. The database trigger repeats the binding check.

**Appends during snapshot creation.** The snapshot describes exactly the
position resolved in step 1. Events appended while it is being built are
outside that boundary. They do not invalidate it, and later reconstructions
apply them after the snapshot. No transaction is held during the fold, and the
write transaction lasts only for the insert. This is tested with a writer
thread appending on its own connection while snapshots are taken repeatedly
(`tests/snapshots.rs::snapshots_taken_while_another_connection_appends_are_consistent`).

**Idempotence.** Saving a snapshot whose key and sequence already exist
returns the existing snapshot (`created == false`) if the state is identical.
A different state is `StoreError::SnapshotConflict`. Projections are
deterministic, so a conflict means a non-deterministic projection, or one that
changed without a version bump.

## Validation

`EventStore::check_snapshot(id)` performs the storage-level checks.
`Reconstructor::check_snapshot(id)` adds the projection-level checks. Each
outcome is distinct:

| Outcome | `SnapshotProblem` | Detected by |
|---------|-------------------|-------------|
| No such snapshot | (error `StoreError::SnapshotNotFound`) | store |
| Row is malformed (bad lengths, invalid JSON, …) | `Malformed` | store |
| Stored digest ≠ recomputed digest | `IntegrityMismatch { stored, computed }` | store |
| Event at `sequence` missing or has another digest | `HistoryMismatch { expected, actual }` | store |
| Different projection id or version | `Incompatible { found, expected }` | reconstructor |
| Valid JSON, but not the projection's state type | `Undecodable { reason }` | reconstructor |
| Another stream (`Exact` policy) | `WrongStream` | reconstructor |
| Past the requested target (`Exact` policy) | `BeyondTarget` | reconstructor |
| All checks pass | `SnapshotStatus::Usable` | — |

`list_snapshots` (ordered by sequence, then projection id, then version) and
`snapshot(id)` decode rows but do not verify them. A malformed row makes them
return `CorruptSnapshot` rather than silently skipping it.

## Selection and fallback

`ReconstructOptions::snapshots` takes a `SnapshotPolicy`:

| Policy | Behaviour |
|--------|-----------|
| `Ignore` | Always replay from the initial state. |
| `Prefer` (default) | Candidates are compatible snapshots (same projection key) at or before the target, newest first. Each is validated. The first usable one is the start point. Rejected candidates are skipped and recorded in `report.snapshot_warnings`, and also logged at `warn`. With no usable candidate, replay starts from the initial state. |
| `Strict` | Like `Prefer`, but the first invalid candidate aborts with `ReconstructionError::Snapshot { snapshot, sequence, problem }`. |
| `Exact(id)` | Start from that snapshot. Any problem with it is an error. |

So integrity failures stay **observable** even when fallback succeeds. A
snapshot after the target is never selected, because the candidate query is
bounded by `sequence ≤ target` and validation checks it again. Snapshots of
other projection versions are never candidates, and don't produce warnings.
Selection is correct at irregular intervals, because it always takes the
greatest compatible sequence ≤ target. Candidates are fetched 8 at a time, so
many rejected snapshots do not load the whole table.

## Performance

These are measured numbers, not claims. See
[DEVELOPMENT.md](DEVELOPMENT.md#phase-2-reconstruction-benchmarks) for the
table and conditions. On a 10,000-event counter stream, starting from a
snapshot 1,000 events before the head was about 10× faster than full replay.
A snapshot at the head was about 400× faster. On a 100-event stream, `Prefer`
with no snapshot present costs about 15% more than `Ignore`, because of the
candidate query. Snapshots only pay off once the replay they skip is more
expensive than validating them: one digest computation, one event lookup, and
deserializing the state.

## Limitations

- There is no automatic snapshot scheduling or pruning. Callers decide when
  to snapshot.
- Digests are unkeyed (as for events). Someone with write access to the file
  can forge a consistent snapshot. Integrity checks do not detect a
  wrong-but-consistent state either. Use `compare_with_full_replay` to audit.
- States must fit in memory and in 16 MiB of canonical JSON.
