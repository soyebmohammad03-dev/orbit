# Deterministic replay and divergence detection

`orbit_core::replay` records reproducible replay computations and re-executes
them later. A typical use is after a projection change: check whether the new
code still produces the recorded results, and if not, where they first differ.

## Data model

A **replay run** (`replay_runs`, schema v3) pins down one computation:

| Field | Meaning |
|-------|---------|
| `id` | `ReplayRunId` (UUIDv7) |
| `stream_id`, `from`, `to` | The interval of events `(from, to]` |
| `from_event_digest`, `to_event_digest` | Digests of the boundary events (`GENESIS` for `from = 0`). They pin the exact history. |
| `projection` | `ProjectionKey` that produced the expectations |
| `start_source` | `initial`, `snapshot {snapshot_id}`, or `reconstructed {via_snapshot}` |
| `start_state`, `start_state_digest` | The starting state, stored in the run itself |
| `expected_state`, `expected_state_digest` | The resulting state |
| `trace` | `state_digest` after each event, 32 bytes per event, at most 100,000 events |
| `record_digest` | `ORBIT-REPLAY-RUN-DIGEST-V1` over every other column |

State digests are `SHA-256(lp("ORBIT-STATE-DIGEST-V1") || lp(canonical JSON))`.
Two states are equal exactly when their canonical JSON is identical.

An **execution** (`replay_executions`) is an append-only log entry. It holds
the outcome label, the replayed final-state digest, and the full serialized
`ReplayExecution` report. Runs and executions are immutable (enforced by
triggers). Deleting a run deletes its executions. `start_snapshot_id` is
provenance only. It is not a foreign key, because snapshots may be deleted.

## Operations

```rust,ignore
let lab = ReplayLab::new(&store, Inventory);
let run = lab.record(&stream, ReplayStart::Initial, Target::Latest)?;      // 1. create + record
let execution = lab.execute(run.id)?;                                     // 2-5. execute, compare, persist
let again = lab.execute(run.id)?;                                         //      re-execute
let inspection = lab.inspect(run.id)?;                                    // 6. run + all executions
let report = lab.experiment(&stream, Target::Latest, SnapshotPolicy::Prefer)?; // structured experiment
```

**Recording:**

1. Resolves the target. A target past the head is `TargetBeyondHead`.
2. Obtains the start state. `Initial` starts at 0. `Snapshot` validates the
   snapshot with the `Exact` policy, so a corrupt snapshot is refused.
   `Reconstructed { at, snapshots }` reconstructs that position.
3. **Verifies the interval** (digests and chain from the start position) and
   refuses damaged history (`ReplayError::HistoryIntegrity`), so damage is
   never baked into an expectation.
4. Folds the events while recording the trace, then persists the run.

**Executing** never substitutes anything. It runs these checks in order and
stops at the first one that fails:

| Step | Failure → outcome |
|------|-------------------|
| Record digest matches | `integrity_failure` (`RunRecord`) |
| Record decodes and is internally consistent | `invalid_input` |
| Current projection key equals the recorded key | `unavailable` (`ProjectionMismatch`). It never falls back to another version. |
| Stream head ≥ `to` | `unavailable` (`HistoryUnavailable`) |
| Boundary digests match, and the interval verifies | `integrity_failure` (`Boundary`, `History` with categories) |
| Start-snapshot provenance still valid | **warning only**: the run stores its own start state |
| Start state decodes into the projection's state type | `invalid_input` |
| Fold the events, comparing each state digest with the trace | see below |

## Divergence classifications (`ReplayOutcome`)

| Outcome | Meaning |
|---------|---------|
| `exact_match` | Every intermediate state and the final state match. |
| `reconverged { first_divergence }` | Some intermediate state differed, but the final state matches. |
| `diverged { first_divergence, changes }` | The final state differs. `changes` is the structural diff from expected to actual. |
| `projection_failed { failure, first_divergence }` | The current projection rejected an event: `ProjectionFailure` with sequence, event id and type. |
| `unavailable { reason }` | The replay cannot run with this projection or history. |
| `invalid_input { reason }` | The record is malformed or inconsistent, or the start state cannot be decoded. |
| `integrity_failure { violations }` | The record or the replayed history was altered. |

`first_divergence` names the **first event after which the state digest
differs**: its sequence, event id and type, the expected and actual digests,
and how many events matched before it. That is where the divergence became
*observable*. It is not necessarily its cause. The tests use controlled
projections that keep the same key but change behaviour, to check that
localisation is exact (`tests/replay_properties.rs`).

## Experiments

`ReplayLab::experiment(stream, target, policy)` produces a serializable
`ExperimentReport`. It verifies the stream, compares snapshot-assisted
reconstruction under `policy` with full replay, records a run from the
initial state, and executes it. The report includes the experiment id (the
execution id), the run id, the interval, the projection, the policy, an
integrity summary, the equivalence summary with both state digests, the replay
outcome and per-phase timings. If integrity verification fails, the later
phases are skipped and a note explains why. Only the run and its execution are
persisted. The report itself is returned.

## Reproducibility assumptions

- Projections are deterministic (see [RECONSTRUCTION.md](RECONSTRUCTION.md)).
  Replay is how violations of that are found.
- "Same projection" means the same `ProjectionKey`. ORBIT cannot detect code
  that changed without a version bump, except through replay divergence, which
  is the point.
- Canonical JSON and the digest constructions are pinned by golden vectors.
  A change in `serde_json` float formatting would change state digests. That
  would show up as divergence on float-bearing states, and should be treated
  as such.
- The trace limits runs to 100,000 events. Replaying longer histories means
  chaining runs, each starting where the previous one ended.
