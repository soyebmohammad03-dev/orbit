//! Runtime selection of projections.
//!
//! The engine's [`Reconstructor`] and [`ReplayLab`] are generic over the
//! projection type. [`DynEngine`] is an object-safe facade over them for one
//! projection, so the API can choose a registered projection by name at
//! request time. It contains no logic of its own: every method delegates to
//! the core, converting states to JSON.

use std::sync::Arc;

use orbit_core::projection::MergePatch;
use orbit_core::projection::examples::{Counter, Inventory};
use orbit_core::reconstruct::{
    EquivalenceReport, Explanation, SnapshotCreated, SnapshotStatus, StateDiff,
};
use orbit_core::replay::{
    ExperimentReport, ReplayError, ReplayExecution, ReplayInspection, ReplayLab, ReplayRun,
    ReplayRunId, ReplayStart,
};
use orbit_core::{
    EventStore, Projection, ProjectionKey, ReconstructOptions, ReconstructionError,
    ReconstructionReport, Reconstructor, Sequence, SnapshotId, SnapshotPolicy, StreamId, Target,
};
use serde_json::Value;

/// One projection, usable without knowing its concrete type.
pub trait DynEngine: Send + Sync {
    /// Projection key.
    fn key(&self) -> ProjectionKey;
    /// One-line description for the capabilities endpoint.
    fn description(&self) -> &'static str;
    /// [`Reconstructor::reconstruct`], with the state as JSON.
    fn reconstruct(
        &self,
        store: &EventStore,
        stream: &StreamId,
        target: Target,
        options: ReconstructOptions,
    ) -> Result<(Value, ReconstructionReport), ReconstructionError>;
    /// [`Reconstructor::diff`].
    fn diff(
        &self,
        store: &EventStore,
        stream: &StreamId,
        from: Sequence,
        to: Sequence,
        options: ReconstructOptions,
    ) -> Result<StateDiff, ReconstructionError>;
    /// [`Reconstructor::explain`].
    fn explain(
        &self,
        store: &EventStore,
        stream: &StreamId,
        from: Sequence,
        to: Sequence,
        options: ReconstructOptions,
    ) -> Result<Explanation, ReconstructionError>;
    /// [`Reconstructor::create_snapshot`].
    fn create_snapshot(
        &self,
        store: &EventStore,
        stream: &StreamId,
        target: Target,
        options: ReconstructOptions,
    ) -> Result<SnapshotCreated, ReconstructionError>;
    /// [`Reconstructor::check_snapshot`].
    fn check_snapshot(
        &self,
        store: &EventStore,
        id: SnapshotId,
    ) -> Result<SnapshotStatus, ReconstructionError>;
    /// [`Reconstructor::compare_with_full_replay`].
    fn compare(
        &self,
        store: &EventStore,
        stream: &StreamId,
        target: Target,
        policy: SnapshotPolicy,
    ) -> Result<EquivalenceReport, ReconstructionError>;
    /// [`ReplayLab::record`].
    fn record_replay(
        &self,
        store: &EventStore,
        stream: &StreamId,
        start: ReplayStart,
        to: Target,
    ) -> Result<ReplayRun, ReplayError>;
    /// [`ReplayLab::execute`].
    fn execute_replay(
        &self,
        store: &EventStore,
        run: ReplayRunId,
    ) -> Result<ReplayExecution, ReplayError>;
    /// [`ReplayLab::experiment`].
    fn experiment(
        &self,
        store: &EventStore,
        stream: &StreamId,
        to: Target,
        policy: SnapshotPolicy,
    ) -> Result<ExperimentReport, ReplayError>;
}

struct Engine<P> {
    description: &'static str,
    make: fn() -> P,
}

impl<P: Projection + Send + Sync + 'static> DynEngine for Engine<P> {
    fn key(&self) -> ProjectionKey {
        (self.make)().key()
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn reconstruct(
        &self,
        store: &EventStore,
        stream: &StreamId,
        target: Target,
        options: ReconstructOptions,
    ) -> Result<(Value, ReconstructionReport), ReconstructionError> {
        let r = Reconstructor::new(store, (self.make)()).reconstruct(stream, target, options)?;
        let state =
            serde_json::to_value(&r.state).map_err(|e| ReconstructionError::StateEncoding {
                projection: self.key(),
                sequence: r.report.reconstructed,
                reason: e.to_string(),
            })?;
        Ok((state, r.report))
    }

    fn diff(
        &self,
        store: &EventStore,
        stream: &StreamId,
        from: Sequence,
        to: Sequence,
        options: ReconstructOptions,
    ) -> Result<StateDiff, ReconstructionError> {
        Reconstructor::new(store, (self.make)()).diff(stream, from, to, options)
    }

    fn explain(
        &self,
        store: &EventStore,
        stream: &StreamId,
        from: Sequence,
        to: Sequence,
        options: ReconstructOptions,
    ) -> Result<Explanation, ReconstructionError> {
        Reconstructor::new(store, (self.make)()).explain(stream, from, to, options)
    }

    fn create_snapshot(
        &self,
        store: &EventStore,
        stream: &StreamId,
        target: Target,
        options: ReconstructOptions,
    ) -> Result<SnapshotCreated, ReconstructionError> {
        Reconstructor::new(store, (self.make)()).create_snapshot(stream, target, options)
    }

    fn check_snapshot(
        &self,
        store: &EventStore,
        id: SnapshotId,
    ) -> Result<SnapshotStatus, ReconstructionError> {
        Reconstructor::new(store, (self.make)()).check_snapshot(id)
    }

    fn compare(
        &self,
        store: &EventStore,
        stream: &StreamId,
        target: Target,
        policy: SnapshotPolicy,
    ) -> Result<EquivalenceReport, ReconstructionError> {
        Reconstructor::new(store, (self.make)()).compare_with_full_replay(stream, target, policy)
    }

    fn record_replay(
        &self,
        store: &EventStore,
        stream: &StreamId,
        start: ReplayStart,
        to: Target,
    ) -> Result<ReplayRun, ReplayError> {
        ReplayLab::new(store, (self.make)()).record(stream, start, to)
    }

    fn execute_replay(
        &self,
        store: &EventStore,
        run: ReplayRunId,
    ) -> Result<ReplayExecution, ReplayError> {
        ReplayLab::new(store, (self.make)()).execute(run)
    }

    fn experiment(
        &self,
        store: &EventStore,
        stream: &StreamId,
        to: Target,
        policy: SnapshotPolicy,
    ) -> Result<ExperimentReport, ReplayError> {
        ReplayLab::new(store, (self.make)()).experiment(stream, to, policy)
    }
}

/// The projections the API can run.
#[derive(Clone)]
pub struct Engines {
    engines: Vec<Arc<dyn DynEngine>>,
}

impl std::fmt::Debug for Engines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.engines.iter().map(|e| e.key().to_string()))
            .finish()
    }
}

/// Name of the projection used when a request does not specify one.
pub const DEFAULT_PROJECTION: &str = "merge-patch";

impl Engines {
    /// The built-in projections: `merge-patch@v1` (default, works on any
    /// stream), `counter@v1` and `inventory@v1` (the engine's examples).
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            engines: vec![
                Arc::new(Engine {
                    description: "Applies each event payload as an RFC 7386 JSON Merge Patch to {}; accepts every event.",
                    make: || MergePatch,
                }),
                Arc::new(Engine {
                    description: "Example: counter.incremented / counter.decremented {by} and counter.reset.",
                    make: || Counter,
                }),
                Arc::new(Engine {
                    description: "Example: single-warehouse inventory (inventory.* events) with stock rules.",
                    make: || Inventory,
                }),
            ],
        }
    }

    /// Every registered engine.
    pub fn all(&self) -> &[Arc<dyn DynEngine>] {
        &self.engines
    }

    /// Looks up an engine by `name` or `name@vN`.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<Arc<dyn DynEngine>> {
        self.engines
            .iter()
            .find(|e| {
                let key = e.key();
                name == key.id.as_str() || name == key.to_string()
            })
            .cloned()
    }

    /// Looks up the engine whose key equals `key` exactly.
    #[must_use]
    pub fn for_key(&self, key: &ProjectionKey) -> Option<Arc<dyn DynEngine>> {
        self.engines.iter().find(|e| e.key() == *key).cloned()
    }

    /// Any engine; used for projection-independent replay reads.
    pub(crate) fn any(&self) -> Arc<dyn DynEngine> {
        Arc::clone(&self.engines[0])
    }
}

/// Projection-independent replay reads (decoding runs does not need the projection).
pub(crate) fn inspect_replay(
    store: &EventStore,
    run: ReplayRunId,
) -> Result<ReplayInspection, ReplayError> {
    ReplayLab::new(store, MergePatch).inspect(run)
}

/// All runs of a stream.
pub(crate) fn list_replays(
    store: &EventStore,
    stream: &StreamId,
) -> Result<Vec<ReplayRun>, ReplayError> {
    ReplayLab::new(store, MergePatch).runs(stream)
}
