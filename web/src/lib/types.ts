// Response and request types of the ORBIT API (/api/v1).
// They mirror crates/orbit-api/src/dto.rs and the engine's serialized
// reports (orbit-core). Keep them in sync when the Rust types change.

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type JsonObject = { [key: string]: Json };

export type ErrorClass =
    "retryable" | "conflict" | "invalid_request" | "not_found" | "integrity" | "incompatible" | "database";

export interface ErrorBody {
    code: string;
    message: string;
    class: ErrorClass | null;
    details: Json;
}

export interface HealthResponse {
    status: string;
    api_version: string;
    engine_version: string;
    database: { reachable: boolean; schema_version: number };
}

export interface ProjectionInfo {
    id: string;
    version: number;
    key: string;
    description: string;
}

export interface CapabilitiesResponse {
    api_version: string;
    engine_version: string;
    default_projection: string;
    projections: ProjectionInfo[];
    snapshot_policies: SnapshotPolicyParam[];
    limits: {
        max_payload_bytes: number;
        max_metadata_bytes: number;
        max_body_bytes: number;
        default_page_size: number;
        max_page_size: number;
        max_explain_events: number;
        max_replay_events: number;
    };
    benchmarks_available: boolean;
}

export interface Statistics {
    streams: number;
    events: number;
    snapshots: number;
    replay_runs: number;
    replay_executions: number;
    schema_version: number;
}

export interface StreamSummary {
    stream_id: string;
    created_at: string;
    head_sequence: number;
    head_digest: string;
    last_recorded_at: string | null;
    snapshot_count: number;
    replay_run_count: number;
}

export interface StreamDetail extends StreamSummary {
    metadata: JsonObject;
}

export interface StreamPage {
    streams: StreamSummary[];
    next_after: string | null;
}

export interface StoredEvent {
    event_id: string;
    stream_id: string;
    sequence: number;
    event_type: string;
    schema_version: number;
    recorded_at: string;
    payload: Json;
    metadata: JsonObject;
    prev_digest: string;
    digest: string;
}

export interface EventPage {
    events: StoredEvent[];
    next_after: number | null;
    head_sequence: number;
}

export interface OverviewResponse {
    statistics: Statistics;
    recent_events: StoredEvent[];
    active_streams: StreamSummary[];
}

export interface AppendEventRequest {
    event_type: string;
    payload: Json;
    metadata?: JsonObject;
    schema_version?: number;
    expected_sequence?: number;
    event_id?: string;
}

export type SnapshotPolicyParam = "ignore" | "prefer" | "strict";

export type StartPoint = { kind: "initial" } | { kind: "snapshot"; snapshot_id: string; sequence: number };

export interface SnapshotWarning {
    snapshot: string;
    sequence: number;
    problem: SnapshotProblem;
}

export interface AppliedEvent {
    sequence: number;
    event_id: string;
    event_type: string;
    schema_version: number;
}

export interface ReconstructionReport {
    stream_id: string;
    projection: { id: string; version: number };
    requested: { kind: "latest" } | { kind: "at"; sequence: number };
    head_at_start: number;
    reconstructed: number;
    reconstructed_digest: string;
    start: StartPoint;
    events_applied: number;
    applied: AppliedEvent[];
    snapshot_warnings: SnapshotWarning[];
    elapsed_micros: number;
}

export interface StateResponse {
    projection: string;
    state: Json;
    report: ReconstructionReport;
}

export type Change =
    | { path: string; op: "added"; value: Json }
    | { path: string; op: "removed"; value: Json }
    | { path: string; op: "modified"; from: Json; to: Json };

export interface StateDiff {
    stream_id: string;
    from: number;
    to: number;
    changes: Change[];
    events: AppliedEvent[];
    base: ReconstructionReport;
}

export interface Explanation {
    stream_id: string;
    from: number;
    to: number;
    transitions: { event: AppliedEvent; changes: Change[] }[];
    base: ReconstructionReport;
}

export type SnapshotProblem = { kind: string } & JsonObject;

export interface Snapshot {
    snapshot_id: string;
    stream_id: string;
    sequence: number;
    projection: string;
    event_digest: string;
    state_digest: string;
    created_at: string;
    state: Json;
    validation: {
        status: "usable" | "unusable";
        checked_by: "projection" | "storage";
        problem: SnapshotProblem | null;
    };
}

export interface CreateSnapshotResponse {
    created: boolean;
    snapshot: Snapshot;
    report: ReconstructionReport;
}

export interface Checkpoint {
    sequence: number;
    digest: string;
}

export type IntegrityCategory =
    | "modified_event"
    | "broken_chain"
    | "invalid_genesis"
    | "sequence_continuity"
    | "head_inconsistency"
    | "checkpoint_mismatch"
    | "malformed_record";

export type IntegrityIssue = { kind: string } & JsonObject;

export interface IntegrityReport {
    stream_id: string;
    events_checked: number;
    head: { sequence: number; digest: string };
    issues: IntegrityIssue[];
    total_issues: number;
    affected_events: { sequence: number; event_id: string }[];
}

export interface StoreIntegrityReport {
    valid: boolean;
    schema_version: number;
    sqlite_integrity: string[];
    foreign_key_violations: number;
    missing_safeguards: string[];
    streams: IntegrityReport[];
    snapshots_checked: number;
    snapshot_issues: { snapshot: string; stream: string; sequence: number; problem: SnapshotProblem }[];
    replay_runs_checked: number;
    replay_run_issues: { run: string; problem: string }[];
    elapsed_micros: number;
}

export type ReplayStart =
    | { kind: "initial" }
    | { kind: "snapshot"; snapshot_id: string }
    | { kind: "reconstructed"; at: number; snapshots: { kind: SnapshotPolicyParam } };

export interface ReplayRunSummary {
    run_id: string;
    stream_id: string;
    projection: string;
    from: number;
    to: number;
    events: number;
    start_source: { kind: string; snapshot_id?: string; via_snapshot?: string | null };
    expected_state_digest: string;
    created_at: string;
    execution_count: number;
    last_outcome: string | null;
}

export interface ReplayRun {
    id: string;
    stream_id: string;
    projection: { id: string; version: number };
    from: number;
    to: number;
    from_event_digest: string;
    to_event_digest: string;
    start_source: { kind: string; snapshot_id?: string; via_snapshot?: string | null };
    start_state: Json;
    start_state_digest: string;
    expected_state: Json;
    expected_state_digest: string;
    trace: string[];
    created_at: string;
    record_digest: string;
}

export interface DivergencePoint {
    sequence: number;
    event_id: string;
    event_type: string;
    expected_state_digest: string;
    actual_state_digest: string;
    matched_events: number;
}

export type ReplayOutcome =
    | { status: "exact_match" }
    | { status: "reconverged"; first_divergence: DivergencePoint }
    | { status: "diverged"; first_divergence: DivergencePoint; changes: Change[] }
    | { status: "projection_failed"; failure: JsonObject; first_divergence: DivergencePoint | null }
    | { status: "unavailable"; reason: { kind: string } & JsonObject }
    | { status: "invalid_input"; reason: string }
    | { status: "integrity_failure"; violations: ({ kind: string } & JsonObject)[] };

export interface ReplayExecution {
    id: string;
    run_id: string;
    executed_at: string;
    projection: { id: string; version: number };
    stream_id: string | null;
    interval: [number, number] | null;
    outcome: ReplayOutcome;
    expected_state_digest: string | null;
    actual_state_digest: string | null;
    actual_position: number | null;
    events_applied: number;
    warnings: string[];
    elapsed_micros: number;
}

export interface StoredExecution {
    id: string;
    run_id: string;
    executed_at: string;
    outcome: string;
    actual_state_digest: string | null;
    report: ReplayExecution;
}

export interface ReplayInspection {
    run: ReplayRun;
    executions: StoredExecution[];
}

export interface ExperimentReport {
    experiment_id: string | null;
    run_id: string | null;
    stream_id: string;
    interval: [number, number];
    projection: { id: string; version: number };
    snapshot_policy: { kind: string };
    integrity: { valid: boolean; events_checked: number; total_issues: number; categories: string[] };
    equivalence: {
        equivalent: boolean;
        assisted_start: StartPoint;
        assisted_events_applied: number;
        snapshot_warnings: number;
        full_state_digest: string;
        assisted_state_digest: string;
        differences: number;
    } | null;
    replay_outcome: string | null;
    timings: { verify_micros: number; equivalence_micros: number; record_micros: number; replay_micros: number };
    notes: string[];
}

export interface BenchmarkResult {
    id: string;
    group: string;
    function: string;
    median_ns: number;
    median_lower_ns: number;
    median_upper_ns: number;
    mean_ns: number;
    confidence_level: number;
    throughput: JsonObject | null;
    measured_at: string | null;
}

export interface BenchmarksResponse {
    available: boolean;
    results: BenchmarkResult[];
    note: string;
}

// ---- Lab (orbit-lab): experiment registry and differential replay ----

export type LabSide = "left" | "right";
export type ExperimentStatus = "created" | "running" | "completed" | "failed";
export type LabOutcome = "equivalent" | "reconverged" | "diverged" | "implementation_failed" | "nondeterministic";
export type Reproducibility = "reproducible" | "unverified" | "not_reproducible";

export interface ImplementationDescriptor {
    descriptor_version: number;
    id: string;
    name: string;
    projection: { id: string; version: number };
    adapter: string;
    implementation_type: string;
    config: Json;
    config_digest: string;
    determinism: "pure" | "uncontrolled";
    fingerprint: string;
}

export interface DifferentialSpec {
    stream: string;
    left: string;
    right: string;
    from?: number;
    to?: number | null;
    reference?: LabSide | null;
    determinism_check?: boolean;
}

export interface ExperimentSummary {
    id: string;
    kind: string;
    status: ExperimentStatus;
    created_at: string;
    stream: string;
    left: string;
    right: string;
    outcome: LabOutcome | null;
    reproducibility: Reproducibility | null;
}

export interface InputRange {
    stream: string;
    from: number;
    to: number;
    from_event_digest: string;
    to_event_digest: string;
    history_fingerprint: string;
}

export interface StateSummary {
    digest: string;
    canonical_bytes: number;
}

export interface LabEventRef {
    sequence: number;
    event_id: string;
    event_type: string;
    schema_version: number;
}

export interface LabComparison {
    boundary: number;
    event: LabEventRef | null;
    left: StateSummary;
    right: StateSummary;
    equal: boolean;
    changes: Change[];
    changes_total: number;
}

export interface LabSideFailure {
    side: LabSide;
    implementation: string;
    boundary: number;
    event: LabEventRef | null;
    failure: { kind: string; message?: string; error?: { kind: string; message: string } };
}

export interface DifferentialResult {
    outcome: LabOutcome;
    reference: LabSide | null;
    left: { implementation: string; fingerprint: string };
    right: { implementation: string; fingerprint: string };
    input: InputRange;
    events_processed: number;
    events_fed_digest: string;
    boundaries_compared: number;
    boundaries_differing: number;
    first_divergence: (LabComparison & { boundaries_matched_before: number }) | null;
    divergent_segments: { first: number; last: number }[];
    divergent_segments_total: number;
    final_comparison: LabComparison | null;
    failures: LabSideFailure[];
    determinism: {
        checked: boolean;
        consistent: boolean | null;
        inconsistency: { side: LabSide; boundary: number } | null;
        declared_uncontrolled: LabSide[];
    };
    reproducibility: { status: Reproducibility; reasons: string[] };
}

export interface LabArtifact {
    role: string;
    digest: string;
    media_type: string;
    size: number;
}

export interface ExperimentRecord {
    id: string;
    kind: string;
    spec_version: number;
    spec: DifferentialSpec;
    spec_digest: string;
    implementations: { side: LabSide; descriptor: ImplementationDescriptor }[];
    build: { orbit_version: string; git_commit: string | null };
    status: ExperimentStatus;
    created_at: string;
    started_at: string | null;
    finished_at: string | null;
    elapsed_micros: number | null;
    input: InputRange | null;
    outcome: LabOutcome | null;
    result: DifferentialResult | null;
    result_digest: string | null;
    reproducibility: Reproducibility | null;
    failure: { kind: string; message: string; details: Json } | null;
    artifacts: LabArtifact[];
}

export interface VerificationRecord {
    id: string;
    experiment_id: string;
    kind: "artifacts" | "reproduction";
    status: string;
    detail: Json;
    verified_at: string;
}

export interface ExperimentDetail {
    experiment: ExperimentRecord;
    verifications: VerificationRecord[];
}

export interface ArtifactVerificationResponse {
    report: {
        status: "intact" | "damaged";
        spec_digest_ok: boolean;
        result_digest_ok: boolean | null;
        traces_consistent: boolean | null;
        artifacts: (LabArtifact & { state: "intact" | "missing" | "corrupted" })[];
    };
    verification: VerificationRecord;
}
