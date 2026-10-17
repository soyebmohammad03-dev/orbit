//! Deterministic replay: recorded replay runs, re-execution, and divergence
//! classification.
//!
//! A **replay run** pins down one reproducible computation: "projection `K`
//! applied to events `(from, to]` of stream `S`, starting from state `X`,
//! produced state `Y`". It stores the digests of the boundary events, the
//! starting state, the expected final state, and a **trace**: the state
//! digest after every event. All of it is covered by a record digest
//! (`ORBIT-REPLAY-RUN-DIGEST-V1`).
//!
//! **Executing** a run re-applies the stored events to the stored starting
//! state with the *current* projection code. It compares the result, event by
//! event, with the trace, and classifies the outcome as a [`ReplayOutcome`].
//! Every execution is persisted, so results can be inspected later.
//!
//! What a divergence means: the trace gives the **first event after which the
//! state digest differs**. That is where the divergence became observable.
//! ORBIT does not claim that this event *caused* the divergence. A code
//! change may have altered behaviour that only this event exercised, or the
//! state may have differed in a part that earlier events left untouched.
//!
//! States are compared by digest of their canonical JSON
//! (`ORBIT-STATE-DIGEST-V1`). Two states are "the same" exactly when their
//! canonical JSON is identical.

use std::fmt;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::diff::{Change, diff};
use crate::domain::canonical::to_canonical_string;
use crate::domain::{Digest, EventId, EventType, Sequence, StreamHead, StreamId, Timestamp};
use crate::error::{StoreError, ValidationError};
use crate::integrity::{IntegrityCategory, IntegrityReport};
use crate::projection::Projection;
use crate::reconstruct::{
    ProjectionFailure, ReconstructOptions, ReconstructionError, Reconstructor, SnapshotPolicy,
    StartPoint, Target,
};
use crate::snapshot::{ProjectionKey, SnapshotCheck, SnapshotId};
use crate::store::EventStore;

/// Maximum number of events in one replay run (the trace stores 32 bytes per event).
pub const MAX_REPLAY_EVENTS: u64 = 100_000;
/// Domain tag of [`state_digest`].
pub const STATE_DIGEST_DOMAIN_TAG: &str = "ORBIT-STATE-DIGEST-V1";
/// Domain tag of the replay-run record digest.
pub const RUN_DIGEST_DOMAIN_TAG: &str = "ORBIT-REPLAY-RUN-DIGEST-V1";

/// Digest of a JSON state: `SHA-256(lp(tag) || lp(canonical JSON))`.
#[must_use]
pub fn state_digest(state: &Value) -> Digest {
    let text = to_canonical_string(state);
    let mut hasher = Sha256::new();
    lp(&mut hasher, STATE_DIGEST_DOMAIN_TAG.as_bytes());
    lp(&mut hasher, text.as_bytes());
    Digest::from_bytes(hasher.finalize().into())
}

fn lp(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

macro_rules! uuid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a new time-based (UUIDv7) id.
            #[must_use]
            pub fn generate() -> Self {
                Self(Uuid::now_v7())
            }

            /// The underlying UUID.
            #[must_use]
            pub fn as_uuid(&self) -> Uuid {
                self.0
            }

            /// The 16-byte representation used for storage.
            #[must_use]
            pub fn as_bytes(&self) -> &[u8; 16] {
                self.0.as_bytes()
            }

            pub(crate) fn from_slice(bytes: &[u8]) -> Option<Self> {
                Uuid::from_slice(bytes).ok().filter(|u| !u.is_nil()).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

uuid_id!(
    /// Identifier of a recorded [`ReplayRun`].
    ReplayRunId
);
uuid_id!(
    /// Identifier of one [`ReplayExecution`].
    ReplayExecutionId
);

/// Where a recorded run takes its starting state from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ReplayStart {
    /// The projection's initial state at sequence 0.
    Initial,
    /// A validated snapshot; the run starts at the snapshot's sequence.
    Snapshot {
        /// The snapshot.
        snapshot_id: SnapshotId,
    },
    /// The state reconstructed at `at`, using `snapshots`.
    Reconstructed {
        /// Start position.
        at: Sequence,
        /// Snapshot policy for reconstructing the start state.
        snapshots: SnapshotPolicy,
    },
}

/// Provenance of a run's starting state, as recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ReplayStartSource {
    /// Initial state at sequence 0.
    Initial,
    /// Taken from this snapshot.
    Snapshot {
        /// The snapshot.
        snapshot_id: SnapshotId,
    },
    /// Reconstructed from history (possibly starting from a snapshot).
    Reconstructed {
        /// Snapshot the reconstruction started from, if any.
        via_snapshot: Option<SnapshotId>,
    },
}

impl ReplayStartSource {
    fn as_db(self) -> (&'static str, Option<SnapshotId>) {
        match self {
            Self::Initial => ("initial", None),
            Self::Snapshot { snapshot_id } => ("snapshot", Some(snapshot_id)),
            Self::Reconstructed { via_snapshot } => ("reconstructed", via_snapshot),
        }
    }
}

/// A recorded, reproducible replay computation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplayRun {
    /// Run identifier.
    pub id: ReplayRunId,
    /// Stream replayed.
    pub stream_id: StreamId,
    /// Projection that produced the expected results.
    pub projection: ProjectionKey,
    /// Start position (exclusive).
    pub from: Sequence,
    /// End position (inclusive).
    pub to: Sequence,
    /// Digest of the event at `from` ([`Digest::GENESIS`] at 0).
    pub from_event_digest: Digest,
    /// Digest of the event at `to`.
    pub to_event_digest: Digest,
    /// Where the starting state came from.
    pub start_source: ReplayStartSource,
    /// Starting state.
    pub start_state: Value,
    /// [`state_digest`] of `start_state`.
    pub start_state_digest: Digest,
    /// Expected final state.
    pub expected_state: Value,
    /// [`state_digest`] of `expected_state`.
    pub expected_state_digest: Digest,
    /// State digest after each event `from + 1 ..= to`.
    pub trace: Vec<Digest>,
    /// When the run was recorded (informational).
    pub created_at: Timestamp,
    /// Digest over the whole record.
    pub record_digest: Digest,
}

/// A replay-run row exactly as stored.
#[derive(Debug, Clone)]
pub(crate) struct RawReplayRun {
    pub(crate) run_id: Vec<u8>,
    pub(crate) stream_id: String,
    pub(crate) projection_id: String,
    pub(crate) projection_version: i64,
    pub(crate) from_sequence: i64,
    pub(crate) to_sequence: i64,
    pub(crate) from_event_digest: Vec<u8>,
    pub(crate) to_event_digest: Vec<u8>,
    pub(crate) start_source: String,
    pub(crate) start_snapshot_id: Option<Vec<u8>>,
    pub(crate) start_state: String,
    pub(crate) start_state_digest: Vec<u8>,
    pub(crate) expected_state: String,
    pub(crate) expected_state_digest: Vec<u8>,
    pub(crate) trace: Vec<u8>,
    pub(crate) created_at: i64,
    pub(crate) record_digest: Vec<u8>,
}

impl RawReplayRun {
    pub(crate) fn display_id(&self) -> String {
        Uuid::from_slice(&self.run_id).map_or_else(
            |_| self.run_id.iter().map(|b| format!("{b:02x}")).collect(),
            |u| u.to_string(),
        )
    }

    /// `ORBIT-REPLAY-RUN-DIGEST-V1` over the stored columns (every column
    /// except `record_digest`, in declaration order; blobs and text
    /// length-prefixed, integers as big-endian `i64`, the optional snapshot
    /// id as a presence byte followed by its length-prefixed bytes).
    pub(crate) fn computed_digest(&self) -> Digest {
        let mut h = Sha256::new();
        lp(&mut h, RUN_DIGEST_DOMAIN_TAG.as_bytes());
        lp(&mut h, &self.run_id);
        lp(&mut h, self.stream_id.as_bytes());
        lp(&mut h, self.projection_id.as_bytes());
        h.update(self.projection_version.to_be_bytes());
        h.update(self.from_sequence.to_be_bytes());
        h.update(self.to_sequence.to_be_bytes());
        lp(&mut h, &self.from_event_digest);
        lp(&mut h, &self.to_event_digest);
        lp(&mut h, self.start_source.as_bytes());
        match &self.start_snapshot_id {
            None => h.update([0u8]),
            Some(id) => {
                h.update([1u8]);
                lp(&mut h, id);
            }
        }
        lp(&mut h, self.start_state.as_bytes());
        lp(&mut h, &self.start_state_digest);
        lp(&mut h, self.expected_state.as_bytes());
        lp(&mut h, &self.expected_state_digest);
        lp(&mut h, &self.trace);
        h.update(self.created_at.to_be_bytes());
        Digest::from_bytes(h.finalize().into())
    }

    /// Decodes every field and checks the record's internal consistency
    /// (state digests, trace length and final trace entry). Does not check
    /// the record digest.
    pub(crate) fn decode(&self) -> Result<ReplayRun, String> {
        let digest = |name: &str, bytes: &[u8]| {
            Digest::from_slice(bytes).map_err(|e| format!("{name}: {e}"))
        };
        let id = ReplayRunId::from_slice(&self.run_id).ok_or("run id is not a valid UUID")?;
        let stream_id =
            StreamId::new(self.stream_id.as_str()).map_err(|e| format!("stream id: {e}"))?;
        let version = u32::try_from(self.projection_version)
            .map_err(|_| format!("projection version {} is invalid", self.projection_version))?;
        let projection =
            ProjectionKey::new(&self.projection_id, version).map_err(|e| e.to_string())?;
        let from = Sequence::from_i64(self.from_sequence).ok_or("negative from_sequence")?;
        let to = Sequence::from_i64(self.to_sequence).ok_or("negative to_sequence")?;
        if to < from {
            return Err(format!("to_sequence {to} is before from_sequence {from}"));
        }
        let snapshot = match &self.start_snapshot_id {
            None => None,
            Some(bytes) => Some(
                Uuid::from_slice(bytes)
                    .ok()
                    .and_then(|u| SnapshotId::from_uuid(u).ok())
                    .ok_or("start snapshot id is not a valid UUID")?,
            ),
        };
        let start_source = match (self.start_source.as_str(), snapshot) {
            ("initial", None) if from == Sequence::ZERO => ReplayStartSource::Initial,
            ("snapshot", Some(snapshot_id)) => ReplayStartSource::Snapshot { snapshot_id },
            ("reconstructed", via_snapshot) => ReplayStartSource::Reconstructed { via_snapshot },
            (other, _) => return Err(format!("inconsistent start source {other:?}")),
        };
        let start_state: Value =
            serde_json::from_str(&self.start_state).map_err(|e| format!("start state: {e}"))?;
        let expected_state: Value = serde_json::from_str(&self.expected_state)
            .map_err(|e| format!("expected state: {e}"))?;
        let start_state_digest = digest("start_state_digest", &self.start_state_digest)?;
        let expected_state_digest = digest("expected_state_digest", &self.expected_state_digest)?;
        if !self.trace.len().is_multiple_of(32) {
            return Err("trace length is not a multiple of 32".to_owned());
        }
        let trace: Vec<Digest> = self
            .trace
            .as_chunks::<32>()
            .0
            .iter()
            .map(|c| digest("trace", c))
            .collect::<Result<_, _>>()?;
        if trace.len() as u64 != to.get() - from.get() {
            return Err(format!(
                "trace has {} entries for {} events",
                trace.len(),
                to.get() - from.get()
            ));
        }
        if state_digest(&start_state) != start_state_digest {
            return Err("start state does not match its digest".to_owned());
        }
        if state_digest(&expected_state) != expected_state_digest {
            return Err("expected state does not match its digest".to_owned());
        }
        if trace.last().copied().unwrap_or(start_state_digest) != expected_state_digest {
            return Err("trace does not end at the expected state".to_owned());
        }
        Ok(ReplayRun {
            id,
            stream_id,
            projection,
            from,
            to,
            from_event_digest: digest("from_event_digest", &self.from_event_digest)?,
            to_event_digest: digest("to_event_digest", &self.to_event_digest)?,
            start_source,
            start_state,
            start_state_digest,
            expected_state,
            expected_state_digest,
            trace,
            created_at: Timestamp::from_unix_micros(self.created_at).map_err(|e| e.to_string())?,
            record_digest: digest("record_digest", &self.record_digest)?,
        })
    }
}

/// The first event after which the replayed state digest differs from the trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DivergencePoint {
    /// Sequence of the event.
    pub sequence: Sequence,
    /// Its id.
    pub event_id: EventId,
    /// Its type.
    pub event_type: EventType,
    /// Recorded state digest after this event.
    pub expected_state_digest: Digest,
    /// Replayed state digest after this event.
    pub actual_state_digest: Digest,
    /// Events whose resulting state matched before this one.
    pub matched_events: u64,
}

/// Why a run cannot be replayed with the current setup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum UnavailableReason {
    /// The run was recorded by a different projection or version. ORBIT
    /// never substitutes one projection version for another.
    ProjectionMismatch {
        /// Key recorded in the run.
        recorded: ProjectionKey,
        /// Key of the projection attempting the replay.
        current: ProjectionKey,
    },
    /// The stream no longer has the events the run needs.
    HistoryUnavailable {
        /// Last sequence required.
        required: Sequence,
        /// Current head.
        head: Sequence,
    },
}

/// An integrity problem that prevents a trustworthy replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum IntegrityViolation {
    /// The run record was modified after it was recorded.
    RunRecord {
        /// Stored record digest.
        stored: Digest,
        /// Recomputed digest.
        computed: Digest,
    },
    /// A boundary event of the interval is missing or has a different digest
    /// than when the run was recorded.
    Boundary {
        /// `"from"` or `"to"`.
        which: &'static str,
        /// Sequence of the boundary.
        sequence: Sequence,
        /// Digest recorded in the run.
        expected: Digest,
        /// Digest found, if the event exists.
        actual: Option<Digest>,
    },
    /// The events in the interval fail verification.
    History {
        /// Categories of the issues found.
        categories: Vec<IntegrityCategory>,
        /// Full report for the interval.
        report: IntegrityReport,
    },
}

/// Classification of one replay execution.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ReplayOutcome {
    /// Every intermediate state and the final state match the recording.
    ExactMatch,
    /// Intermediate states diverged but the final state matches.
    Reconverged {
        /// First mismatching event.
        first_divergence: DivergencePoint,
    },
    /// The final state differs from the recording.
    Diverged {
        /// First mismatching event (the divergence became observable here).
        first_divergence: DivergencePoint,
        /// Structural diff from the expected to the actual final state.
        changes: Vec<Change>,
    },
    /// The current projection rejected an event that it accepted when the run
    /// was recorded.
    ProjectionFailed {
        /// The failure.
        failure: ProjectionFailure,
        /// First mismatch before the failure, if any.
        first_divergence: Option<DivergencePoint>,
    },
    /// The replay cannot be performed with the current projection or history.
    Unavailable {
        /// Why.
        reason: UnavailableReason,
    },
    /// The run record is malformed or internally inconsistent, or its
    /// starting state cannot be decoded by the projection.
    InvalidInput {
        /// Description.
        reason: String,
    },
    /// The run record or the replayed history failed integrity checks.
    IntegrityFailure {
        /// What failed.
        violations: Vec<IntegrityViolation>,
    },
}

impl ReplayOutcome {
    /// Stable snake-case label, as stored in `replay_executions.outcome`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::ExactMatch => "exact_match",
            Self::Reconverged { .. } => "reconverged",
            Self::Diverged { .. } => "diverged",
            Self::ProjectionFailed { .. } => "projection_failed",
            Self::Unavailable { .. } => "unavailable",
            Self::InvalidInput { .. } => "invalid_input",
            Self::IntegrityFailure { .. } => "integrity_failure",
        }
    }

    /// `true` for [`ExactMatch`](Self::ExactMatch) and [`Reconverged`](Self::Reconverged).
    #[must_use]
    pub fn final_state_matches(&self) -> bool {
        matches!(self, Self::ExactMatch | Self::Reconverged { .. })
    }
}

/// The result of executing a replay run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplayExecution {
    /// Execution identifier.
    pub id: ReplayExecutionId,
    /// Run executed.
    pub run_id: ReplayRunId,
    /// When it was executed.
    pub executed_at: Timestamp,
    /// Projection used for this execution.
    pub projection: ProjectionKey,
    /// Stream, as recorded (absent if the record is undecodable).
    pub stream_id: Option<StreamId>,
    /// Recorded interval `(from, to]`, if decodable.
    pub interval: Option<(Sequence, Sequence)>,
    /// Classification.
    pub outcome: ReplayOutcome,
    /// Recorded final-state digest, if decodable.
    pub expected_state_digest: Option<Digest>,
    /// Replayed final-state digest, if the replay ran to completion.
    pub actual_state_digest: Option<Digest>,
    /// Last position whose state was computed.
    pub actual_position: Option<Sequence>,
    /// Events applied.
    pub events_applied: u64,
    /// Non-fatal observations (e.g. the start snapshot was since deleted or corrupted).
    pub warnings: Vec<String>,
    /// Wall-clock duration in microseconds.
    pub elapsed_micros: u64,
}

/// An execution as read back from storage. `report` is the serialized
/// [`ReplayExecution`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredExecution {
    /// Execution id.
    pub id: ReplayExecutionId,
    /// Run id.
    pub run_id: ReplayRunId,
    /// When it ran.
    pub executed_at: Timestamp,
    /// Outcome label ([`ReplayOutcome::label`]).
    pub outcome: String,
    /// Replayed final-state digest, if any.
    pub actual_state_digest: Option<Digest>,
    /// Full serialized execution report.
    pub report: Value,
}

impl StoredExecution {
    pub(crate) fn decode(
        run_id: ReplayRunId,
        id: &[u8],
        executed_at: i64,
        outcome: String,
        digest: Option<&[u8]>,
        report: &str,
    ) -> Result<Self, StoreError> {
        let corrupt = |reason: String| StoreError::CorruptReplayRecord {
            record: format!("execution of run {run_id}"),
            reason,
        };
        Ok(Self {
            id: ReplayExecutionId::from_slice(id).ok_or_else(|| corrupt("execution id".into()))?,
            run_id,
            executed_at: Timestamp::from_unix_micros(executed_at)
                .map_err(|e| corrupt(e.to_string()))?,
            outcome,
            actual_state_digest: digest
                .map(|d| Digest::from_slice(d).map_err(|e| corrupt(e.to_string())))
                .transpose()?,
            report: serde_json::from_str(report).map_err(|e| corrupt(e.to_string()))?,
        })
    }
}

/// A run with all its executions.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplayInspection {
    /// The run.
    pub run: ReplayRun,
    /// Its executions, oldest first.
    pub executions: Vec<StoredExecution>,
}

/// Summary of a stream's integrity in an [`ExperimentReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegritySummary {
    /// `true` if the stream verified cleanly.
    pub valid: bool,
    /// Events checked.
    pub events_checked: u64,
    /// Total issues.
    pub total_issues: u64,
    /// Issue categories.
    pub categories: Vec<IntegrityCategory>,
}

/// Snapshot-assisted versus full-replay comparison in an [`ExperimentReport`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EquivalenceSummary {
    /// Both reconstructions produced the same state.
    pub equivalent: bool,
    /// Where the snapshot-assisted reconstruction started.
    pub assisted_start: StartPoint,
    /// Events applied by the snapshot-assisted reconstruction.
    pub assisted_events_applied: u64,
    /// Snapshots rejected during selection.
    pub snapshot_warnings: usize,
    /// State digest of the full replay.
    pub full_state_digest: Digest,
    /// State digest of the snapshot-assisted reconstruction.
    pub assisted_state_digest: Digest,
    /// Number of structural differences.
    pub differences: usize,
}

/// Timings of an experiment's phases, in microseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ExperimentTimings {
    /// `verify_stream`.
    pub verify_micros: u64,
    /// Snapshot-assisted versus full-replay comparison.
    pub equivalence_micros: u64,
    /// Recording the replay run.
    pub record_micros: u64,
    /// Executing it.
    pub replay_micros: u64,
}

/// Structured result of [`ReplayLab::experiment`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExperimentReport {
    /// Experiment identifier (the replay execution id, when a replay ran).
    pub experiment_id: Option<ReplayExecutionId>,
    /// Replay run recorded for this experiment.
    pub run_id: Option<ReplayRunId>,
    /// Stream.
    pub stream_id: StreamId,
    /// Interval `(0, to]`.
    pub interval: (Sequence, Sequence),
    /// Projection.
    pub projection: ProjectionKey,
    /// Snapshot policy evaluated.
    pub snapshot_policy: SnapshotPolicy,
    /// Integrity verification.
    pub integrity: IntegritySummary,
    /// Snapshot-assisted versus full replay, if integrity allowed it.
    pub equivalence: Option<EquivalenceSummary>,
    /// Replay outcome label, if a replay ran.
    pub replay_outcome: Option<String>,
    /// Timings.
    pub timings: ExperimentTimings,
    /// Explanations for skipped phases.
    pub notes: Vec<String>,
}

/// Failure to *perform* a replay-lab operation. Replay results, including
/// divergences and integrity failures found while executing, are
/// [`ReplayOutcome`]s, not errors.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    /// Store failure, including not-found errors.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Reconstructing the starting state or recording the run failed.
    #[error(transparent)]
    Reconstruction(#[from] ReconstructionError),
    /// The interval to record does not verify; recording it would bake
    /// damaged history into the expectation.
    #[error("refusing to record a replay over damaged history ({} issues)", .0.total_issues)]
    HistoryIntegrity(Box<IntegrityReport>),
    /// `from` is after `to`.
    #[error("invalid replay interval: start {from} is after end {to}")]
    InvalidRange {
        /// Start.
        from: Sequence,
        /// End.
        to: Sequence,
    },
}

/// Records, executes and inspects replay runs for one projection.
#[derive(Debug)]
pub struct ReplayLab<'s, P> {
    store: &'s EventStore,
    reconstructor: Reconstructor<'s, P>,
}

impl<'s, P: Projection> ReplayLab<'s, P> {
    /// Creates a lab for `projection` over `store`.
    pub fn new(store: &'s EventStore, projection: P) -> Self {
        Self {
            store,
            reconstructor: Reconstructor::new(store, projection),
        }
    }

    /// The underlying reconstructor.
    pub fn reconstructor(&self) -> &Reconstructor<'s, P> {
        &self.reconstructor
    }

    fn key(&self) -> ProjectionKey {
        self.reconstructor.projection().key()
    }

    fn encode(&self, state: &P::State, sequence: Sequence) -> Result<Value, ReconstructionError> {
        serde_json::to_value(state).map_err(|e| ReconstructionError::StateEncoding {
            projection: self.key(),
            sequence,
            reason: e.to_string(),
        })
    }

    /// Records a replay run over `(start, to]` and persists it.
    ///
    /// The interval is verified first (event digests and chain from the
    /// start position); damaged history is refused. The start state comes
    /// from `start`: the initial state, a validated snapshot (which must not
    /// be after `to`), or a reconstruction.
    ///
    /// # Errors
    /// [`ReplayError`]; e.g. `TargetBeyondHead`, `InvalidRange`, an unusable
    /// snapshot, a projection failure inside the interval, or
    /// [`ValidationError::ReplayIntervalTooLarge`].
    pub fn record(
        &self,
        stream: &StreamId,
        start: ReplayStart,
        to: Target,
    ) -> Result<ReplayRun, ReplayError> {
        let head = self.store.head(stream)?;
        let to = match to {
            Target::Latest => head.sequence,
            Target::At(n) if n <= head.sequence => n,
            Target::At(n) => {
                return Err(ReconstructionError::TargetBeyondHead {
                    stream: stream.clone(),
                    target: n,
                    head: head.sequence,
                }
                .into());
            }
        };

        let (state, from, source) = match start {
            ReplayStart::Initial => (
                self.reconstructor.projection().initial_state(),
                StreamHead::EMPTY,
                ReplayStartSource::Initial,
            ),
            ReplayStart::Snapshot { snapshot_id } => {
                let sequence = self.store.snapshot(snapshot_id)?.sequence;
                if sequence > to {
                    return Err(ReplayError::InvalidRange { from: sequence, to });
                }
                let r = self.reconstructor.reconstruct(
                    stream,
                    Target::At(sequence),
                    ReconstructOptions::with_snapshots(SnapshotPolicy::Exact(snapshot_id)),
                )?;
                let head = StreamHead {
                    sequence,
                    digest: r.report.reconstructed_digest,
                };
                (r.state, head, ReplayStartSource::Snapshot { snapshot_id })
            }
            ReplayStart::Reconstructed { at, snapshots } => {
                if at > to {
                    return Err(ReplayError::InvalidRange { from: at, to });
                }
                let r = self.reconstructor.reconstruct(
                    stream,
                    Target::At(at),
                    ReconstructOptions::with_snapshots(snapshots),
                )?;
                let via_snapshot = match r.report.start {
                    StartPoint::Snapshot { snapshot_id, .. } => Some(snapshot_id),
                    StartPoint::Initial => None,
                };
                let head = StreamHead {
                    sequence: at,
                    digest: r.report.reconstructed_digest,
                };
                (
                    r.state,
                    head,
                    ReplayStartSource::Reconstructed { via_snapshot },
                )
            }
        };
        let events = to.get() - from.sequence.get();
        if events > MAX_REPLAY_EVENTS {
            return Err(StoreError::from(ValidationError::ReplayIntervalTooLarge {
                actual: events,
                max: MAX_REPLAY_EVENTS,
            })
            .into());
        }

        let (report, _) = self.store.verify_range(stream, from, to)?;
        if !report.is_valid() {
            return Err(ReplayError::HistoryIntegrity(Box::new(report)));
        }

        let start_state = self.encode(&state, from.sequence)?;
        let mut trace = Vec::with_capacity(usize::try_from(events).unwrap_or(0));
        let (final_state, end) =
            self.reconstructor
                .fold_from(stream, state, from, to, |event, state| {
                    trace.push(state_digest(&self.encode(state, event.sequence)?));
                    Ok(())
                })?;
        let expected_state = self.encode(&final_state, to)?;

        let id = ReplayRunId::generate();
        let key = self.key();
        let (start_source, start_snapshot) = source.as_db();
        let mut raw = RawReplayRun {
            run_id: id.as_bytes().to_vec(),
            stream_id: stream.to_string(),
            projection_id: key.id.to_string(),
            projection_version: i64::from(key.version),
            from_sequence: from.sequence.to_i64(),
            to_sequence: to.to_i64(),
            from_event_digest: from.digest.as_bytes().to_vec(),
            to_event_digest: end.digest.as_bytes().to_vec(),
            start_source: start_source.to_owned(),
            start_snapshot_id: start_snapshot.map(|s| s.as_bytes().to_vec()),
            start_state: to_canonical_string(&start_state),
            start_state_digest: state_digest(&start_state).as_bytes().to_vec(),
            expected_state: to_canonical_string(&expected_state),
            expected_state_digest: state_digest(&expected_state).as_bytes().to_vec(),
            trace: trace.iter().flat_map(|d| *d.as_bytes()).collect(),
            created_at: Timestamp::now().map_err(StoreError::from)?.unix_micros(),
            record_digest: Vec::new(),
        };
        raw.record_digest = raw.computed_digest().as_bytes().to_vec();
        self.store.insert_replay_run(&raw)?;
        tracing::debug!(run = %id, stream = %stream, from = %from.sequence, %to, "recorded replay run");
        raw.decode().map_err(|reason| {
            StoreError::CorruptReplayRecord {
                record: id.to_string(),
                reason,
            }
            .into()
        })
    }

    /// Loads and decodes a run.
    ///
    /// # Errors
    /// [`StoreError::ReplayRunNotFound`] or [`StoreError::CorruptReplayRecord`].
    pub fn run(&self, id: ReplayRunId) -> Result<ReplayRun, ReplayError> {
        let raw = self.store.load_replay_run(id)?;
        raw.decode().map_err(|reason| {
            StoreError::CorruptReplayRecord {
                record: raw.display_id(),
                reason,
            }
            .into()
        })
    }

    /// All runs of a stream, oldest first.
    ///
    /// # Errors
    /// [`StoreError::StreamNotFound`] or [`StoreError::CorruptReplayRecord`].
    pub fn runs(&self, stream: &StreamId) -> Result<Vec<ReplayRun>, ReplayError> {
        self.store
            .replay_runs_of(stream)?
            .iter()
            .map(|raw| {
                raw.decode().map_err(|reason| {
                    StoreError::CorruptReplayRecord {
                        record: raw.display_id(),
                        reason,
                    }
                    .into()
                })
            })
            .collect()
    }

    /// A run with all of its executions.
    ///
    /// # Errors
    /// As [`run`](Self::run).
    pub fn inspect(&self, id: ReplayRunId) -> Result<ReplayInspection, ReplayError> {
        Ok(ReplayInspection {
            run: self.run(id)?,
            executions: self.store.replay_executions(id)?,
        })
    }

    /// Executes a run with the current projection, classifies the outcome,
    /// and persists the execution.
    ///
    /// # Errors
    /// Only if the run does not exist or the store fails. Everything found
    /// while executing (divergence, damaged history, incompatible projection,
    /// corrupt record) is reported in [`ReplayExecution::outcome`].
    pub fn execute(&self, id: ReplayRunId) -> Result<ReplayExecution, ReplayError> {
        let started = Instant::now();
        let raw = self.store.load_replay_run(id)?;
        let mut execution = ReplayExecution {
            id: ReplayExecutionId::generate(),
            run_id: id,
            executed_at: Timestamp::now().map_err(StoreError::from)?,
            projection: self.key(),
            stream_id: None,
            interval: None,
            outcome: ReplayOutcome::ExactMatch,
            expected_state_digest: None,
            actual_state_digest: None,
            actual_position: None,
            events_applied: 0,
            warnings: Vec::new(),
            elapsed_micros: 0,
        };
        execution.outcome = self.evaluate(&raw, &mut execution)?;
        execution.elapsed_micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);

        let report =
            serde_json::to_string(&execution).map_err(|e| StoreError::CorruptReplayRecord {
                record: id.to_string(),
                reason: e.to_string(),
            })?;
        self.store.insert_replay_execution(
            execution.id,
            id,
            execution.executed_at,
            execution.outcome.label(),
            execution.actual_state_digest.as_ref().map(Digest::as_bytes),
            &report,
        )?;
        match &execution.outcome {
            ReplayOutcome::ExactMatch => {
                tracing::debug!(run = %id, "replay matched");
            }
            other => {
                tracing::warn!(run = %id, outcome = other.label(), "replay did not match exactly")
            }
        }
        Ok(execution)
    }

    fn evaluate(
        &self,
        raw: &RawReplayRun,
        execution: &mut ReplayExecution,
    ) -> Result<ReplayOutcome, ReplayError> {
        // 1. The record itself.
        let stored = Digest::from_slice(&raw.record_digest).unwrap_or(Digest::GENESIS);
        let computed = raw.computed_digest();
        if stored != computed {
            return Ok(ReplayOutcome::IntegrityFailure {
                violations: vec![IntegrityViolation::RunRecord { stored, computed }],
            });
        }
        let run = match raw.decode() {
            Ok(run) => run,
            Err(reason) => return Ok(ReplayOutcome::InvalidInput { reason }),
        };
        execution.stream_id = Some(run.stream_id.clone());
        execution.interval = Some((run.from, run.to));
        execution.expected_state_digest = Some(run.expected_state_digest);

        // 2. Same projection, same version. Never substitute.
        let current = self.key();
        if run.projection != current {
            return Ok(ReplayOutcome::Unavailable {
                reason: UnavailableReason::ProjectionMismatch {
                    recorded: run.projection,
                    current,
                },
            });
        }

        // 3. The history the run was recorded over.
        let head = self.store.head(&run.stream_id)?;
        if head.sequence < run.to {
            return Ok(ReplayOutcome::Unavailable {
                reason: UnavailableReason::HistoryUnavailable {
                    required: run.to,
                    head: head.sequence,
                },
            });
        }
        let mut violations = Vec::new();
        if run.from > Sequence::ZERO {
            let actual = self.event_digest(&run.stream_id, run.from)?;
            if actual != Some(run.from_event_digest) {
                violations.push(IntegrityViolation::Boundary {
                    which: "from",
                    sequence: run.from,
                    expected: run.from_event_digest,
                    actual,
                });
            }
        }
        let start = StreamHead {
            sequence: run.from,
            digest: run.from_event_digest,
        };
        let (report, last) = self.store.verify_range(&run.stream_id, start, run.to)?;
        if !report.is_valid() {
            violations.push(IntegrityViolation::History {
                categories: report.categories(),
                report,
            });
        }
        if run.to > run.from
            && last
                != (StreamHead {
                    sequence: run.to,
                    digest: run.to_event_digest,
                })
        {
            let actual = self.event_digest(&run.stream_id, run.to)?;
            if actual != Some(run.to_event_digest) {
                violations.push(IntegrityViolation::Boundary {
                    which: "to",
                    sequence: run.to,
                    expected: run.to_event_digest,
                    actual,
                });
            }
        }
        if !violations.is_empty() {
            return Ok(ReplayOutcome::IntegrityFailure { violations });
        }

        // 4. Provenance of the start state (non-fatal: the run stores the state itself).
        self.check_start_provenance(&run, &mut execution.warnings)?;

        // 5. Re-execute and compare against the trace.
        let state = match P::State::deserialize(&run.start_state) {
            Ok(state) => state,
            Err(e) => {
                return Ok(ReplayOutcome::InvalidInput {
                    reason: format!("start state cannot be decoded by {current}: {e}"),
                });
            }
        };
        let mut first_divergence: Option<DivergencePoint> = None;
        let mut index = 0usize;
        let mut position = run.from;
        let result =
            self.reconstructor
                .fold_from(&run.stream_id, state, start, run.to, |event, state| {
                    let actual = state_digest(&self.encode(state, event.sequence)?);
                    let expected = run.trace.get(index).copied().unwrap_or(Digest::GENESIS);
                    if first_divergence.is_none() && actual != expected {
                        first_divergence = Some(DivergencePoint {
                            sequence: event.sequence,
                            event_id: event.event_id,
                            event_type: event.event_type.clone(),
                            expected_state_digest: expected,
                            actual_state_digest: actual,
                            matched_events: index as u64,
                        });
                    }
                    index += 1;
                    position = event.sequence;
                    Ok(())
                });
        execution.events_applied = index as u64;
        execution.actual_position = Some(position);
        let final_state = match result {
            Ok((state, _)) => state,
            Err(ReconstructionError::Projection(failure)) => {
                return Ok(ReplayOutcome::ProjectionFailed {
                    failure: *failure,
                    first_divergence,
                });
            }
            Err(ReconstructionError::HistoryInconsistent {
                reason, sequence, ..
            }) => {
                // Unreachable after verify_range unless history changed concurrently.
                return Ok(ReplayOutcome::InvalidInput {
                    reason: format!("history inconsistent at {sequence}: {reason}"),
                });
            }
            Err(other) => return Err(other.into()),
        };
        let actual_value = self.encode(&final_state, run.to)?;
        let actual = state_digest(&actual_value);
        execution.actual_state_digest = Some(actual);

        Ok(match first_divergence {
            None => ReplayOutcome::ExactMatch,
            Some(first_divergence) if actual == run.expected_state_digest => {
                ReplayOutcome::Reconverged { first_divergence }
            }
            Some(first_divergence) => ReplayOutcome::Diverged {
                first_divergence,
                changes: diff(&run.expected_state, &actual_value),
            },
        })
    }

    fn event_digest(
        &self,
        stream: &StreamId,
        sequence: Sequence,
    ) -> Result<Option<Digest>, ReplayError> {
        match self.store.event(stream, sequence) {
            Ok(event) => Ok(Some(event.digest)),
            Err(StoreError::EventNotFound { .. } | StoreError::CorruptRecord { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn check_start_provenance(
        &self,
        run: &ReplayRun,
        warnings: &mut Vec<String>,
    ) -> Result<(), ReplayError> {
        let (snapshot_id, must_equal_start) = match run.start_source {
            ReplayStartSource::Snapshot { snapshot_id } => (snapshot_id, true),
            ReplayStartSource::Reconstructed {
                via_snapshot: Some(snapshot_id),
            } => (snapshot_id, false),
            _ => return Ok(()),
        };
        match self.store.check_snapshot(snapshot_id) {
            Err(StoreError::SnapshotNotFound(_)) => {
                warnings.push(format!("start snapshot {snapshot_id} no longer exists"));
            }
            Err(e) => return Err(e.into()),
            Ok(SnapshotCheck::Unusable { problem, .. }) => {
                warnings.push(format!(
                    "start snapshot {snapshot_id} is now unusable: {problem}"
                ));
            }
            Ok(SnapshotCheck::Intact { snapshot }) => {
                if must_equal_start && state_digest(&snapshot.state) != run.start_state_digest {
                    warnings.push(format!(
                        "start snapshot {snapshot_id} no longer matches the recorded start state"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Runs a complete, structured experiment on `(0, to]`:
    ///
    /// 1. verify the stream's integrity;
    /// 2. compare snapshot-assisted reconstruction under `policy` with full replay;
    /// 3. record a replay run from the initial state;
    /// 4. execute it.
    ///
    /// Steps 2–4 are skipped (with a note) when integrity verification fails.
    /// The run and its execution are persisted; the report is returned.
    ///
    /// # Errors
    /// Store failures, or reconstruction errors such as `TargetBeyondHead`
    /// or a projection failure during the equivalence step.
    pub fn experiment(
        &self,
        stream: &StreamId,
        to: Target,
        policy: SnapshotPolicy,
    ) -> Result<ExperimentReport, ReplayError> {
        let micros = |t: Instant| u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
        let head = self.store.head(stream)?;
        let to = match to {
            Target::Latest => head.sequence,
            Target::At(n) if n <= head.sequence => n,
            Target::At(n) => {
                return Err(ReconstructionError::TargetBeyondHead {
                    stream: stream.clone(),
                    target: n,
                    head: head.sequence,
                }
                .into());
            }
        };
        let mut report = ExperimentReport {
            experiment_id: None,
            run_id: None,
            stream_id: stream.clone(),
            interval: (Sequence::ZERO, to),
            projection: self.key(),
            snapshot_policy: policy,
            integrity: IntegritySummary {
                valid: false,
                events_checked: 0,
                total_issues: 0,
                categories: Vec::new(),
            },
            equivalence: None,
            replay_outcome: None,
            timings: ExperimentTimings::default(),
            notes: Vec::new(),
        };

        let t = Instant::now();
        let verified = self.store.verify_stream(stream)?;
        report.timings.verify_micros = micros(t);
        report.integrity = IntegritySummary {
            valid: verified.is_valid(),
            events_checked: verified.events_checked,
            total_issues: verified.total_issues,
            categories: verified.categories(),
        };
        if !verified.is_valid() {
            report.notes.push(
                "integrity verification failed; reconstruction and replay skipped".to_owned(),
            );
            return Ok(report);
        }

        let t = Instant::now();
        let full = self.reconstructor.reconstruct(
            stream,
            Target::At(to),
            ReconstructOptions::with_snapshots(SnapshotPolicy::Ignore),
        )?;
        let assisted = self.reconstructor.reconstruct(
            stream,
            Target::At(to),
            ReconstructOptions::with_snapshots(policy),
        )?;
        let full_value = self.encode(&full.state, to)?;
        let assisted_value = self.encode(&assisted.state, to)?;
        let differences = diff(&full_value, &assisted_value).len();
        report.timings.equivalence_micros = micros(t);
        report.equivalence = Some(EquivalenceSummary {
            equivalent: differences == 0,
            assisted_start: assisted.report.start.clone(),
            assisted_events_applied: assisted.report.events_applied,
            snapshot_warnings: assisted.report.snapshot_warnings.len(),
            full_state_digest: state_digest(&full_value),
            assisted_state_digest: state_digest(&assisted_value),
            differences,
        });

        let t = Instant::now();
        let run = self.record(stream, ReplayStart::Initial, Target::At(to))?;
        report.timings.record_micros = micros(t);
        report.run_id = Some(run.id);

        let t = Instant::now();
        let execution = self.execute(run.id)?;
        report.timings.replay_micros = micros(t);
        report.experiment_id = Some(execution.id);
        report.replay_outcome = Some(execution.outcome.label().to_owned());
        Ok(report)
    }
}
