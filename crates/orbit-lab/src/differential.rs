//! Differential replay: two implementations, one ordered history.
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
//! 3. **Compare at every boundary.** Boundary `b` is the state after event
//!    `b` (0 is the initial state). For `from <= b <= to`, each state is
//!    serialized with `serde_json` and compared by
//!    `orbit_core::replay::state_digest`, i.e. by canonical
//!    JSON. At the first differing boundary the lab records both state
//!    summaries and the structural diff (`orbit_core::diff::diff`, left →
//!    right). Every maximal run of differing boundaries is a *segment*.
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
//! Structural changes always describe how the right state differs from the
//! left one.

use std::ops::ControlFlow;

use orbit_core::diff::{Change, diff};
use orbit_core::domain::canonical::to_canonical_string;
use orbit_core::replay::state_digest;
use orbit_core::{
    Digest, EventId, EventStore, EventType, SchemaVersion, Sequence, StoreError, StoredEvent,
    StreamId,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::catalog::{Implementation, StepFailure, Stepper};
use crate::digest::{EVENTS_TAG, HISTORY_TAG, TaggedHasher};
use crate::identity::Determinism;
use crate::registry::{Failure, FailureKind, InputRange};
use crate::spec::{DifferentialSpec, MAX_EVENTS, Side};

/// Events read from the store per page.
pub const PAGE_SIZE: usize = 1_000;
/// Structural changes recorded per comparison; the total is always reported.
pub const MAX_RECORDED_CHANGES: usize = 200;
/// Divergent segments recorded; the total is always reported.
pub const MAX_RECORDED_SEGMENTS: usize = 100;

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

/// The event that produced a boundary.
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
}

impl From<&StoredEvent> for EventRef {
    fn from(e: &StoredEvent) -> Self {
        Self {
            sequence: e.sequence,
            event_id: e.event_id,
            event_type: e.event_type.clone(),
            schema_version: e.schema_version,
        }
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
}

impl Comparison {
    fn new(boundary: u64, event: Option<EventRef>, left: &Value, right: &Value) -> Self {
        let l = StateSummary::of(left);
        let r = StateSummary::of(right);
        let equal = l.digest == r.digest;
        let mut changes = if equal { Vec::new() } else { diff(left, right) };
        let changes_total = changes.len() as u64;
        changes.truncate(MAX_RECORDED_CHANGES);
        Self {
            boundary,
            event,
            left: l,
            right: r,
            equal,
            changes,
            changes_total,
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

/// A maximal run of consecutive differing boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Segment {
    /// First differing boundary.
    pub first: u64,
    /// Last differing boundary.
    pub last: u64,
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

/// The result of a differential replay. Contains no timings, ids or
/// timestamps, so executing the same experiment on the same input yields
/// the same document (and the same digest).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DifferentialResult {
    /// Outcome.
    pub outcome: Outcome,
    /// Reference side, if any (its state is the expected one).
    pub reference: Option<Side>,
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
    /// Runs of differing boundaries (at most [`MAX_RECORDED_SEGMENTS`]).
    pub divergent_segments: Vec<Segment>,
    /// Number of segments in total.
    pub divergent_segments_total: u64,
    /// Comparison at `to`, when both implementations got there.
    pub final_comparison: Option<Comparison>,
    /// Implementation failures.
    pub failures: Vec<SideFailure>,
    /// Determinism check.
    pub determinism: DeterminismReport,
    /// Reproducibility.
    pub reproducibility: Reproducibility,
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
    let mut hasher = TaggedHasher::new(HISTORY_TAG);
    hasher
        .field(stream.as_str().as_bytes())
        .field(&spec.from.to_be_bytes())
        .field(&to.to_be_bytes())
        .field(from_event_digest.as_bytes())
        .field(to_event_digest.as_bytes());
    Ok(InputRange {
        stream: stream.to_string(),
        from: spec.from,
        to,
        from_event_digest,
        to_event_digest,
        history_fingerprint: hasher.finish(),
    })
}

/// Calls `f` for events `1..=through` in order, checking continuity, until
/// `f` breaks. Returns the number of events passed to `f`.
fn walk(
    store: &EventStore,
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
fn solo(
    store: &EventStore,
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

/// Executes a differential replay over an already resolved input.
///
/// # Errors
/// A [`Failure`] if the history changes or cannot be read while executing.
pub fn execute(
    store: &EventStore,
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
    let mut compared = 0u64;
    let mut differing = 0u64;
    let mut first: Option<Divergence> = None;
    let mut first_states: Option<Value> = None;
    let mut segments: Vec<Segment> = Vec::new();
    let mut segments_total = 0u64;
    let mut in_segment = false;
    let mut recording_segment = false;
    let mut final_event: Option<EventRef> = None;
    let mut last_observed: Option<u64> = None;
    let mut last_states: Option<(Value, Value)> = None;

    let mut observe = |lanes: &mut [Lane<'_>; 2], b: u64, event: Option<&StoredEvent>| -> bool {
        let l = lanes[0].state(b, event);
        let r = lanes[1].state(b, event);
        let (Some(l), Some(r)) = (l, r) else {
            return false;
        };
        last_observed = Some(b);
        if b >= input.from {
            lanes[0].trace.push(state_digest(&l));
            lanes[1].trace.push(state_digest(&r));
            compared += 1;
            let equal = lanes[0].trace.last() == lanes[1].trace.last();
            if equal {
                in_segment = false;
            } else {
                differing += 1;
                if first.is_none() {
                    first = Some(Divergence {
                        comparison: Comparison::new(b, event.map(EventRef::from), &l, &r),
                        boundaries_matched_before: compared - 1,
                    });
                    first_states = Some(json!({ "left": l, "right": r }));
                }
                if in_segment {
                    if recording_segment && let Some(s) = segments.last_mut() {
                        s.last = b;
                    }
                } else {
                    in_segment = true;
                    segments_total += 1;
                    recording_segment = segments.len() < MAX_RECORDED_SEGMENTS;
                    if recording_segment {
                        segments.push(Segment { first: b, last: b });
                    }
                }
            }
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
                let boundary = input.from + index as u64;
                inconsistency = Some(Inconsistency {
                    side: lane.side,
                    boundary,
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

    let result = DifferentialResult {
        outcome,
        reference: spec.reference,
        left: identity(&lanes[0]),
        right: identity(&lanes[1]),
        input: input.clone(),
        events_processed,
        events_fed_digest: events_hasher.finish(),
        boundaries_compared: compared,
        boundaries_differing: differing,
        first_divergence: first,
        divergent_segments: segments,
        divergent_segments_total: segments_total,
        final_comparison,
        failures,
        determinism,
        reproducibility: Reproducibility { status, reasons },
    };
    Ok(DifferentialRun { result, artifacts })
}
