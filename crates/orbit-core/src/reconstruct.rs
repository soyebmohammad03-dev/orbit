//! Historical state reconstruction, snapshot-assisted replay, state diffs and
//! provenance.
//!
//! A [`Reconstructor`] pairs an [`EventStore`] with a [`Projection`] and folds
//! a stream's events, in sequence order, into the projection's state.
//!
//! # Target semantics
//!
//! * [`Target::Latest`] is resolved to the stream head **once, at the start**
//!   of the call. Events appended afterwards are not included.
//! * [`Target::At(n)`](Target::At) reconstructs the state after applying
//!   exactly events `1..=n`. `At(Sequence::ZERO)` is the projection's initial
//!   state, valid for every existing stream (including empty ones).
//! * A target beyond the head fails with
//!   [`ReconstructionError::TargetBeyondHead`]. A missing stream fails with
//!   `StreamNotFound`. There is no clamping.
//!
//! # Consistency boundary
//!
//! Events are immutable and sequences are dense, so the events `1..=target`
//! never change once `target <= head`. Reconstruction therefore reads them in
//! bounded pages through [`EventStore::read_after`], without holding a
//! transaction open across the fold, and concurrent appends cannot affect
//! the result. As a defence in depth, every applied event must have exactly
//! the next sequence number and a `prev_digest` equal to the digest of the
//! event before it (or of the snapshot's event). Otherwise the call fails with
//! [`ReconstructionError::HistoryInconsistent`].
//!
//! # Failure semantics
//!
//! Reconstruction is all-or-nothing. If the projection rejects an event, the
//! call fails with [`ReconstructionError::Projection`], naming the stream,
//! sequence, event id and type. No partial state is returned and no event is
//! skipped.
//!
//! # Snapshots
//!
//! See [`SnapshotPolicy`] for how snapshots are selected and what happens to
//! invalid ones.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::diff::{Change, diff};
use crate::domain::{Digest, EventId, EventType, SchemaVersion, Sequence, StoredEvent, StreamId};
use crate::error::StoreError;
use crate::projection::{Projection, ProjectionError};
use crate::snapshot::{
    ProjectionKey, RawSnapshotRecord, Snapshot, SnapshotCheck, SnapshotId, SnapshotProblem,
};
use crate::store::{EventStore, NewSnapshot, SavedSnapshot};

/// Events fetched per page while folding.
const PAGE_SIZE: usize = 512;
/// Snapshot candidates fetched per query during selection.
const CANDIDATE_PAGE: usize = 8;

/// Which position to reconstruct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "sequence")]
pub enum Target {
    /// The stream head at the moment the call starts.
    Latest,
    /// The state after applying events `1..=n`.
    At(Sequence),
}

/// How a reconstruction may use snapshots.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "snapshot_id")]
pub enum SnapshotPolicy {
    /// Never use snapshots: always replay from the initial state.
    Ignore,
    /// Use the newest *valid* compatible snapshot at or before the target.
    /// Candidates that fail validation are skipped, recorded in
    /// [`ReconstructionReport::snapshot_warnings`] and logged at `warn`
    /// level, and the next older candidate is tried. With no usable
    /// candidate, replay starts from the initial state. This is the default.
    #[default]
    Prefer,
    /// Like `Prefer`, but the first candidate that fails validation aborts the
    /// reconstruction with [`ReconstructionError::Snapshot`].
    Strict,
    /// Start from exactly this snapshot. Any problem with it, including being
    /// beyond the target or for another stream or projection, is an error.
    Exact(SnapshotId),
}

/// Options for a reconstruction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconstructOptions {
    /// Snapshot use.
    pub snapshots: SnapshotPolicy,
    /// Record every applied event in [`ReconstructionReport::applied`].
    /// Off by default because it grows with the number of events applied.
    pub record_events: bool,
}

impl ReconstructOptions {
    /// Default options with the given snapshot policy.
    #[must_use]
    pub fn with_snapshots(snapshots: SnapshotPolicy) -> Self {
        Self {
            snapshots,
            record_events: false,
        }
    }

    /// Enables or disables per-event recording.
    #[must_use]
    pub fn recording_events(mut self, record: bool) -> Self {
        self.record_events = record;
        self
    }
}

/// Where a reconstruction began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StartPoint {
    /// [`Projection::initial_state`] at sequence 0.
    Initial,
    /// A validated snapshot.
    Snapshot {
        /// Snapshot used.
        snapshot_id: SnapshotId,
        /// Its sequence.
        sequence: Sequence,
    },
}

/// Identity of an applied event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppliedEvent {
    /// Sequence number.
    pub sequence: Sequence,
    /// Event id.
    pub event_id: EventId,
    /// Event type.
    pub event_type: EventType,
    /// Payload schema version.
    pub schema_version: SchemaVersion,
}

impl From<&StoredEvent> for AppliedEvent {
    fn from(event: &StoredEvent) -> Self {
        Self {
            sequence: event.sequence,
            event_id: event.event_id,
            event_type: event.event_type.clone(),
            schema_version: event.schema_version,
        }
    }
}

/// A snapshot that was considered and rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotWarning {
    /// Snapshot id as stored (hex if it is not a valid UUID).
    pub snapshot: String,
    /// Stored sequence.
    pub sequence: i64,
    /// Why it was rejected.
    pub problem: SnapshotProblem,
}

/// Provenance of a reconstruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReconstructionReport {
    /// Stream reconstructed.
    pub stream_id: StreamId,
    /// Projection used.
    pub projection: ProjectionKey,
    /// Target as requested.
    pub requested: Target,
    /// Stream head when the call started.
    pub head_at_start: Sequence,
    /// Position actually reconstructed (`requested` resolved against the head).
    pub reconstructed: Sequence,
    /// Digest of the event at `reconstructed` ([`Digest::GENESIS`] at 0).
    pub reconstructed_digest: Digest,
    /// Where the fold began.
    pub start: StartPoint,
    /// Number of events applied after the start point.
    pub events_applied: u64,
    /// Applied events in order, if [`ReconstructOptions::record_events`] was set.
    pub applied: Vec<AppliedEvent>,
    /// Snapshots considered and rejected, newest first.
    pub snapshot_warnings: Vec<SnapshotWarning>,
    /// Wall-clock duration of the call in microseconds (measurement only).
    pub elapsed_micros: u64,
}

/// A reconstructed state with its provenance.
#[derive(Debug, Clone)]
pub struct Reconstruction<S> {
    /// The state at `report.reconstructed`.
    pub state: S,
    /// How it was obtained.
    pub report: ReconstructionReport,
}

/// A stored snapshot together with its reconstruction report.
#[derive(Debug, Clone)]
pub struct SnapshotCreated {
    /// The stored snapshot (pre-existing if `saved.created` is `false`).
    pub saved: SavedSnapshot,
    /// The reconstruction that produced its state.
    pub report: ReconstructionReport,
}

/// Validation outcome of one snapshot for a given projection.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SnapshotStatus {
    /// The snapshot is intact, compatible and decodable.
    Usable {
        /// The snapshot.
        snapshot: Snapshot,
    },
    /// The snapshot must not be used.
    Unusable {
        /// Stored sequence.
        sequence: i64,
        /// Why.
        problem: SnapshotProblem,
    },
}

/// Comparison of full replay with snapshot-assisted reconstruction.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EquivalenceReport {
    /// `true` if both states serialize to equal JSON.
    pub equivalent: bool,
    /// Differences from the full-replay state to the snapshot-assisted state.
    pub differences: Vec<Change>,
    /// Full replay provenance.
    pub full: ReconstructionReport,
    /// Snapshot-assisted provenance.
    pub assisted: ReconstructionReport,
}

/// Structured comparison of the states at two positions.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateDiff {
    /// Stream compared.
    pub stream_id: StreamId,
    /// Position of the "old" state.
    pub from: Sequence,
    /// Position of the "new" state.
    pub to: Sequence,
    /// Changes turning the state at `from` into the state at `to`.
    pub changes: Vec<Change>,
    /// Events with sequence in `(min(from, to), max(from, to)]`, in order.
    pub events: Vec<AppliedEvent>,
    /// Provenance of the reconstruction of the earlier position.
    pub base: ReconstructionReport,
}

/// The state change caused by one event.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Transition {
    /// The event.
    pub event: AppliedEvent,
    /// Changes it made to the state (empty if it had no visible effect).
    pub changes: Vec<Change>,
}

/// Per-event explanation of how the state evolved between two positions.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Explanation {
    /// Stream explained.
    pub stream_id: StreamId,
    /// Start position (exclusive).
    pub from: Sequence,
    /// End position (inclusive).
    pub to: Sequence,
    /// One entry per event in `(from, to]`, in sequence order.
    pub transitions: Vec<Transition>,
    /// Provenance of the reconstruction of `from`.
    pub base: ReconstructionReport,
}

/// Why a reconstruction failed. No partial result is ever returned.
#[derive(Debug, thiserror::Error)]
pub enum ReconstructionError {
    /// A store operation failed (including `StreamNotFound`).
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The requested position is beyond the stream head.
    #[error("target sequence {target} is beyond the head {head} of stream {stream}")]
    TargetBeyondHead {
        /// Stream.
        stream: StreamId,
        /// Requested target.
        target: Sequence,
        /// Head at the start of the call.
        head: Sequence,
    },
    /// A range was given with `from > to` where an ordered range is required.
    #[error("invalid range: from {from} is after to {to}")]
    InvalidRange {
        /// Range start.
        from: Sequence,
        /// Range end.
        to: Sequence,
    },
    /// The projection rejected an event.
    #[error(transparent)]
    Projection(Box<ProjectionFailure>),
    /// Stored history violated the sequence or chain invariants during the fold.
    #[error("inconsistent history in stream {stream} at sequence {sequence}: {reason}")]
    HistoryInconsistent {
        /// Stream.
        stream: StreamId,
        /// Sequence at which the inconsistency was found.
        sequence: Sequence,
        /// What was wrong.
        reason: String,
    },
    /// A snapshot could not be used under [`SnapshotPolicy::Strict`] or
    /// [`SnapshotPolicy::Exact`].
    #[error("snapshot {snapshot} (sequence {sequence}) is unusable: {problem}")]
    Snapshot {
        /// Snapshot id as stored.
        snapshot: String,
        /// Stored sequence.
        sequence: i64,
        /// Why.
        problem: SnapshotProblem,
    },
    /// The projection state could not be converted to JSON.
    #[error(
        "state of projection {projection} at sequence {sequence} cannot be serialized: {reason}"
    )]
    StateEncoding {
        /// Projection.
        projection: ProjectionKey,
        /// Position of the state.
        sequence: Sequence,
        /// Serializer error.
        reason: String,
    },
}

/// Details of [`ReconstructionError::Projection`]: which event a projection
/// rejected, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error(
    "projection {projection} failed on stream {stream} at sequence {sequence} (event {event_id}, type {event_type}): {source}"
)]
pub struct ProjectionFailure {
    /// Stream.
    pub stream: StreamId,
    /// Projection.
    pub projection: ProjectionKey,
    /// Sequence of the offending event.
    pub sequence: Sequence,
    /// Id of the offending event.
    pub event_id: EventId,
    /// Type of the offending event.
    pub event_type: EventType,
    /// The projection's error.
    #[source]
    pub source: ProjectionError,
}

/// Verdict of validating one snapshot: usable with its decoded state, or why not.
type Validated<S> = Result<(Snapshot, S), SnapshotProblem>;

/// In-progress fold.
struct Run<S> {
    state: S,
    sequence: Sequence,
    digest: Digest,
    report: ReconstructionReport,
    started: Instant,
}

/// Reconstructs projection state from an [`EventStore`].
///
/// ```no_run
/// use orbit_core::projection::examples::Counter;
/// use orbit_core::reconstruct::{ReconstructOptions, Reconstructor, Target};
/// use orbit_core::{EventStore, Sequence, StoreConfig, StreamId};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let store = EventStore::open(StoreConfig::new("orbit.db"))?;
/// let counters = Reconstructor::new(&store, Counter);
/// let stream = StreamId::new("counter-1")?;
///
/// let at_10 = counters.reconstruct(&stream, Target::At(Sequence::new(10)?), ReconstructOptions::default())?;
/// println!("value at #10: {}", at_10.state.value);
///
/// counters.create_snapshot(&stream, Target::Latest, ReconstructOptions::default())?;
/// let diff = counters.diff(&stream, Sequence::new(5)?, Sequence::new(10)?, ReconstructOptions::default())?;
/// println!("{} changes over {} events", diff.changes.len(), diff.events.len());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Reconstructor<'s, P> {
    store: &'s EventStore,
    projection: P,
}

impl<'s, P: Projection> Reconstructor<'s, P> {
    /// Pairs a store with a projection.
    pub fn new(store: &'s EventStore, projection: P) -> Self {
        Self { store, projection }
    }

    /// The projection.
    pub fn projection(&self) -> &P {
        &self.projection
    }

    /// Reconstructs the state at `target`.
    ///
    /// # Errors
    /// See [`ReconstructionError`].
    pub fn reconstruct(
        &self,
        stream: &StreamId,
        target: Target,
        options: ReconstructOptions,
    ) -> Result<Reconstruction<P::State>, ReconstructionError> {
        let run = self.reconstruct_run(stream, target, options)?;
        Ok(self.finish(run))
    }

    /// Reconstructs the state at `target` and stores it as a snapshot.
    ///
    /// The snapshot is tied to the position that was actually reconstructed
    /// and the digest of the event at that position. Events appended while
    /// this runs are not included and do not invalidate it. Saving is
    /// idempotent (see [`EventStore::save_snapshot`]).
    ///
    /// # Errors
    /// [`ReconstructionError`]. Snapshotting position 0 fails with
    /// [`ValidationError::SnapshotAtZero`](crate::ValidationError::SnapshotAtZero).
    pub fn create_snapshot(
        &self,
        stream: &StreamId,
        target: Target,
        options: ReconstructOptions,
    ) -> Result<SnapshotCreated, ReconstructionError> {
        let reconstruction = self.reconstruct(stream, target, options)?;
        let report = reconstruction.report;
        let state = self.encode(&reconstruction.state, report.reconstructed)?;
        let saved = self.store.save_snapshot(NewSnapshot {
            stream_id: stream.clone(),
            sequence: report.reconstructed,
            event_digest: report.reconstructed_digest,
            projection: self.projection.key(),
            state,
        })?;
        Ok(SnapshotCreated { saved, report })
    }

    /// Fully validates one snapshot for this projection: integrity digest,
    /// binding to history, projection key, and state decoding.
    ///
    /// # Errors
    /// [`StoreError::SnapshotNotFound`] or a database error.
    pub fn check_snapshot(&self, id: SnapshotId) -> Result<SnapshotStatus, ReconstructionError> {
        let raw = self.store.load_snapshot_record(id)?;
        Ok(match self.validate(&raw, None, None)? {
            Ok((snapshot, _state)) => SnapshotStatus::Usable { snapshot },
            Err(problem) => SnapshotStatus::Unusable {
                sequence: raw.sequence,
                problem,
            },
        })
    }

    /// Reconstructs `target` twice, once by full replay and once with
    /// `policy`, and compares the resulting states as JSON.
    ///
    /// # Errors
    /// [`ReconstructionError`] from either reconstruction.
    pub fn compare_with_full_replay(
        &self,
        stream: &StreamId,
        target: Target,
        policy: SnapshotPolicy,
    ) -> Result<EquivalenceReport, ReconstructionError> {
        // Resolve `Latest` once so both runs reconstruct the same position.
        let target = Target::At(self.resolve(stream, target)?.1);
        let full = self.reconstruct(
            stream,
            target,
            ReconstructOptions::with_snapshots(SnapshotPolicy::Ignore),
        )?;
        let assisted =
            self.reconstruct(stream, target, ReconstructOptions::with_snapshots(policy))?;
        let differences = diff(
            &self.encode(&full.state, full.report.reconstructed)?,
            &self.encode(&assisted.state, assisted.report.reconstructed)?,
        );
        Ok(EquivalenceReport {
            equivalent: differences.is_empty(),
            differences,
            full: full.report,
            assisted: assisted.report,
        })
    }

    /// Compares the states at `from` and `to` (either order).
    ///
    /// The earlier position is reconstructed with `options`, then the fold
    /// continues to the later one, so the interval is read only once.
    /// `changes` always describe the transformation *from* `from` *to* `to`;
    /// with `from > to` they describe going back in time.
    ///
    /// # Errors
    /// [`ReconstructionError::TargetBeyondHead`] if either position is
    /// beyond the head, or any other reconstruction error.
    pub fn diff(
        &self,
        stream: &StreamId,
        from: Sequence,
        to: Sequence,
        options: ReconstructOptions,
    ) -> Result<StateDiff, ReconstructionError> {
        let (low, high) = if from <= to { (from, to) } else { (to, from) };
        // Validates `high` against the head before any work is done.
        let (_, high) = self.resolve(stream, Target::At(high))?;
        let run = self.reconstruct_run(stream, Target::At(low), options)?;
        let base = self.finish_report(&run);
        let low_value = self.encode(&run.state, low)?;
        let mut events = Vec::new();
        let run = self.advance(stream, run, high, false, |event, _| {
            events.push(AppliedEvent::from(event));
            Ok(())
        })?;
        let high_value = self.encode(&run.state, high)?;
        let changes = if from <= to {
            diff(&low_value, &high_value)
        } else {
            diff(&high_value, &low_value)
        };
        Ok(StateDiff {
            stream_id: stream.clone(),
            from,
            to,
            changes,
            events,
            base,
        })
    }

    /// Explains, event by event, how the state changed in `(from, to]`.
    ///
    /// Serializes the state after every event, so the cost grows with
    /// `(to - from) × state size`; keep intervals focused.
    ///
    /// # Errors
    /// [`ReconstructionError::InvalidRange`] if `from > to`, or any
    /// reconstruction error.
    pub fn explain(
        &self,
        stream: &StreamId,
        from: Sequence,
        to: Sequence,
        options: ReconstructOptions,
    ) -> Result<Explanation, ReconstructionError> {
        if from > to {
            return Err(ReconstructionError::InvalidRange { from, to });
        }
        let (_, to) = self.resolve(stream, Target::At(to))?;
        let run = self.reconstruct_run(stream, Target::At(from), options)?;
        let base = self.finish_report(&run);
        let mut previous = self.encode(&run.state, from)?;
        let mut transitions = Vec::new();
        self.advance(stream, run, to, false, |event, state| {
            let current = self.encode(state, event.sequence)?;
            transitions.push(Transition {
                event: AppliedEvent::from(event),
                changes: diff(&previous, &current),
            });
            previous = current;
            Ok(())
        })?;
        Ok(Explanation {
            stream_id: stream.clone(),
            from,
            to,
            transitions,
            base,
        })
    }

    // ---------------------------------------------------------------------

    /// Folds events after `start` up to and including `target` onto `state`,
    /// with the same continuity checks as [`reconstruct`](Self::reconstruct).
    /// `hook` runs after every applied event. Used by replay.
    pub(crate) fn fold_from(
        &self,
        stream: &StreamId,
        state: P::State,
        start: crate::domain::StreamHead,
        target: Sequence,
        hook: impl FnMut(&StoredEvent, &P::State) -> Result<(), ReconstructionError>,
    ) -> Result<(P::State, crate::domain::StreamHead), ReconstructionError> {
        let run = Run {
            state,
            sequence: start.sequence,
            digest: start.digest,
            report: ReconstructionReport {
                stream_id: stream.clone(),
                projection: self.projection.key(),
                requested: Target::At(target),
                head_at_start: target,
                reconstructed: start.sequence,
                reconstructed_digest: start.digest,
                start: StartPoint::Initial,
                events_applied: 0,
                applied: Vec::new(),
                snapshot_warnings: Vec::new(),
                elapsed_micros: 0,
            },
            started: Instant::now(),
        };
        let run = self.advance(stream, run, target, false, hook)?;
        Ok((
            run.state,
            crate::domain::StreamHead {
                sequence: run.sequence,
                digest: run.digest,
            },
        ))
    }

    /// Establishes the start point and folds up to the resolved target.
    fn reconstruct_run(
        &self,
        stream: &StreamId,
        target: Target,
        options: ReconstructOptions,
    ) -> Result<Run<P::State>, ReconstructionError> {
        let (run, target) = self.begin(stream, target, options)?;
        self.advance(stream, run, target, options.record_events, |_, _| Ok(()))
    }

    /// Reads the head once and resolves the target against it.
    fn resolve(
        &self,
        stream: &StreamId,
        target: Target,
    ) -> Result<(Sequence, Sequence), ReconstructionError> {
        let head = self.store.head(stream)?.sequence;
        let resolved = match target {
            Target::Latest => head,
            Target::At(sequence) if sequence <= head => sequence,
            Target::At(sequence) => {
                return Err(ReconstructionError::TargetBeyondHead {
                    stream: stream.clone(),
                    target: sequence,
                    head,
                });
            }
        };
        Ok((head, resolved))
    }

    /// Resolves the target and establishes the starting state.
    fn begin(
        &self,
        stream: &StreamId,
        requested: Target,
        options: ReconstructOptions,
    ) -> Result<(Run<P::State>, Sequence), ReconstructionError> {
        let started = Instant::now();
        let (head, target) = self.resolve(stream, requested)?;
        let mut report = ReconstructionReport {
            stream_id: stream.clone(),
            projection: self.projection.key(),
            requested,
            head_at_start: head,
            reconstructed: Sequence::ZERO,
            reconstructed_digest: Digest::GENESIS,
            start: StartPoint::Initial,
            events_applied: 0,
            applied: Vec::new(),
            snapshot_warnings: Vec::new(),
            elapsed_micros: 0,
        };

        let start = match options.snapshots {
            SnapshotPolicy::Ignore => None,
            SnapshotPolicy::Exact(id) => {
                let raw = self.store.load_snapshot_record(id)?;
                match self.validate(&raw, Some(stream), Some(target))? {
                    Ok(found) => Some(found),
                    Err(problem) => {
                        return Err(ReconstructionError::Snapshot {
                            snapshot: raw.display_id(),
                            sequence: raw.sequence,
                            problem,
                        });
                    }
                }
            }
            // No snapshot can be at or before position 0.
            SnapshotPolicy::Prefer | SnapshotPolicy::Strict if target == Sequence::ZERO => None,
            SnapshotPolicy::Prefer | SnapshotPolicy::Strict => {
                let strict = options.snapshots == SnapshotPolicy::Strict;
                self.select(stream, target, strict, &mut report.snapshot_warnings)?
            }
        };

        let run = match start {
            Some((snapshot, state)) => {
                report.start = StartPoint::Snapshot {
                    snapshot_id: snapshot.id,
                    sequence: snapshot.sequence,
                };
                report.reconstructed = snapshot.sequence;
                report.reconstructed_digest = snapshot.event_digest;
                Run {
                    state,
                    sequence: snapshot.sequence,
                    digest: snapshot.event_digest,
                    report,
                    started,
                }
            }
            None => Run {
                state: self.projection.initial_state(),
                sequence: Sequence::ZERO,
                digest: Digest::GENESIS,
                report,
                started,
            },
        };
        Ok((run, target))
    }

    /// Newest usable compatible snapshot at or before `target`.
    fn select(
        &self,
        stream: &StreamId,
        target: Sequence,
        strict: bool,
        warnings: &mut Vec<SnapshotWarning>,
    ) -> Result<Option<(Snapshot, P::State)>, ReconstructionError> {
        let key = self.projection.key();
        let mut at_most = target;
        loop {
            let candidates =
                self.store
                    .snapshot_candidates(stream, &key, at_most, CANDIDATE_PAGE)?;
            for raw in &candidates {
                match self.validate(raw, Some(stream), Some(target))? {
                    Ok(found) => return Ok(Some(found)),
                    Err(problem) if strict => {
                        return Err(ReconstructionError::Snapshot {
                            snapshot: raw.display_id(),
                            sequence: raw.sequence,
                            problem,
                        });
                    }
                    Err(problem) => {
                        tracing::warn!(stream = %stream, snapshot = %raw.display_id(),
                            sequence = raw.sequence, %problem, "skipping unusable snapshot");
                        warnings.push(SnapshotWarning {
                            snapshot: raw.display_id(),
                            sequence: raw.sequence,
                            problem,
                        });
                    }
                }
            }
            // Continue below the oldest candidate of this page, if there may be more.
            let next = candidates
                .last()
                .filter(|_| candidates.len() == CANDIDATE_PAGE)
                .and_then(|last| last.sequence.checked_sub(1))
                .and_then(Sequence::from_i64);
            match next {
                Some(next) if next >= Sequence::FIRST && next < at_most => at_most = next,
                _ => return Ok(None),
            }
        }
    }

    /// Full validation of one snapshot row. The outer `Result` carries
    /// failures to *perform* validation; the inner one the verdict.
    fn validate(
        &self,
        raw: &RawSnapshotRecord,
        stream: Option<&StreamId>,
        target: Option<Sequence>,
    ) -> Result<Validated<P::State>, ReconstructionError> {
        let snapshot = match self.store.check_snapshot_record(raw)? {
            SnapshotCheck::Intact { snapshot } => snapshot,
            SnapshotCheck::Unusable { problem, .. } => return Ok(Err(problem)),
        };
        if let Some(stream) = stream
            && snapshot.stream_id != *stream
        {
            return Ok(Err(SnapshotProblem::WrongStream {
                found: snapshot.stream_id,
            }));
        }
        let expected = self.projection.key();
        if snapshot.projection != expected {
            return Ok(Err(SnapshotProblem::Incompatible {
                found: snapshot.projection,
                expected,
            }));
        }
        if let Some(target) = target
            && snapshot.sequence > target
        {
            return Ok(Err(SnapshotProblem::BeyondTarget {
                sequence: snapshot.sequence,
                target,
            }));
        }
        match P::State::deserialize(&snapshot.state) {
            Ok(state) => Ok(Ok((snapshot, state))),
            Err(e) => Ok(Err(SnapshotProblem::Undecodable {
                reason: e.to_string(),
            })),
        }
    }

    /// Applies events after `run.sequence` up to and including `target`,
    /// calling `hook` after each one with the new state.
    fn advance(
        &self,
        stream: &StreamId,
        mut run: Run<P::State>,
        target: Sequence,
        record: bool,
        mut hook: impl FnMut(&StoredEvent, &P::State) -> Result<(), ReconstructionError>,
    ) -> Result<Run<P::State>, ReconstructionError> {
        let inconsistent =
            |sequence: Sequence, reason: String| ReconstructionError::HistoryInconsistent {
                stream: stream.clone(),
                sequence,
                reason,
            };
        while run.sequence < target {
            let remaining = target.get() - run.sequence.get();
            let limit = usize::try_from(remaining).map_or(PAGE_SIZE, |r| r.min(PAGE_SIZE));
            let page = self.store.read_after(stream, run.sequence, limit)?;
            let expected_next = run.sequence.next().unwrap_or(Sequence::MAX);
            if page.is_empty() {
                return Err(inconsistent(expected_next, "event is missing".to_owned()));
            }
            for event in page {
                let expected = run.sequence.next().unwrap_or(Sequence::MAX);
                if event.sequence != expected {
                    return Err(inconsistent(
                        expected,
                        format!("found sequence {} instead", event.sequence),
                    ));
                }
                if event.prev_digest != run.digest {
                    return Err(inconsistent(
                        event.sequence,
                        format!(
                            "prev_digest {} does not match digest {} of the preceding position",
                            event.prev_digest, run.digest
                        ),
                    ));
                }
                run.state = self.projection.apply(run.state, &event).map_err(|source| {
                    ReconstructionError::Projection(Box::new(ProjectionFailure {
                        stream: stream.clone(),
                        projection: self.projection.key(),
                        sequence: event.sequence,
                        event_id: event.event_id,
                        event_type: event.event_type.clone(),
                        source,
                    }))
                })?;
                run.sequence = event.sequence;
                run.digest = event.digest;
                run.report.events_applied += 1;
                if record {
                    run.report.applied.push(AppliedEvent::from(&event));
                }
                hook(&event, &run.state)?;
            }
        }
        run.report.reconstructed = run.sequence;
        run.report.reconstructed_digest = run.digest;
        Ok(run)
    }

    fn finish_report(&self, run: &Run<P::State>) -> ReconstructionReport {
        let mut report = run.report.clone();
        report.reconstructed = run.sequence;
        report.reconstructed_digest = run.digest;
        report.elapsed_micros =
            u64::try_from(run.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        report
    }

    fn finish(&self, run: Run<P::State>) -> Reconstruction<P::State> {
        let report = self.finish_report(&run);
        tracing::debug!(stream = %report.stream_id, projection = %report.projection,
            sequence = %report.reconstructed, applied = report.events_applied,
            "reconstructed state");
        Reconstruction {
            state: run.state,
            report,
        }
    }

    fn encode(&self, state: &P::State, sequence: Sequence) -> Result<Value, ReconstructionError> {
        serde_json::to_value(state).map_err(|e| ReconstructionError::StateEncoding {
            projection: self.projection.key(),
            sequence,
            reason: e.to_string(),
        })
    }
}
