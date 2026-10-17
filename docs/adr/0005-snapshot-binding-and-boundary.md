# ADR 0005: Snapshots bound to an event digest, with no long transactions

- Status: accepted
- Date: 2026-10-18

## Context

Snapshots let reconstruction skip replaying a prefix of a stream. A snapshot
is only correct if its state is exactly what the projection produces from
events `1..=n` of *this* history. Long read transactions would block WAL
checkpointing and serialise with writers, and the brief asks to avoid them.

## Decision

1. Each snapshot stores the digest of the event at its sequence
   (`event_digest`). `save_snapshot` and a database trigger reject a snapshot
   unless the stored event at that sequence has exactly that digest. Using a
   snapshot re-checks this binding.
2. The reconstruction boundary is the target sequence, resolved once against
   the head. Because events are immutable and dense, events `≤ target` are
   read in independent bounded pages. No transaction spans the fold.
3. The fold checks `prev_digest` continuity from the start point, so a snapshot's
   `event_digest` must chain into the events applied after it.
4. Invalid snapshots are skipped and reported under the default `Prefer`
   policy, or are fatal under `Strict` and `Exact`.

## Consequences

- A snapshot cannot claim events that were not applied. It becomes unusable,
  and observably so, if the event it was built on is altered.
- Concurrent appends never invalidate an in-progress reconstruction or
  snapshot.
- The binding proves which history a snapshot was built from. It does not
  prove that the state is the correct result of that history. Full-replay
  comparison (`compare_with_full_replay`) covers that case.
