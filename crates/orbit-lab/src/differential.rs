//! Differential replay: two implementations, one ordered history.
//!
//! # Boundaries
//!
//! A *boundary* is a position between events. Boundary `b` is the state
//! **after** applying events `1..=b`; boundary 0 is the initial state, before
//! any event. "The states diverge at boundary `b`" therefore means that
//! applying event `b` produced different states, given states that were
//! equal at `b - 1` (or, at the first compared boundary, unknown).
//! "Converged at boundary `c`" means the states differed at `c - 1` and are
//! equal again after event `c`.
//!
//! # Algorithm
//!
//! 1. **Resolve the input.** Read the stream head, resolve `to`, check
//!    `from <= to <= head`, read the digests of events `from` and `to`, and
//!    verify the whole stream (`EventStore::verify_stream`). A damaged
//!    stream is refused before any implementation runs.
//! 2. **Lockstep execution.** Start both implementations at their own
//!    initial state. Read events `1..=to` in sequence order, in pages, from
//!    the store. Each event is checked for continuity (sequence `n + 1`,
//!    `prev_digest` equal to the previous event's digest) and the *same*
//!    `&StoredEvent` value is passed to the left and then the right
//!    implementation, so both receive identical ordered input. A digest over
//!    the ordered event digests fed (`events_fed_digest`) records it.
//! 3. **Compare at every boundary** `from <= b <= to`: each state is
//!    serialized with `serde_json` and compared by
//!    `orbit_core::replay::state_digest`, i.e. by canonical JSON. Both
//!    digests are appended to the trace artifacts. Every maximal run of
//!    differing boundaries is a [`DivergenceInterval`]; the result lists
//!    each one with how it ended (converged, still open at `to`, or stopped
//!    by a failure). The first [`MAX_DIAGNOSED_INTERVALS`] intervals carry a
//!    structural diff (`orbit_core::diff::diff`, left → right) at their first
//!    and last boundary. Full states are kept only for the current and, while
//!    a diagnosed interval is open, the latest differing boundary.
//! 4. **Stop at the first failure.** If either implementation rejects an
//!    event, panics or produces an unencodable state, both stop there and
//!    the failure is the result (`implementation_failed`).
//! 5. **Determinism check** (default on). Each implementation is executed
//!    again on a fresh instance, alone, over exactly the events it saw, and
//!    its digest trace and failure must be identical. Otherwise the outcome
//!    is `nondeterministic` and the comparison is not reported as meaningful.
//!
//! # Terminology
//!
//! Without a `reference` side the result describes a *difference between
//! implementations*; neither side is called wrong. With a reference, that
//! side's state is the *expected* state and the other is *observed*.
//! Structural changes always read left → right: `from` is the left value,
//! `to` the right value ([`Perspective`]).
//!
//! # Evidence and hypotheses
//!
//! [`Diagnosis::evidence`] lists only facts read from the comparison.
//! [`Diagnosis::not_established`] lists what those facts do not show. The lab
//! never states a cause.

use std::ops::ControlFlow;

use orbit_core::diff::{Change, ChangeKind, diff};
use orbit_core::domain::canonical::to_canonical_string;
use orbit_core::replay::state_digest;
use orbit_core::{
    Digest, EventId, EventStore, EventType, SchemaVersion, Sequence, StoreError, StoredEvent,
    StreamId, Timestamp,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::catalog::{Implementation, StepFailure, Stepper};
use crate::digest::{EVENTS_TAG, HISTORY_TAG, TaggedHasher};
use crate::identity::Determinism;
use crate::registry::{Failure, FailureKind, InputRange};
use crate::spec::{DifferentialSpec, MAX_EVENTS, Side};

/// Layout version of [`DifferentialResult`]. Sprint 7 results (without this
/// field) are version 1.
pub const RESULT_VERSION: u32 = 2;
/// Version of the comparison rules ([`Semantics`]). Unchanged since Sprint 7,
/// whose results (without the field) used the same rules.
pub const SEMANTICS_VERSION: u32 = 1;
/// Events read from the store per page.
pub const PAGE_SIZE: usize = 1_000;
/// Structural changes recorded per comparison; the total is always reported.
pub const MAX_RECORDED_CHANGES: usize = 200;
/// Divergence intervals listed in a result; the total is always reported.
pub const MAX_RECORDED_INTERVALS: usize = 1_000;
/// Divergence intervals that carry structural diffs at their first and last
/// boundary. Later intervals are listed with boundaries only; their digests
/// remain in the trace artifacts.
pub const MAX_DIAGNOSED_INTERVALS: usize = 20;

/// Outcome of a differential replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The states were equal at every compared boundary.
    Equivalent,
    /// The states differed at some boundaries but are equal at `to`.
    Reconverged,
    /// The states differ at `to`.
    Diverged,
    /// At least one implementation failed; comparison stopped there.
    ImplementationFailed,
    /// An implementation produced different results on two executions over
    /// the same input. Its comparison is not meaningful.
    Nondeterministic,
}

impl Outcome {
    /// Stored label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Equivalent => "equivalent",
            Self::Reconverged => "reconverged",
            Self::Diverged => "diverged",
            Self::ImplementationFailed => "implementation_failed",
            Self::Nondeterministic => "nondeterministic",
        }
    }
}

/// The comparison rules a result was produced under. Reproduction refuses
/// to compare results produced under different rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Semantics {
    /// [`SEMANTICS_VERSION`].
    pub version: u32,
    /// What a boundary is.
    pub boundary: &'static str,
    /// How states are compared.
    pub state_comparison: &'static str,
    /// How structural differences are computed.
    pub structural_diff: &'static str,
    /// Largest supported `to`.
    pub max_events: u64,
    /// [`MAX_RECORDED_CHANGES`].
    pub max_recorded_changes: usize,
    /// [`MAX_RECORDED_INTERVALS`].
    pub max_recorded_intervals: usize,
    /// [`MAX_DIAGNOSED_INTERVALS`].
    pub max_diagnosed_intervals: usize,
}

impl Semantics {
    /// The rules of this build.
    #[must_use]
    pub fn current() -> Self {
        Self {
            version: SEMANTICS_VERSION,
            boundary: "boundary b is the state after applying events 1..=b; boundary 0 is the initial state",
            state_comparison: "equal iff ORBIT-STATE-DIGEST-V1 digests of the canonical JSON of the serde_json encodings are equal",
            structural_diff: "orbit_core::diff::diff from the left to the right state; JSON Pointer paths; arrays compared by index",
            max_events: MAX_EVENTS,
            max_recorded_changes: MAX_RECORDED_CHANGES,
            max_recorded_intervals: MAX_RECORDED_INTERVALS,
            max_diagnosed_intervals: MAX_DIAGNOSED_INTERVALS,
        }
    }
}

/// Which side, if any, is the expected behaviour, and how to read diffs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Perspective {
    /// The reference side, if one was designated.
    pub reference: Option<Side>,
    /// The expected side (the reference), if any.
    pub expected: Option<Side>,
    /// The observed side (the other one), if a reference was designated.
    pub observed: Option<Side>,
    /// Always `"left_to_right"`: a change's `from` is the left value and
    /// `to` the right value.
    pub changes_orientation: &'static str,
}

impl Perspective {
    fn new(reference: Option<Side>) -> Self {
        Self {
            reference,
            expected: reference,
            observed: reference.map(|s| match s {
                Side::Left => Side::Right,
                Side::Right => Side::Left,
            }),
            changes_orientation: "left_to_right",
        }
    }
}

/// Size and digest of one state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StateSummary {
    /// `ORBIT-STATE-DIGEST-V1` digest of the canonical JSON.
    pub digest: Digest,
    /// Length of the canonical JSON in bytes.
    pub canonical_bytes: u64,
}

impl StateSummary {
    fn of(state: &Value) -> Self {
        Self {
            digest: state_digest(state),
            canonical_bytes: to_canonical_string(state).len() as u64,
        }
    }
}

/// The event that produced a boundary. Metadata *values* are not copied
/// into results (they may be sensitive); only their keys are listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventRef {
    /// Sequence.
    pub sequence: Sequence,
    /// Event id.
    pub event_id: EventId,
    /// Event type.
    pub event_type: EventType,
    /// Payload schema version.
    pub schema_version: SchemaVersion,
    /// The event's digest (it chains to every earlier event).
    pub digest: Digest,
    /// When it was recorded (informational).
    pub recorded_at: Timestamp,
    /// Metadata keys, sorted.
    pub metadata_keys: Vec<String>,
}

impl From<&StoredEvent> for EventRef {
    fn from(e: &StoredEvent) -> Self {
        let mut metadata_keys: Vec<String> = e.metadata.keys().cloned().collect();
        metadata_keys.sort();
        Self {
            sequence: e.sequence,
            event_id: e.event_id,
            event_type: e.event_type.clone(),
            schema_version: e.schema_version,
            digest: e.digest,
            recorded_at: e.recorded_at,
            metadata_keys,
        }
    }
}

/// Counts of structural changes by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ChangeSummary {
    /// Paths present only in the right state.
    pub added: u64,
    /// Paths present only in the left state.
    pub removed: u64,
    /// Paths present in both with different values.
    pub modified: u64,
    /// Of the modified paths, those whose JSON type differs (for example a
    /// number on one side and a string or `null` on the other).
    pub type_changed: u64,
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

impl ChangeSummary {
    fn of(changes: &[Change]) -> Self {
        let mut s = Self::default();
        for change in changes {
            match &change.kind {
                ChangeKind::Added { .. } => s.added += 1,
                ChangeKind::Removed { .. } => s.removed += 1,
                ChangeKind::Modified { from, to } => {
                    s.modified += 1;
                    if json_type(from) != json_type(to) {
                        s.type_changed += 1;
                    }
                }
            }
        }
        s
    }
}

/// Two states compared at one boundary.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Comparison {
    /// Boundary (number of events applied).
    pub boundary: u64,
    /// The event that produced it (`None` for boundary 0).
    pub event: Option<EventRef>,
    /// Left state.
    pub left: StateSummary,
    /// Right state.
    pub right: StateSummary,
    /// Whether the states are equal.
    pub equal: bool,
    /// Structural changes from the left to the right state (at most
    /// [`MAX_RECORDED_CHANGES`]).
    pub changes: Vec<Change>,
    /// Number of changes in total.
    pub changes_total: u64,
    /// Counts of all changes (not only the recorded ones) by kind.
    pub change_summary: ChangeSummary,
}

impl Comparison {
    fn new(boundary: u64, event: Option<EventRef>, left: &Value, right: &Value) -> Self {
        let l = StateSummary::of(left);
        let r = StateSummary::of(right);
        let equal = l.digest == r.digest;
        let mut changes = if equal { Vec::new() } else { diff(left, right) };
        let changes_total = changes.len() as u64;
        let change_summary = ChangeSummary::of(&changes);
        changes.truncate(MAX_RECORDED_CHANGES);
        Self {
            boundary,
            event,
            left: l,
            right: r,
            equal,
            changes,
            changes_total,
            change_summary,
        }
    }
}

/// The first boundary at which the states differed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Divergence {
    /// The comparison at that boundary.
    #[serde(flatten)]
    pub comparison: Comparison,
    /// Boundaries compared equal before it.
    pub boundaries_matched_before: u64,
}

/// How a divergence interval ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntervalEnd {
    /// The states became equal again at `converged_at`.
    Converged,
    /// The states still differed at the last boundary, `to`.
    OpenAtEnd,
    /// An implementation failed while the states differed.
    StoppedByFailure,
}

/// A maximal run of consecutive boundaries at which the states differ.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DivergenceInterval {
    /// 1-based position among all intervals.
    pub index: u64,
    /// First differing boundary.
    pub first: u64,
    /// Last differing boundary.
    pub last: u64,
    /// Number of differing boundaries (`last - first + 1`).
    pub boundaries: u64,
    /// How it ended.
    pub end: IntervalEnd,
    /// Boundary at which the states became equal again (`last + 1`).
    pub converged_at: Option<u64>,
    /// The event that made them equal again.
    pub convergence_event: Option<EventRef>,
    /// Comparison at `first` (for the first [`MAX_DIAGNOSED_INTERVALS`] intervals).
    pub entry: Option<Comparison>,
    /// Comparison at `last` when it differs from `first` (diagnosed intervals only).
    pub exit: Option<Comparison>,
}

/// The complete divergence history.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Timeline {
    /// First compared boundary (`from`).
    pub first_boundary: u64,
    /// Last compared boundary (`to`, or earlier if an implementation failed);
    /// `None` if nothing was compared.
    pub last_boundary: Option<u64>,
    /// Divergence intervals in order (at most [`MAX_RECORDED_INTERVALS`]).
    pub intervals: Vec<DivergenceInterval>,
    /// Number of intervals in total.
    pub intervals_total: u64,
    /// Whether `intervals` is shorter than `intervals_total`.
    pub intervals_truncated: bool,
    /// Number of intervals that ended by converging.
    pub convergences: u64,
    /// Whether the states are equal at `last_boundary`.
    pub final_equal: Option<bool>,
}

/// Facts read from the comparison, and what they do not show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnosis {
    /// Observed facts.
    pub evidence: Vec<String>,
    /// Questions the evidence does not answer.
    pub not_established: Vec<String>,
}

/// An implementation failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SideFailure {
    /// Which side.
    pub side: Side,
    /// Implementation id.
    pub implementation: String,
    /// The boundary at which it failed: the sequence of the event it could not
    /// apply, or of the state it could not encode.
    pub boundary: u64,
    /// The event involved (`None` at boundary 0).
    pub event: Option<EventRef>,
    /// What went wrong.
    pub failure: StepFailure,
}

/// What the determinism check found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeterminismReport {
    /// Whether each side was executed twice.
    pub checked: bool,
    /// `Some(true)` if both executions of each side agreed.
    pub consistent: Option<bool>,
    /// The first inconsistent side and boundary.
    pub inconsistency: Option<Inconsistency>,
    /// Sides whose implementation declares [`Determinism::Uncontrolled`].
    pub declared_uncontrolled: Vec<Side>,
}

/// Where two executions of one implementation disagreed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Inconsistency {
    /// Side.
    pub side: Side,
    /// First boundary at which the executions disagree.
    pub boundary: u64,
}

/// Reproducibility status of a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproducibilityStatus {
    /// The input is pinned by digests and both implementations produced
    /// identical traces on two executions. This is evidence, not proof:
    /// see the module documentation.
    Reproducible,
    /// The determinism check was disabled.
    Unverified,
    /// An implementation was nondeterministic or declares uncontrolled
    /// dependencies.
    NotReproducible,
}

impl ReproducibilityStatus {
    /// Stored label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Reproducible => "reproducible",
            Self::Unverified => "unverified",
            Self::NotReproducible => "not_reproducible",
        }
    }
}

/// Reproducibility status with its reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reproducibility {
    /// Status.
    pub status: ReproducibilityStatus,
    /// Why, in plain words.
    pub reasons: Vec<String>,
}

/// One implementation as it appears in a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SideIdentity {
    /// Implementation id.
    pub implementation: String,
    /// Its fingerprint.
    pub fingerprint: Digest,
}

/// The result of a differential replay. Contains no timings, ids, build
/// details or timestamps of the run, so executing the same experiment on the
/// same input yields the same document (and the same digest).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DifferentialResult {
    /// [`RESULT_VERSION`].
    pub result_version: u32,
    /// The comparison rules.
    pub semantics: Semantics,
    /// Outcome.
    pub outcome: Outcome,
    /// Reference side, if any (its state is the expected one).
    pub reference: Option<Side>,
    /// How to read sides and diffs.
    pub perspective: Perspective,
    /// Left implementation.
    pub left: SideIdentity,
    /// Right implementation.
    pub right: SideIdentity,
    /// Input history.
    pub input: InputRange,
    /// Events passed to both implementations.
    pub events_processed: u64,
    /// `ORBIT-LAB-EVENTS-V1` digest over the ordered digests of those events.
    pub events_fed_digest: Digest,
    /// Boundaries at which both states were compared.
    pub boundaries_compared: u64,
    /// Boundaries at which they differed.
    pub boundaries_differing: u64,
    /// First differing boundary.
    pub first_divergence: Option<Divergence>,
    /// Every divergence interval and convergence.
    pub timeline: Timeline,
    /// Comparison at the last compared boundary, when neither side failed.
    pub final_comparison: Option<Comparison>,
    /// Implementation failures.
    pub failures: Vec<SideFailure>,
    /// Determinism check.
    pub determinism: DeterminismReport,
    /// Reproducibility.
    pub reproducibility: Reproducibility,
    /// Evidence and open questions.
    pub diagnosis: Diagnosis,
}

/// A computed result plus the artifacts to store with it.
#[derive(Debug)]
pub struct DifferentialRun {
    /// The result.
    pub result: DifferentialResult,
    /// `(role, media type, bytes)` of each artifact.
    pub artifacts: Vec<(String, &'static str, Vec<u8>)>,
}

/// Why an experiment could not be carried out, with the input if it was
/// resolved.
#[derive(Debug)]
pub struct RunFailure {
    /// The failure.
    pub failure: Failure,
    /// The resolved input, if any.
    pub input: Option<Box<InputRange>>,
}

impl From<Failure> for RunFailure {
    fn from(failure: Failure) -> Self {
        Self {
            failure,
            input: None,
        }
    }
}

fn internal(e: &StoreError) -> Failure {
    Failure::new(FailureKind::Internal, e.to_string())
}

fn seq(n: u64) -> Result<Sequence, Failure> {
    Sequence::new(n).map_err(|e| Failure::new(FailureKind::InvalidRange, e.to_string()))
}

/// Where a differential replay reads its events from: an [`EventStore`],
/// or a [`MemoryHistory`] whose digest chain was verified when it was built.
pub trait EventSource {
    /// Up to `limit` events of `stream` after `after`, in order.
    ///
    /// # Errors
    /// Store errors.
    fn read_after(
        &self,
        stream: &StreamId,
        after: Sequence,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, StoreError>;
}

impl EventSource for EventStore {
    fn read_after(
        &self,
        stream: &StreamId,
        after: Sequence,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        EventStore::read_after(self, stream, after, limit)
    }
}

/// Events `1..=n` of one stream held in memory, with every digest and the
/// chain verified (built by [`MemoryHistory::verified`]).
#[derive(Debug, Clone)]
pub struct MemoryHistory {
    stream: StreamId,
    events: Vec<StoredEvent>,
}

impl MemoryHistory {
    /// Checks that `events` are events `1..=n` of `stream` in order, that
    /// each event's digest is the `ORBIT-EVENT-DIGEST-V1` digest of its own
    /// fields, and that each links to the previous one.
    ///
    /// # Errors
    /// A description of the first problem.
    pub fn verified(stream: &str, events: Vec<StoredEvent>) -> Result<Self, String> {
        let stream = StreamId::new(stream).map_err(|e| e.to_string())?;
        let mut prev = Digest::GENESIS;
        for (i, event) in events.iter().enumerate() {
            let expected = i as u64 + 1;
            if event.stream_id != stream || event.sequence.get() != expected {
                return Err(format!("event {expected} is missing or out of order"));
            }
            if event.prev_digest != prev {
                return Err(format!(
                    "event {expected} does not link to event {}",
                    expected - 1
                ));
            }
            let computed = orbit_core::integrity::EventDigestInput {
                stream_id: event.stream_id.as_str(),
                sequence: event.sequence,
                event_id: event.event_id,
                event_type: event.event_type.as_str(),
                schema_version: event.schema_version,
                recorded_at: event.recorded_at,
                payload: &to_canonical_string(&event.payload),
                metadata: &to_canonical_string(&Value::Object(event.metadata.clone())),
                prev_digest: event.prev_digest,
            }
            .compute();
            if computed != event.digest {
                return Err(format!("event {expected} does not match its digest"));
            }
            prev = event.digest;
        }
        Ok(Self { stream, events })
    }

    /// Number of events.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.events.len() as u64
    }

    /// Whether there are no events.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// The input range of `spec` over these events, computed exactly as
    /// [`resolve_input`] computes it from a store.
    ///
    /// # Errors
    /// A [`Failure`] if the range is not within these events.
    pub fn resolve(&self, spec: &DifferentialSpec) -> Result<InputRange, Failure> {
        let head = self.len();
        let to = spec.to.unwrap_or(head);
        if to > head || spec.from > to || to > MAX_EVENTS || spec.stream != self.stream.as_str() {
            return Err(Failure::new(
                FailureKind::HistoryUnavailable,
                format!(
                    "the history holds {head} events of {}; the experiment needs {}..={to} of {}",
                    self.stream, spec.from, spec.stream
                ),
            ));
        }
        let digest_at = |n: u64| {
            if n == 0 {
                Digest::GENESIS
            } else {
                self.events[usize::try_from(n - 1).unwrap_or(usize::MAX)].digest
            }
        };
        Ok(history_range(
            &self.stream,
            spec.from,
            to,
            digest_at(spec.from),
            digest_at(to),
        ))
    }
}

impl EventSource for MemoryHistory {
    fn read_after(
        &self,
        stream: &StreamId,
        after: Sequence,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        if *stream != self.stream {
            return Err(StoreError::StreamNotFound(stream.clone()));
        }
        let start = usize::try_from(after.get()).unwrap_or(usize::MAX);
        Ok(self
            .events
            .iter()
            .skip(start)
            .take(limit)
            .cloned()
            .collect())
    }
}

fn history_range(
    stream: &StreamId,
    from: u64,
    to: u64,
    from_event_digest: Digest,
    to_event_digest: Digest,
) -> InputRange {
    let mut hasher = TaggedHasher::new(HISTORY_TAG);
    hasher
        .field(stream.as_str().as_bytes())
        .field(&from.to_be_bytes())
        .field(&to.to_be_bytes())
        .field(from_event_digest.as_bytes())
        .field(to_event_digest.as_bytes());
    InputRange {
        stream: stream.to_string(),
        from,
        to,
        from_event_digest,
        to_event_digest,
        history_fingerprint: hasher.finish(),
    }
}

/// Resolves and verifies the input history of `spec`.
///
/// # Errors
/// A [`Failure`] if the history is unavailable, damaged or the range invalid.
pub fn resolve_input(store: &EventStore, spec: &DifferentialSpec) -> Result<InputRange, Failure> {
    let stream = StreamId::new(spec.stream.as_str())
        .map_err(|e| Failure::new(FailureKind::InvalidRange, e.to_string()))?;
    let head = match store.head(&stream) {
        Ok(head) => head.sequence.get(),
        Err(StoreError::StreamNotFound(_)) => {
            return Err(Failure::new(
                FailureKind::HistoryUnavailable,
                format!("stream {stream} does not exist"),
            ));
        }
        Err(e) => return Err(internal(&e)),
    };
    let to = spec.to.unwrap_or(head);
    if to > head {
        return Err(Failure {
            kind: FailureKind::HistoryUnavailable,
            message: format!("stream {stream} has {head} events; the experiment needs {to}"),
            details: json!({ "required": to, "head": head }),
        });
    }
    if spec.from > to {
        return Err(Failure {
            kind: FailureKind::InvalidRange,
            message: format!("from ({}) is after to ({to})", spec.from),
            details: json!({ "from": spec.from, "to": to }),
        });
    }
    if to > MAX_EVENTS {
        return Err(Failure::new(
            FailureKind::InvalidRange,
            format!("at most {MAX_EVENTS} events can be replayed"),
        ));
    }
    let report = store.verify_stream(&stream).map_err(|e| internal(&e))?;
    if !report.is_valid() {
        return Err(Failure {
            kind: FailureKind::IntegrityFailure,
            message: format!(
                "stream {stream} failed integrity verification ({} issues); nothing was replayed",
                report.total_issues
            ),
            details: json!({ "categories": report.categories(), "total_issues": report.total_issues }),
        });
    }
    let digest_at = |n: u64| -> Result<Digest, Failure> {
        if n == 0 {
            return Ok(Digest::GENESIS);
        }
        store
            .event(&stream, seq(n)?)
            .map(|e| e.digest)
            .map_err(|e| internal(&e))
    };
    let from_event_digest = digest_at(spec.from)?;
    let to_event_digest = digest_at(to)?;
    Ok(history_range(
        &stream,
        spec.from,
        to,
        from_event_digest,
        to_event_digest,
    ))
}

/// Calls `f` for events `1..=through` in order, checking continuity, until
/// `f` breaks. Returns the number of events passed to `f`.
fn walk<S: EventSource + ?Sized>(
    store: &S,
    input: &InputRange,
    through: u64,
    mut f: impl FnMut(&StoredEvent) -> ControlFlow<()>,
) -> Result<u64, Failure> {
    let stream = StreamId::new(input.stream.as_str())
        .map_err(|e| Failure::new(FailureKind::InvalidRange, e.to_string()))?;
    let changed = |message: String| Failure::new(FailureKind::IntegrityFailure, message);
    let mut last = Digest::GENESIS;
    let mut seen = 0u64;
    while seen < through {
        let page = store
            .read_after(&stream, seq(seen)?, PAGE_SIZE)
            .map_err(|e| internal(&e))?;
        if page.is_empty() {
            return Err(changed(format!("history ends at {seen}, before {through}")));
        }
        for event in &page {
            if seen == through {
                break;
            }
            if event.sequence.get() != seen + 1 || event.prev_digest != last {
                return Err(changed(format!(
                    "history is not continuous at sequence {}",
                    seen + 1
                )));
            }
            last = event.digest;
            seen += 1;
            if f(event).is_break() {
                return Ok(seen);
            }
        }
    }
    if through == input.to && last != input.to_event_digest {
        return Err(changed(format!(
            "event {through} no longer has the digest recorded for this experiment"
        )));
    }
    Ok(seen)
}

/// One side during execution.
struct Lane<'a> {
    side: Side,
    implementation: &'a dyn Implementation,
    stepper: Option<Box<dyn Stepper>>,
    trace: Vec<Digest>,
    failure: Option<SideFailure>,
}

impl<'a> Lane<'a> {
    fn start(side: Side, implementation: &'a dyn Implementation) -> Self {
        let mut lane = Self {
            side,
            implementation,
            stepper: None,
            trace: Vec::new(),
            failure: None,
        };
        match implementation.start() {
            Ok(stepper) => lane.stepper = Some(stepper),
            Err(failure) => lane.fail(0, None, failure),
        }
        lane
    }

    fn fail(&mut self, boundary: u64, event: Option<&StoredEvent>, failure: StepFailure) {
        self.stepper = None;
        self.failure = Some(SideFailure {
            side: self.side,
            implementation: self.implementation.descriptor().id.to_string(),
            boundary,
            event: event.map(EventRef::from),
            failure,
        });
    }

    fn apply(&mut self, event: &StoredEvent) {
        if let Some(stepper) = self.stepper.as_mut()
            && let Err(failure) = stepper.apply(event)
        {
            self.fail(event.sequence.get(), Some(event), failure);
        }
    }

    fn state(&mut self, boundary: u64, event: Option<&StoredEvent>) -> Option<Value> {
        let result = self.stepper.as_ref()?.state();
        match result {
            Ok(value) => Some(value),
            Err(failure) => {
                self.fail(boundary, event, failure);
                None
            }
        }
    }
}

/// Runs one implementation alone over events `1..=stop` (observing
/// boundaries `from..=stop`), stopping at its first failure. `stop = None`
/// only starts it.
fn solo<S: EventSource + ?Sized>(
    store: &S,
    input: &InputRange,
    side: Side,
    implementation: &dyn Implementation,
    stop: Option<u64>,
) -> Result<(Vec<Digest>, Option<SideFailure>), Failure> {
    let mut lane = Lane::start(side, implementation);
    let Some(stop) = stop else {
        return Ok((lane.trace, lane.failure));
    };
    let observe = |lane: &mut Lane<'_>, b: u64, e: Option<&StoredEvent>| {
        if let Some(state) = lane.state(b, e)
            && b >= input.from
        {
            lane.trace.push(state_digest(&state));
        }
    };
    if lane.failure.is_none() {
        observe(&mut lane, 0, None);
    }
    if stop > 0 && lane.failure.is_none() {
        walk(store, input, stop, |event| {
            lane.apply(event);
            if lane.failure.is_some() {
                return ControlFlow::Break(());
            }
            observe(&mut lane, event.sequence.get(), Some(event));
            if lane.failure.is_some() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })?;
    }
    Ok((lane.trace, lane.failure))
}

fn trace_bytes(trace: &[Digest]) -> Vec<u8> {
    trace.iter().flat_map(|d| *d.as_bytes()).collect()
}

/// An interval while it is still open.
struct OpenInterval {
    index: u64,
    first: u64,
    last: u64,
    recorded: bool,
    diagnosed: bool,
    entry: Option<Comparison>,
    /// Latest differing boundary after `first`, with both states, while diagnosed.
    pending_exit: Option<(u64, Option<EventRef>, Value, Value)>,
}

impl OpenInterval {
    fn close(
        self,
        end: IntervalEnd,
        converged_at: Option<u64>,
        convergence_event: Option<EventRef>,
    ) -> DivergenceInterval {
        DivergenceInterval {
            index: self.index,
            first: self.first,
            last: self.last,
            boundaries: self.last - self.first + 1,
            end,
            converged_at,
            convergence_event,
            entry: self.entry,
            exit: self
                .pending_exit
                .map(|(b, event, l, r)| Comparison::new(b, event, &l, &r)),
        }
    }
}

/// Builds the timeline as boundaries are observed.
struct TimelineBuilder {
    first_boundary: u64,
    last_boundary: Option<u64>,
    compared: u64,
    differing: u64,
    intervals: Vec<DivergenceInterval>,
    intervals_total: u64,
    convergences: u64,
    open: Option<OpenInterval>,
    first: Option<Divergence>,
    first_states: Option<Value>,
    final_equal: Option<bool>,
}

impl TimelineBuilder {
    fn new(first_boundary: u64) -> Self {
        Self {
            first_boundary,
            last_boundary: None,
            compared: 0,
            differing: 0,
            intervals: Vec::new(),
            intervals_total: 0,
            convergences: 0,
            open: None,
            first: None,
            first_states: None,
            final_equal: None,
        }
    }

    fn push(
        &mut self,
        interval: OpenInterval,
        end: IntervalEnd,
        at: Option<u64>,
        event: Option<EventRef>,
    ) {
        if interval.recorded {
            self.intervals.push(interval.close(end, at, event));
        }
    }

    /// Records the comparison of `left` and `right` at boundary `b`.
    fn observe(
        &mut self,
        b: u64,
        event: Option<&StoredEvent>,
        equal: bool,
        left: &Value,
        right: &Value,
    ) {
        self.compared += 1;
        self.last_boundary = Some(b);
        self.final_equal = Some(equal);
        if equal {
            if let Some(open) = self.open.take() {
                self.convergences += 1;
                self.push(
                    open,
                    IntervalEnd::Converged,
                    Some(b),
                    event.map(EventRef::from),
                );
            }
            return;
        }
        self.differing += 1;
        if self.first.is_none() {
            self.first = Some(Divergence {
                comparison: Comparison::new(b, event.map(EventRef::from), left, right),
                boundaries_matched_before: self.compared - 1,
            });
            self.first_states = Some(json!({ "left": left, "right": right }));
        }
        match self.open.as_mut() {
            Some(open) => {
                open.last = b;
                if open.diagnosed {
                    open.pending_exit =
                        Some((b, event.map(EventRef::from), left.clone(), right.clone()));
                }
            }
            None => {
                self.intervals_total += 1;
                let index = self.intervals_total;
                let diagnosed = index <= MAX_DIAGNOSED_INTERVALS as u64;
                let entry = if index == 1 {
                    self.first.as_ref().map(|d| d.comparison.clone())
                } else if diagnosed {
                    Some(Comparison::new(b, event.map(EventRef::from), left, right))
                } else {
                    None
                };
                self.open = Some(OpenInterval {
                    index,
                    first: b,
                    last: b,
                    recorded: self.intervals.len() < MAX_RECORDED_INTERVALS,
                    diagnosed,
                    entry,
                    pending_exit: None,
                });
            }
        }
    }

    fn finish(mut self, failed: bool) -> (Timeline, Option<Divergence>, Option<Value>, u64, u64) {
        if let Some(open) = self.open.take() {
            let end = if failed {
                IntervalEnd::StoppedByFailure
            } else {
                IntervalEnd::OpenAtEnd
            };
            self.push(open, end, None, None);
        }
        let timeline = Timeline {
            first_boundary: self.first_boundary,
            last_boundary: self.last_boundary,
            intervals_truncated: (self.intervals.len() as u64) < self.intervals_total,
            intervals: self.intervals,
            intervals_total: self.intervals_total,
            convergences: self.convergences,
            final_equal: self.final_equal,
        };
        (
            timeline,
            self.first,
            self.first_states,
            self.compared,
            self.differing,
        )
    }
}

fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else if let Some(stem) = word.strip_suffix("ary") {
        format!("{n} {stem}aries")
    } else {
        format!("{n} {word}s")
    }
}

fn describe_event(e: &EventRef) -> String {
    format!(
        "event #{} ({}, schema v{})",
        e.sequence, e.event_type, e.schema_version
    )
}

fn diagnose(
    timeline: &Timeline,
    first: Option<&Divergence>,
    failures: &[SideFailure],
    compared: u64,
    differing: u64,
    reference: Option<Side>,
) -> Diagnosis {
    let mut evidence = Vec::new();
    let mut not_established = Vec::new();
    match first {
        None if compared > 0 => evidence.push(format!(
            "The states were equal at all {} compared (boundaries {}–{}).",
            plural(compared, "boundary"),
            timeline.first_boundary,
            timeline.last_boundary.unwrap_or(timeline.first_boundary)
        )),
        None => evidence.push("No boundary was compared.".to_owned()),
        Some(d) => {
            let c = &d.comparison;
            let at = match &c.event {
                None => "in the initial states (boundary 0), before any event".to_owned(),
                Some(e) => format!("at boundary {}, after {}", c.boundary, describe_event(e)),
            };
            if d.boundaries_matched_before == 0 {
                evidence.push(format!(
                    "The states already differ at the first compared boundary, {at}."
                ));
            } else {
                evidence.push(format!(
                    "The states were equal at {} and first differ {at}.",
                    plural(d.boundaries_matched_before, "compared boundary")
                ));
            }
            let s = c.change_summary;
            evidence.push(format!(
                "There, {} {}: {} only in the right state, {} only in the left state, {} in both with different values ({} with a different JSON type).",
                plural(c.changes_total, "JSON path"),
                if c.changes_total == 1 { "differs" } else { "differ" },
                s.added,
                s.removed,
                s.modified,
                s.type_changed
            ));
            evidence.push(format!(
                "In total, {} of {} differ, in {}; the states became equal again {}.",
                plural(differing, "boundary"),
                compared,
                plural(timeline.intervals_total, "divergence interval"),
                plural(timeline.convergences, "time")
            ));
        }
    }
    match timeline.final_equal {
        Some(true) if first.is_some() && failures.is_empty() => {
            evidence.push("At the last compared boundary the states are equal.".to_owned());
        }
        Some(false) if failures.is_empty() => {
            evidence.push("At the last compared boundary the states differ.".to_owned());
        }
        _ => {}
    }
    for f in failures {
        evidence.push(format!(
            "The {} implementation ({}) failed at boundary {}.",
            f.side.as_str(),
            f.implementation,
            f.boundary
        ));
    }
    if let Some(d) = first {
        not_established.push(
            "Why the states differ. The first differing boundary is where a difference first becomes visible in the serialized states, not necessarily where it was caused.".to_owned(),
        );
        if d.comparison.boundary > 0 {
            not_established.push(
                "Whether the cause is in how either implementation handles this event, or in earlier internal state that the serialized states do not expose.".to_owned(),
            );
        }
        not_established.push(match reference {
            None => "Which implementation, if either, is correct: no reference side was designated.".to_owned(),
            Some(side) => format!(
                "Whether the {} implementation is correct: it was designated as the reference, and the lab does not check that.",
                side.as_str()
            ),
        });
    }
    Diagnosis {
        evidence,
        not_established,
    }
}

/// Executes a differential replay over an already resolved input.
///
/// # Errors
/// A [`Failure`] if the history changes or cannot be read while executing.
pub fn execute<S: EventSource + ?Sized>(
    store: &S,
    spec: &DifferentialSpec,
    input: &InputRange,
    left: &dyn Implementation,
    right: &dyn Implementation,
) -> Result<DifferentialRun, Failure> {
    let mut lanes = [
        Lane::start(Side::Left, left),
        Lane::start(Side::Right, right),
    ];
    let mut events_hasher = TaggedHasher::new(EVENTS_TAG);
    let mut events_processed = 0u64;
    let mut timeline = TimelineBuilder::new(input.from);
    let mut last_observed: Option<u64> = None;
    let mut last_states: Option<(Value, Value)> = None;
    let mut final_event: Option<EventRef> = None;

    let mut observe = |lanes: &mut [Lane<'_>; 2], b: u64, event: Option<&StoredEvent>| -> bool {
        let l = lanes[0].state(b, event);
        let r = lanes[1].state(b, event);
        let (Some(l), Some(r)) = (l, r) else {
            return false;
        };
        last_observed = Some(b);
        if b >= input.from {
            let (dl, dr) = (state_digest(&l), state_digest(&r));
            lanes[0].trace.push(dl);
            lanes[1].trace.push(dr);
            timeline.observe(b, event, dl == dr, &l, &r);
        }
        last_states = Some((l, r));
        final_event = event.map(EventRef::from);
        true
    };

    if lanes.iter().all(|l| l.failure.is_none()) {
        let ok = observe(&mut lanes, 0, None);
        if ok && input.to > 0 {
            walk(store, input, input.to, |event| {
                events_hasher.field(event.digest.as_bytes());
                events_processed += 1;
                for lane in &mut lanes {
                    lane.apply(event);
                }
                if lanes.iter().any(|l| l.failure.is_some()) {
                    return ControlFlow::Break(());
                }
                if observe(&mut lanes, event.sequence.get(), Some(event)) {
                    ControlFlow::Continue(())
                } else {
                    ControlFlow::Break(())
                }
            })?;
        }
    }

    let failures: Vec<SideFailure> = lanes.iter().filter_map(|l| l.failure.clone()).collect();
    let (timeline, first, first_states, compared, differing) =
        timeline.finish(!failures.is_empty());
    let final_comparison = match (&last_states, failures.is_empty()) {
        (Some((l, r)), true) => Some(Comparison::new(input.to, final_event, l, r)),
        _ => None,
    };

    // Determinism: each side again, alone, over exactly what it saw.
    let declared_uncontrolled: Vec<Side> = lanes
        .iter()
        .filter(|l| l.implementation.descriptor().determinism == Determinism::Uncontrolled)
        .map(|l| l.side)
        .collect();
    let mut inconsistency = None;
    if spec.determinism_check {
        for lane in &lanes {
            let stop = lane.failure.as_ref().map(|f| f.boundary).or(last_observed);
            let (trace, failure) = solo(store, input, lane.side, lane.implementation, stop)?;
            if trace != lane.trace || failure != lane.failure {
                let index = trace
                    .iter()
                    .zip(&lane.trace)
                    .position(|(a, b)| a != b)
                    .unwrap_or_else(|| trace.len().min(lane.trace.len()));
                inconsistency = Some(Inconsistency {
                    side: lane.side,
                    boundary: input.from + index as u64,
                });
                break;
            }
        }
    }
    let determinism = DeterminismReport {
        checked: spec.determinism_check,
        consistent: spec.determinism_check.then_some(inconsistency.is_none()),
        inconsistency,
        declared_uncontrolled: declared_uncontrolled.clone(),
    };

    let outcome = if inconsistency.is_some() {
        Outcome::Nondeterministic
    } else if !failures.is_empty() {
        Outcome::ImplementationFailed
    } else if first.is_none() {
        Outcome::Equivalent
    } else if final_comparison.as_ref().is_some_and(|c| c.equal) {
        Outcome::Reconverged
    } else {
        Outcome::Diverged
    };

    let mut reasons = Vec::new();
    if let Some(i) = inconsistency {
        reasons.push(format!(
            "the {} implementation produced different results on two executions (first at boundary {})",
            i.side.as_str(),
            i.boundary
        ));
    }
    for side in &declared_uncontrolled {
        reasons.push(format!(
            "the {} implementation declares uncontrolled (nondeterministic) dependencies",
            side.as_str()
        ));
    }
    if !spec.determinism_check {
        reasons.push("the determinism check was disabled".to_owned());
    }
    let status = if inconsistency.is_some() || !declared_uncontrolled.is_empty() {
        ReproducibilityStatus::NotReproducible
    } else if spec.determinism_check {
        reasons.push(
            "input pinned by event digests; both implementations gave identical traces on two executions"
                .to_owned(),
        );
        ReproducibilityStatus::Reproducible
    } else {
        ReproducibilityStatus::Unverified
    };

    let identity = |lane: &Lane<'_>| SideIdentity {
        implementation: lane.implementation.descriptor().id.to_string(),
        fingerprint: lane.implementation.descriptor().fingerprint,
    };
    let mut artifacts = vec![
        (
            "left_trace".to_owned(),
            "application/octet-stream",
            trace_bytes(&lanes[0].trace),
        ),
        (
            "right_trace".to_owned(),
            "application/octet-stream",
            trace_bytes(&lanes[1].trace),
        ),
    ];
    if let Some(states) = &first_states {
        artifacts.push((
            "first_divergence_states".to_owned(),
            "application/json",
            to_canonical_string(states).into_bytes(),
        ));
    }
    if let (Some((l, r)), true) = (&last_states, failures.is_empty()) {
        artifacts.push((
            "final_states".to_owned(),
            "application/json",
            to_canonical_string(&json!({ "left": l, "right": r })).into_bytes(),
        ));
    }

    let diagnosis = diagnose(
        &timeline,
        first.as_ref(),
        &failures,
        compared,
        differing,
        spec.reference,
    );
    let result = DifferentialResult {
        result_version: RESULT_VERSION,
        semantics: Semantics::current(),
        outcome,
        reference: spec.reference,
        perspective: Perspective::new(spec.reference),
        left: identity(&lanes[0]),
        right: identity(&lanes[1]),
        input: input.clone(),
        events_processed,
        events_fed_digest: events_hasher.finish(),
        boundaries_compared: compared,
        boundaries_differing: differing,
        first_divergence: first,
        timeline,
        final_comparison,
        failures,
        determinism,
        reproducibility: Reproducibility { status, reasons },
        diagnosis,
    };
    Ok(DifferentialRun { result, artifacts })
}
