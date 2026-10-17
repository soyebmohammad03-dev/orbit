# Historical reconstruction

This document covers projections, reconstruction, state diffs and provenance
(`crates/orbit-core/src/{projection,reconstruct,diff}`). Snapshot storage and
validation are covered in [SNAPSHOTS.md](SNAPSHOTS.md).

## Projections

A projection is a deterministic left fold over a stream:

```rust,ignore
pub trait Projection {
    type State: Clone + Serialize + DeserializeOwned;
    fn key(&self) -> ProjectionKey;            // name + version, e.g. inventory@v1
    fn initial_state(&self) -> Self::State;    // the state at sequence 0
    fn apply(&self, state: Self::State, event: &StoredEvent)
        -> Result<Self::State, ProjectionError>;
}
```

The contract:

- **Deterministic.** `apply` may depend only on the state and event it is
  given: no clock, randomness, I/O or globals. Order-related logic must use
  `event.sequence`, never `recorded_at`.
- **Explicit about failure.** Every event yields either a new state or a
  `ProjectionError` with a kind:
  - `UnsupportedEvent`: an unknown event type or schema version;
  - `InvalidPayload`: the payload does not match the event type;
  - `RejectedTransition`: well-formed, but invalid in the current state.

  A projection that deliberately ignores certain events must document it and
  return the state unchanged.
- **Versioned.** Bump `key().version` whenever the same events could produce a
  different state, or the serialized state changes shape. Snapshots are only
  reused by projections with an identical key.
- **JSON-round-trippable state.** States are snapshotted, compared and diffed
  as `serde_json` values.

`projection::decode_payload::<T>(event)` deserializes a payload and maps
failures to `InvalidPayload`.

### Example projections (`projection::examples`)

| Projection | State | Events |
|------------|-------|--------|
| `Counter` (`counter@v1`) | `{ value: i64 }` | `counter.incremented {by}`, `counter.decremented {by}`, `counter.reset {}`. Overflow is a rejected transition. Unknown types are unsupported. |
| `Inventory` (`inventory@v1`) | `{ warehouse, items: {sku → {name, on_hand, reserved}}, discontinued: [sku], shipped_total }` | `warehouse_named`, `item_added`, `restocked`, `reserved`, `shipped`, `discontinued`. It enforces stock rules, such as no reserving more than is on hand and no re-adding a discontinued SKU. |

`Inventory` is a teaching example with nested objects, an array and several
independent fields. It is not a stock-keeping system.

## Reconstruction

```rust,ignore
let inventory = Reconstructor::new(&store, Inventory);
let r = inventory.reconstruct(&stream, Target::At(Sequence::new(10)?), ReconstructOptions::default())?;
r.state;   // InventoryState after events 1..=10
r.report;  // ReconstructionReport (provenance)
```

### Target semantics

| Target | Meaning |
|--------|---------|
| `Target::Latest` | The stream head, read **once at the start of the call**. Events appended later are not included. |
| `Target::At(n)`, `0 < n ≤ head` | The state after applying exactly events `1..=n`. |
| `Target::At(0)` | `initial_state()`. Valid for every existing stream, including empty ones. |
| `Target::At(n)`, `n > head` | `ReconstructionError::TargetBeyondHead { target, head }`. There is no clamping. |
| Unknown stream | `ReconstructionError::Store(StoreError::StreamNotFound)` |

An empty stream reconstructs to the initial state at sequence 0, with zero
events applied, for both `Latest` and `At(0)`.

### Algorithm and consistency boundary

1. Read the head once and resolve the target.
2. Choose a start point: the initial state at sequence 0, or a validated
   snapshot (see [SNAPSHOTS.md](SNAPSHOTS.md#selection-and-fallback)).
3. Fold the events after the start point, up to the target, reading them in
   pages of 512 through `EventStore::read_after`.

Events are immutable and sequences are dense, so once `target ≤ head` the
events `1..=target` cannot change. Reconstruction therefore needs no
long-lived transaction, and concurrent appends cannot affect the result.
Memory use is bounded by one page of events plus the state. As defence in
depth, every applied event must have exactly the next sequence number, and a
`prev_digest` equal to the digest of the previous position (the previous event,
or the event the snapshot was built on). A violation stops the fold with
`HistoryInconsistent { sequence, reason }`. This check is not a substitute for
`EventStore::verify_stream`, which recomputes digests.

### Failure semantics

Reconstruction is all-or-nothing:

- If the projection rejects an event, the call returns
  `ReconstructionError::Projection(ProjectionFailure { stream, projection,
  sequence, event_id, event_type, source })`.
- No partial state is ever returned. No event is skipped.
- History before a bad event is still reconstructible by targeting a position
  before it.
- `create_snapshot` fails the same way, so it cannot store a state built past a
  failure.

### The report

`ReconstructionReport` is `Serialize` and reports:

| Field | Meaning |
|-------|---------|
| `stream_id`, `projection` | What was reconstructed, and with which projection key |
| `requested`, `head_at_start` | The target as asked for, and the head when the call began |
| `reconstructed`, `reconstructed_digest` | The position actually reached, and the digest of the event there (`GENESIS` at 0) |
| `start` | `Initial`, or `Snapshot { snapshot_id, sequence }` |
| `events_applied` | Events folded after the start point |
| `applied` | Each applied event (sequence, id, type, schema version). Only filled with `ReconstructOptions::recording_events(true)`, because it grows with the stream. |
| `snapshot_warnings` | Snapshots considered and rejected, and why |
| `elapsed_micros` | Wall-clock time of the call (measurement only) |

## Comparing full replay with snapshot-assisted reconstruction

`compare_with_full_replay(stream, target, policy)` resolves the target once. It
then reconstructs that position twice, once ignoring snapshots and once with
`policy`, and returns an `EquivalenceReport { equivalent, differences, full,
assisted }`. An integrity-valid snapshot whose state is still wrong passes
every digest check. This can happen with a non-deterministic projection, or one
that changed without a version bump. The equivalence check is how such a
snapshot gets caught (see `tests/snapshots.rs::equivalence_check_exposes_a_wrong_snapshot`).

## State diffs

`diff(stream, from, to, options)` reconstructs the earlier position (using
snapshots per `options`), then continues the fold to the later one, so each
event in the interval is read once. It returns a `StateDiff`:

- `changes`: the structural changes that turn the state at `from` into the
  state at `to`. If `from > to`, they describe going back in time.
- `events`: every event with sequence in `(min, max]`.
- `base`: the report for the earlier position.

Both positions must be ≤ head (`TargetBeyondHead`). Diffing an empty stream at
`(0, 0)` returns no changes.

### Structural diff semantics (`diff::diff`)

| Rule | Behaviour |
|------|-----------|
| Paths | [RFC 6901](https://www.rfc-editor.org/rfc/rfc6901) JSON Pointers. `""` is the root, `/items/A-1/on_hand` an object member, `/discontinued/0` an array index. `~` → `~0`, `/` → `~1`. |
| Objects | Key only in the new value → `added`. Key only in the old value → `removed`. Key in both → recurse. |
| Arrays | Compared **by index**. Common indices recurse. Extra trailing elements are `added` or `removed`. There is no alignment, so inserting at the front shows up as modifications plus one addition. This is predictable and linear. |
| Scalars and type changes | One `modified { from, to }` at that path, with nothing reported below it. `1` and `1.0` are different. |
| Order | Depth-first. Object keys are visited in bytewise order (the canonical-JSON order) and array indices ascending. Equal inputs always give identical output, and equal values give `[]`. |

Example (from `examples/time_travel.rs`, inventory between events 3 and 6):

```json
[
  {"path": "/discontinued/0",     "op": "added",    "value": "B-2"},
  {"path": "/items/A-1/on_hand",  "op": "modified", "from": 10, "to": 7},
  {"path": "/items/A-1/reserved", "op": "modified", "from": 0,  "to": 1},
  {"path": "/items/B-2",          "op": "removed",  "value": {"name": "Nut", "on_hand": 4, "reserved": 0}},
  {"path": "/shipped_total",      "op": "modified", "from": 0,  "to": 2}
]
```

## Provenance: which events caused which changes

`explain(stream, from, to, options)` requires `from ≤ to`. It returns one
`Transition { event, changes }` per event in `(from, to]`, where `changes` is
the diff of the state immediately before and after that event. An event with no
visible effect has an empty `changes` list. Its cost grows with the number of
events times the state size, so keep intervals focused.

```text
#4 inventory.reserved     -> ["/items/A-1/on_hand", "/items/A-1/reserved"]
#5 inventory.shipped      -> ["/items/A-1/reserved", "/shipped_total"]
#6 inventory.discontinued -> ["/discontinued/0", "/items/B-2"]
```

## Known limitations

- Reconstruction runs one stream and one projection at a time. There are no
  cross-stream projections.
- The state is held in memory and must fit there.
- `diff`'s `events` list and `explain`'s transitions grow with the interval.
- Array diffs are positional, not LCS-aligned.
- Projections run in-process as Rust code. There is no sandboxing or
  dynamic loading.
