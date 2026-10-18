//! Request and response types of the HTTP API.
//!
//! Resources (streams, events, snapshots, pages) have dedicated types.
//! Engine reports (reconstruction reports, diffs, integrity reports, replay
//! runs and executions, experiments) are the engine's own serializable types,
//! returned unchanged and documented as objects; their fields are described
//! in `docs/API.md` and the engine documentation.

use orbit_core::replay::ReplayStart;
use orbit_core::{SnapshotPolicy, StoredEvent, StreamSummary};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};

/// Default page size for list endpoints.
pub const DEFAULT_PAGE_SIZE: usize = 50;
/// Maximum page size for list endpoints.
pub const MAX_PAGE_SIZE: usize = 500;
/// Maximum interval length for `/explain` (it serializes the state after every event).
pub const MAX_EXPLAIN_EVENTS: u64 = 1_000;

/// `GET /health`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HealthResponse {
    /// `ok` when the database answered a query.
    pub status: String,
    /// API version, `v1`.
    pub api_version: String,
    /// `orbit-core` crate version.
    pub engine_version: String,
    /// Database status.
    pub database: DatabaseHealth,
}

/// Database part of the health response. Paths are deliberately not exposed.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DatabaseHealth {
    /// Whether a query succeeded.
    pub reachable: bool,
    /// Schema version of the database file.
    pub schema_version: i64,
}

/// A projection the API can run.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectionInfo {
    /// Name, e.g. `merge-patch`.
    pub id: String,
    /// Version.
    pub version: u32,
    /// `name@vN`.
    pub key: String,
    /// What it does.
    pub description: String,
}

/// Limits enforced by the API and the engine.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LimitsInfo {
    /// Maximum canonical payload size of one event, in bytes.
    pub max_payload_bytes: usize,
    /// Maximum canonical metadata size, in bytes.
    pub max_metadata_bytes: usize,
    /// Maximum HTTP request body size, in bytes.
    pub max_body_bytes: usize,
    /// Default page size.
    pub default_page_size: usize,
    /// Maximum page size.
    pub max_page_size: usize,
    /// Maximum events in one `/explain` interval.
    pub max_explain_events: u64,
    /// Maximum events in one replay run.
    pub max_replay_events: u64,
}

/// `GET /capabilities`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CapabilitiesResponse {
    /// API version.
    pub api_version: String,
    /// Engine version.
    pub engine_version: String,
    /// Projection used when a request names none.
    pub default_projection: String,
    /// Registered projections.
    pub projections: Vec<ProjectionInfo>,
    /// Snapshot policies accepted by `snapshots=` parameters.
    pub snapshot_policies: Vec<String>,
    /// Limits.
    pub limits: LimitsInfo,
    /// Whether Criterion benchmark results are configured.
    pub benchmarks_available: bool,
}

/// Row counts of the store.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StatisticsDto {
    /// Streams.
    pub streams: u64,
    /// Events.
    pub events: u64,
    /// Snapshots.
    pub snapshots: u64,
    /// Replay runs.
    pub replay_runs: u64,
    /// Replay executions.
    pub replay_executions: u64,
    /// Schema version.
    pub schema_version: i64,
}

/// `GET /overview`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct OverviewResponse {
    /// Store-wide counts.
    pub statistics: StatisticsDto,
    /// Most recently recorded events across all streams (by `recorded_at`; informational).
    pub recent_events: Vec<EventDto>,
    /// Streams with the most recent activity first (up to 10).
    pub active_streams: Vec<StreamSummaryDto>,
}

/// A stream with summary information.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StreamSummaryDto {
    /// Stream id.
    pub stream_id: String,
    /// Creation time, RFC 3339.
    pub created_at: String,
    /// Head sequence (= number of events).
    pub head_sequence: u64,
    /// Head digest (64 hex chars; all zeros for an empty stream).
    pub head_digest: String,
    /// Time the head event was recorded, if any.
    pub last_recorded_at: Option<String>,
    /// Snapshots stored for the stream.
    pub snapshot_count: u64,
    /// Replay runs recorded for the stream.
    pub replay_run_count: u64,
}

impl From<StreamSummary> for StreamSummaryDto {
    fn from(s: StreamSummary) -> Self {
        Self {
            stream_id: s.stream_id.to_string(),
            created_at: s.created_at.to_rfc3339(),
            head_sequence: s.head.sequence.get(),
            head_digest: s.head.digest.to_hex(),
            last_recorded_at: s.last_recorded_at.map(|t| t.to_rfc3339()),
            snapshot_count: s.snapshot_count,
            replay_run_count: s.replay_run_count,
        }
    }
}

/// `GET /streams/{stream_id}`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StreamDetail {
    /// Summary.
    #[serde(flatten)]
    pub summary: StreamSummaryDto,
    /// Immutable stream metadata.
    #[schema(value_type = Object)]
    pub metadata: Value,
}

/// A page of streams.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StreamPage {
    /// Streams ordered by id.
    pub streams: Vec<StreamSummaryDto>,
    /// Pass as `after` to get the next page; absent on the last page.
    pub next_after: Option<String>,
}

/// A stored event.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EventDto {
    /// Event id (UUID).
    pub event_id: String,
    /// Stream id.
    pub stream_id: String,
    /// Sequence within the stream (authoritative order).
    pub sequence: u64,
    /// Event type.
    pub event_type: String,
    /// Payload schema version.
    pub schema_version: u32,
    /// Time recorded by the store, RFC 3339 (informational).
    pub recorded_at: String,
    /// Payload.
    #[schema(value_type = Object)]
    pub payload: Value,
    /// Metadata.
    #[schema(value_type = Object)]
    pub metadata: Value,
    /// Digest of the previous event (hex).
    pub prev_digest: String,
    /// Digest of this event (hex).
    pub digest: String,
}

impl From<StoredEvent> for EventDto {
    fn from(e: StoredEvent) -> Self {
        Self {
            event_id: e.event_id.to_string(),
            stream_id: e.stream_id.to_string(),
            sequence: e.sequence.get(),
            event_type: e.event_type.to_string(),
            schema_version: e.schema_version.get(),
            recorded_at: e.recorded_at.to_rfc3339(),
            payload: e.payload,
            metadata: Value::Object(e.metadata),
            prev_digest: e.prev_digest.to_hex(),
            digest: e.digest.to_hex(),
        }
    }
}

/// A page of events in ascending sequence order.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EventPage {
    /// Events.
    pub events: Vec<EventDto>,
    /// Pass as `after` to continue; absent when the page reached the head.
    pub next_after: Option<u64>,
    /// Stream head at the time of the request.
    pub head_sequence: u64,
}

/// `POST /streams`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateStreamRequest {
    /// New stream id: 1–128 chars of `[A-Za-z0-9._:-]`, starting with a letter or digit.
    pub stream_id: String,
    /// Optional immutable metadata object.
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<serde_json::Map<String, Value>>,
}

/// `POST /streams/{stream_id}/events`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AppendEventRequest {
    /// Event type, same grammar as stream ids.
    pub event_type: String,
    /// Payload (any JSON value).
    #[schema(value_type = Object)]
    pub payload: Value,
    /// Optional metadata object.
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<serde_json::Map<String, Value>>,
    /// Payload schema version (default 1).
    pub schema_version: Option<u32>,
    /// Optimistic concurrency: append only if the head is exactly this
    /// sequence (0 = empty stream). Omit to append unconditionally.
    pub expected_sequence: Option<u64>,
    /// Caller-chosen event id (UUID) for idempotent retries.
    pub event_id: Option<uuid::Uuid>,
}

/// `?after=&limit=` for event pages.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventPageQuery {
    /// Return events with sequence strictly greater than this (default 0).
    pub after: Option<u64>,
    /// Page size, 1–500 (default 50).
    pub limit: Option<usize>,
}

/// `?after=&limit=` for stream pages.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StreamPageQuery {
    /// Return streams whose id sorts after this one.
    pub after: Option<String>,
    /// Page size, 1–500 (default 50).
    pub limit: Option<usize>,
}

/// Snapshot policy as a query value.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyParam {
    /// Never use snapshots.
    Ignore,
    /// Use the newest valid snapshot; skip and report invalid ones (default).
    #[default]
    Prefer,
    /// Fail on the first invalid snapshot candidate.
    Strict,
}

impl From<PolicyParam> for SnapshotPolicy {
    fn from(p: PolicyParam) -> Self {
        match p {
            PolicyParam::Ignore => Self::Ignore,
            PolicyParam::Prefer => Self::Prefer,
            PolicyParam::Strict => Self::Strict,
        }
    }
}

/// `GET /streams/{stream_id}/state`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StateQuery {
    /// Projection name or key (default `merge-patch`).
    pub projection: Option<String>,
    /// Historical sequence; omit for the latest state.
    pub at: Option<u64>,
    /// Snapshot policy (default `prefer`).
    pub snapshots: Option<PolicyParam>,
    /// Include every applied event in the report.
    pub record_events: Option<bool>,
}

/// `GET /streams/{stream_id}/diff`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DiffQuery {
    /// Projection name or key (default `merge-patch`).
    pub projection: Option<String>,
    /// Position of the "old" state.
    pub from: u64,
    /// Position of the "new" state (default: head).
    pub to: Option<u64>,
    /// Snapshot policy for the earlier position (default `prefer`).
    pub snapshots: Option<PolicyParam>,
}

/// `GET /streams/{stream_id}/explain`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExplainQuery {
    /// Projection name or key (default `merge-patch`).
    pub projection: Option<String>,
    /// Start position (exclusive).
    pub from: u64,
    /// End position (inclusive, default: head). At most 1000 events after `from`.
    pub to: Option<u64>,
}

/// `GET /streams/{stream_id}/state` response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StateResponse {
    /// Projection key used.
    pub projection: String,
    /// The reconstructed state.
    #[schema(value_type = Object)]
    pub state: Value,
    /// The engine's `ReconstructionReport` (start point, events applied, snapshot warnings, timing).
    #[schema(value_type = Object)]
    pub report: Value,
}

/// How a snapshot was validated for a response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SnapshotValidation {
    /// `usable` or `unusable`.
    pub status: String,
    /// `projection` (digest, history binding, key and state decoding) or
    /// `storage` (digest and history binding only, for unregistered projections).
    pub checked_by: String,
    /// The engine's `SnapshotProblem` when unusable.
    #[schema(value_type = Option<Object>)]
    pub problem: Option<Value>,
}

/// A snapshot with its validation result.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SnapshotDto {
    /// Snapshot id.
    pub snapshot_id: String,
    /// Stream.
    pub stream_id: String,
    /// Last event applied to produce the state.
    pub sequence: u64,
    /// Projection key.
    pub projection: String,
    /// Digest of the event at `sequence` the snapshot is bound to.
    pub event_digest: String,
    /// Snapshot integrity digest.
    pub state_digest: String,
    /// Creation time.
    pub created_at: String,
    /// The stored state.
    #[schema(value_type = Object)]
    pub state: Value,
    /// Validation performed for this response.
    pub validation: SnapshotValidation,
}

/// `POST /streams/{stream_id}/snapshots`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSnapshotRequest {
    /// Projection name or key (default `merge-patch`).
    pub projection: Option<String>,
    /// Position to snapshot (default: head).
    pub at: Option<u64>,
}

/// `POST /streams/{stream_id}/snapshots` response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateSnapshotResponse {
    /// `false` when an identical snapshot already existed.
    pub created: bool,
    /// The snapshot.
    pub snapshot: SnapshotDto,
    /// Reconstruction report of the state that was saved.
    #[schema(value_type = Object)]
    pub report: Value,
}

/// A trusted checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckpointDto {
    /// Sequence.
    pub sequence: u64,
    /// Event digest at that sequence (64 hex chars).
    pub digest: String,
}

/// `POST /streams/{stream_id}/verify`.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyStreamRequest {
    /// Checkpoints to verify against (optional).
    #[serde(default)]
    pub checkpoints: Vec<CheckpointDto>,
}

/// A replay run without its states and trace.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ReplayRunSummary {
    /// Run id.
    pub run_id: String,
    /// Stream.
    pub stream_id: String,
    /// Projection key that recorded it.
    pub projection: String,
    /// Interval start (exclusive).
    pub from: u64,
    /// Interval end (inclusive).
    pub to: u64,
    /// Number of events.
    pub events: u64,
    /// Start-state provenance.
    #[schema(value_type = Object)]
    pub start_source: Value,
    /// Expected final-state digest.
    pub expected_state_digest: String,
    /// Recording time.
    pub created_at: String,
    /// Executions so far.
    pub execution_count: u64,
    /// Outcome label of the most recent execution.
    pub last_outcome: Option<String>,
}

/// `POST /streams/{stream_id}/replays`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateReplayRequest {
    /// Projection name or key (default `merge-patch`).
    pub projection: Option<String>,
    /// `{"kind":"initial"}`, `{"kind":"snapshot","snapshot_id":"…"}` or
    /// `{"kind":"reconstructed","at":N,"snapshots":{"kind":"prefer"}}` (default initial).
    #[schema(value_type = Option<Object>)]
    pub start: Option<ReplayStart>,
    /// End of the interval (default: head).
    pub to: Option<u64>,
}

/// `POST /replays/{run_id}/executions`.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecuteReplayRequest {
    /// Projection to execute with (default: the one the run was recorded with).
    pub projection: Option<String>,
}

/// `POST /streams/{stream_id}/experiments`.
#[derive(Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ExperimentRequest {
    /// Projection name or key (default `merge-patch`).
    pub projection: Option<String>,
    /// End of the interval (default: head).
    pub to: Option<u64>,
    /// Snapshot policy to compare against full replay (default `prefer`).
    pub snapshot_policy: Option<PolicyParam>,
}

/// One Criterion benchmark result, read from `estimates.json`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BenchmarkResult {
    /// Full Criterion id, e.g. `replay/execute_1k_from_initial`.
    pub id: String,
    /// Group.
    pub group: String,
    /// Function within the group.
    pub function: String,
    /// Median point estimate, nanoseconds.
    pub median_ns: f64,
    /// Lower bound of the median's confidence interval, nanoseconds.
    pub median_lower_ns: f64,
    /// Upper bound of the median's confidence interval, nanoseconds.
    pub median_upper_ns: f64,
    /// Mean point estimate, nanoseconds.
    pub mean_ns: f64,
    /// Confidence level of the interval (e.g. 0.95).
    pub confidence_level: f64,
    /// Throughput declared by the benchmark, if any (Criterion's raw object).
    #[schema(value_type = Option<Object>)]
    pub throughput: Option<Value>,
    /// Modification time of the result file (when the measurement was taken).
    pub measured_at: Option<String>,
}

/// `GET /benchmarks`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BenchmarksResponse {
    /// Whether a Criterion results directory is configured and present.
    pub available: bool,
    /// Results, ordered by id. Empty when none have been produced.
    pub results: Vec<BenchmarkResult>,
    /// How to produce results when none exist.
    pub note: String,
}

/// Body of `POST /api/v1/lab/experiments`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateExperimentRequest {
    /// Experiment kind: `differential_replay` (run to completion within the
    /// request) or `performance` (accepted and measured in the background).
    /// `reliability` is refused (see docs/RELIABILITY.md).
    pub kind: String,
    /// The specification. Differential replay: `stream`, `left`, `right`,
    /// and optionally `from`, `to`, `reference` (`left` or `right`) and
    /// `determinism_check`. Performance: `workload`, and optionally
    /// `input_size`, `warmup_iterations`, `measured_iterations`, `seed`.
    #[schema(value_type = Object)]
    pub spec: Value,
}

/// Body of `POST /api/v1/lab/performance/compare`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareRequest {
    /// Baseline experiment id. Omitted: the newest earlier completed
    /// performance experiment with the same configuration and a compatible
    /// environment (`status` is `no_baseline` if there is none).
    pub baseline: Option<String>,
    /// Candidate experiment ids (1–10).
    pub candidates: Vec<String>,
    /// Smallest relative change of the median that counts, in percent
    /// (0.1–100, default 10).
    pub threshold_percent: Option<f64>,
    /// Significance level of the Mann–Whitney test (0.0001–0.1, default 0.01).
    pub alpha: Option<f64>,
}

/// Query of `GET /api/v1/lab/bundles`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BundlePageQuery {
    /// Return bundles imported before this cursor (`next_before` of the previous page).
    pub before: Option<i64>,
    /// Page size, 1–500 (default 50).
    pub limit: Option<usize>,
}

/// Query of `GET /api/v1/lab/experiments/{id}/bundle`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExportQuery {
    /// Include the input event history when the store still holds it
    /// (differential replays; default `true`).
    pub history: Option<bool>,
}

/// Query of `GET /api/v1/lab/experiments`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExperimentPageQuery {
    /// Return experiments created before this one (an experiment id).
    pub before: Option<String>,
    /// Only this kind (`differential_replay`, `reliability` or `performance`).
    pub kind: Option<String>,
    /// Page size, 1–500 (default 50).
    pub limit: Option<usize>,
}

/// A page of experiments, newest first.
#[derive(Debug, Serialize, ToSchema)]
pub struct ExperimentPage {
    /// Experiment summaries.
    #[schema(value_type = Vec<Object>)]
    pub experiments: Vec<Value>,
    /// Pass as `before` to get the next page; absent on the last page.
    pub next_before: Option<String>,
}

/// Registered implementations.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImplementationList {
    /// Implementation descriptors.
    #[schema(value_type = Vec<Object>)]
    pub implementations: Vec<Value>,
}

/// One experiment with its verification history.
#[derive(Debug, Serialize, ToSchema)]
pub struct ExperimentDetail {
    /// The stored experiment.
    #[schema(value_type = Object)]
    pub experiment: Value,
    /// Verifications, oldest first.
    #[schema(value_type = Vec<Object>)]
    pub verifications: Vec<Value>,
}

/// Query of `GET /api/v1/lab/experiments/{id}/trace`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TraceQuery {
    /// First boundary to return (default: the experiment's first compared boundary).
    pub from: Option<u64>,
    /// Page size, 1–500 (default 50).
    pub limit: Option<usize>,
}

/// Result of an artifact check.
#[derive(Debug, Serialize, ToSchema)]
pub struct ArtifactVerificationResponse {
    /// What was found (`status`: `intact` or `damaged`).
    #[schema(value_type = Object)]
    pub report: Value,
    /// The stored verification record.
    #[schema(value_type = Object)]
    pub verification: Value,
}
