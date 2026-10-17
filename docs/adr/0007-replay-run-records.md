# ADR 0007: Self-contained replay runs with a per-event state-digest trace

- Status: accepted
- Date: 2026-10-18

## Context

Replay must detect divergence, say where it first appears, and stay
meaningful when snapshots are deleted or damaged.

## Decision

- A run stores its own start state and the expected final state, plus the
  digest of the state after every event (the trace, 32 bytes per event). It
  also stores the digests of the interval's boundary events. A record digest
  covers everything.
- Execution re-verifies the record and the interval, then compares the state
  digest after each event with the trace.
- Snapshots used as start points are provenance. Their later corruption or
  deletion is a warning, not a different result.
- Interval length is capped at 100,000 events, keeping a trace at or below 3.2 MB.

## Consequences

- The first divergent event is identified exactly, at the cost of
  serializing and hashing the state after every event. That cost is measured
  in `benches/crash_lab.rs`.
- Runs remain replayable as long as their events exist, and runs over
  altered history are classified as integrity failures, not divergences.
