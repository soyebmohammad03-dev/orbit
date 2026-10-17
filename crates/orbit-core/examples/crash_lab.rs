//! Phase 3 in one run: store verification, a trusted checkpoint, a replay
//! experiment, and a replay that catches a projection whose behaviour changed
//! without a version bump.
//!
//! ```sh
//! cargo run -p orbit-core --example crash_lab
//! ```

use orbit_core::projection::examples::{Counter, CounterState};
use orbit_core::replay::{ReplayLab, ReplayOutcome, ReplayStart};
use orbit_core::{
    AppendRequest, EventStore, EventType, Metadata, Projection, ProjectionError, ProjectionKey,
    SnapshotPolicy, StoreConfig, StoredEvent, StreamId, Target,
};
use serde_json::json;

/// Pretends to be `counter@v1`, but double-counts increments of 5 or more.
struct ChangedCounter;

impl Projection for ChangedCounter {
    type State = CounterState;
    fn key(&self) -> ProjectionKey {
        Counter.key()
    }
    fn initial_state(&self) -> CounterState {
        CounterState::default()
    }
    fn apply(
        &self,
        state: CounterState,
        event: &StoredEvent,
    ) -> Result<CounterState, ProjectionError> {
        let next = Counter.apply(state, event)?;
        let by = event.payload["by"].as_i64().unwrap_or(0);
        let doubled = event.event_type.as_str() == "counter.incremented" && by >= 5;
        Ok(CounterState {
            value: next.value + if doubled { by } else { 0 },
        })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = EventStore::open(StoreConfig::new(dir.path().join("crash_lab.db")))?;
    let stream = StreamId::new("counter-1")?;
    store.create_stream(&stream, Metadata::new())?;
    for by in [1, 2, 3, 7, 1, 4] {
        store.append(
            &stream,
            AppendRequest::new(EventType::new("counter.incremented")?, json!({ "by": by })),
        )?;
    }

    // Verification and a checkpoint to keep somewhere the database writer cannot reach.
    let report = store.verify_store()?;
    println!(
        "store valid: {} ({} streams, schema v{})",
        report.is_valid(),
        report.streams.len(),
        report.schema_version
    );
    let checkpoint = store.checkpoint(&stream)?;
    println!("checkpoint: #{} {}", checkpoint.sequence, checkpoint.digest);
    println!(
        "verified against checkpoint: {}",
        store
            .verify_stream_against(&stream, &[checkpoint])?
            .is_valid()
    );

    // A structured experiment.
    let lab = ReplayLab::new(&store, Counter);
    let experiment = lab.experiment(&stream, Target::Latest, SnapshotPolicy::Prefer)?;
    println!(
        "experiment:\n{}",
        serde_json::to_string_pretty(&experiment)?
    );

    // Replaying the recorded run with changed code pinpoints where it diverges.
    let run_id = experiment.run_id.ok_or("experiment recorded no run")?;
    let changed = ReplayLab::new(&store, ChangedCounter).execute(run_id)?;
    match &changed.outcome {
        ReplayOutcome::Diverged {
            first_divergence,
            changes,
        } => println!(
            "changed projection diverged first after event #{} ({}); final diff: {}",
            first_divergence.sequence,
            first_divergence.event_type,
            serde_json::to_string(changes)?
        ),
        other => println!("outcome: {}", other.label()),
    }

    // A run from a reconstructed mid-point, replayed exactly.
    let mid = lab.record(
        &stream,
        ReplayStart::Reconstructed {
            at: orbit_core::Sequence::new(3)?,
            snapshots: SnapshotPolicy::Prefer,
        },
        Target::Latest,
    )?;
    println!("mid-point run: {}", lab.execute(mid.id)?.outcome.label());
    Ok(())
}
