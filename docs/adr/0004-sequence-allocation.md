# ADR 0004: Sequence allocation under SQLite's write lock

- Status: accepted
- Date: 2026-10-18

## Context

Each append must receive `head + 1` with no gaps and no duplicates, including
when several connections or processes write to the same stream.

## Decision

Allocate sequence numbers inside a `BEGIN IMMEDIATE` transaction that reads
`streams.head_sequence`, inserts the event, and advances the head. The
database backs this up with a primary key on `(stream_id, sequence)` and
triggers that reject any insert other than `head + 1` chained to the head
digest.

## Alternatives considered

- **`MAX(sequence) + 1` in a deferred transaction.** Two readers can compute
  the same value. One then fails on the unique constraint with
  `SQLITE_BUSY_SNAPSHOT` and has to retry. That works, but the retry
  logic is subtle.
- **Retry loop on constraint violation.** Hides contention, and makes
  optimistic-concurrency errors harder to report precisely.

## Consequences

- Writers are serialised database-wide, not per stream. That is SQLite's
  model anyway. Throughput is bounded by commit latency (see the benchmarks).
- A writer waits up to `busy_timeout` for the lock, then gets the typed
  `StoreError::Busy`. Nothing is written in that case.
- `ExpectedSequence::Exact` checks are exact. The head the caller is compared
  against is read while the lock is held.
