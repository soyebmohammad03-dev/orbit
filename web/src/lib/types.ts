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
    kind: "differential_replay" | "reliability" | string;
    status: ExperimentStatus;
    created_at: string;
    /** Differential replays only. */
    stream: string | null;
    left: string | null;
    right: string | null;
    /** A differential outcome or a reliability verdict. */
    outcome: string | null;
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
    /** Sprint 8 results only. */
    digest?: string;
    recorded_at?: string;
    metadata_keys?: string[];
}

export interface ChangeSummary {
    added: number;
    removed: number;
    modified: number;
    type_changed: number;
}

export interface LabComparison {
    boundary: number;
    event: LabEventRef | null;
    left: StateSummary;
    right: StateSummary;
    equal: boolean;
    changes: Change[];
    changes_total: number;
    /** Sprint 8 results only. */
    change_summary?: ChangeSummary;
}

export interface LabSideFailure {
    side: LabSide;
    implementation: string;
    boundary: number;
    event: LabEventRef | null;
    failure: { kind: string; message?: string; error?: { kind: string; message: string } };
}

export interface DivergenceInterval {
    index: number;
    first: number;
    last: number;
    boundaries: number;
    end: "converged" | "open_at_end" | "stopped_by_failure";
    converged_at: number | null;
    convergence_event: LabEventRef | null;
    entry: LabComparison | null;
    exit: LabComparison | null;
}

export interface Timeline {
    first_boundary: number;
    last_boundary: number | null;
    intervals: DivergenceInterval[];
    intervals_total: number;
    intervals_truncated: boolean;
    convergences: number;
    final_equal: boolean | null;
}

export interface DifferentialResult {
    /** Absent in Sprint 7 results (version 1). */
    result_version?: number;
    semantics?: {
        version: number;
        boundary: string;
        state_comparison: string;
        structural_diff: string;
        max_events: number;
    };
    outcome: LabOutcome;
    reference: LabSide | null;
    perspective?: {
        reference: LabSide | null;
        expected: LabSide | null;
        observed: LabSide | null;
        changes_orientation: string;
    };
    left: { implementation: string; fingerprint: string };
    right: { implementation: string; fingerprint: string };
    input: InputRange;
    events_processed: number;
    events_fed_digest: string;
    boundaries_compared: number;
    boundaries_differing: number;
    first_divergence: (LabComparison & { boundaries_matched_before: number }) | null;
    /** Sprint 8 results. */
    timeline?: Timeline;
    /** Sprint 7 results. */
    divergent_segments?: { first: number; last: number }[];
    divergent_segments_total?: number;
    final_comparison: LabComparison | null;
    failures: LabSideFailure[];
    determinism: {
        checked: boolean;
        consistent: boolean | null;
        inconsistency: { side: LabSide; boundary: number } | null;
        declared_uncontrolled: LabSide[];
    };
    reproducibility: { status: Reproducibility; reasons: string[] };
    diagnosis?: { evidence: string[]; not_established: string[] };
}

export interface IdentityAssessment {
    sides: {
        side: string;
        implementation: string;
        projection: { id: string; version: number } | null;
        fingerprint: string | null;
        config_digest: string | null;
    }[];
    build: BuildInfo;
    completeness: "declared" | "source_revision" | "executable";
    gaps: string[];
}

export interface BuildInfo {
    orbit_version: string;
    git_commit: string | null;
    executable_digest?: string | null;
}

export interface TracePage {
    experiment_id: string;
    first_boundary: number;
    last_boundary: number | null;
    boundaries_total: number;
    entries: { boundary: number; left: string; right: string; equal: boolean }[];
    next_from: number | null;
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
    /** A DifferentialSpec, or for reliability experiments `{scenario, scenario_version, parameters}`. */
    spec: DifferentialSpec & { scenario?: string; scenario_version?: number; parameters?: Record<string, number> };
    spec_digest: string;
    implementations: { side: LabSide; descriptor: ImplementationDescriptor }[];
    build: BuildInfo;
    status: ExperimentStatus;
    created_at: string;
    started_at: string | null;
    finished_at: string | null;
    elapsed_micros: number | null;
    input: InputRange | null;
    outcome: string | null;
    /** For reliability experiments, a ReliabilityResult (see `kind`). */
    result: DifferentialResult | null;
    result_digest: string | null;
    reproducibility: Reproducibility | null;
    failure: { kind: string; message: string; details: Json } | null;
    artifacts: LabArtifact[];
    identity: IdentityAssessment;
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

// ---- Reliability scenarios (orbit-lab, run only by the orbit-reliability tool) ----

export interface ScenarioInfo {
    id: string;
    version: number;
    title: string;
    summary: string;
    simulates: string[];
    does_not_simulate: string[];
    invariants: string[];
    parameters: { name: string; description: string; default: number; min: number; max: number }[];
}

/** Whether this server can launch reliability runs (a separately configured runner). */
export type ReliabilityRunner =
    { enabled: false; reason: string } | { enabled: true; timeout_secs: number; busy: boolean };

export interface ReliabilityCatalogResponse {
    scenarios: ScenarioInfo[];
    runner: ReliabilityRunner;
    execution: string;
}

export interface InvariantCheck {
    name: string;
    expected: string;
    observed: string;
    held: boolean;
}

export type FailureMode =
    | "simulated_error"
    | "process_abort"
    | "process_killed"
    | "process_paused"
    | "deliberate_corruption"
    | "concurrent_processes";

export interface ReliabilityCase {
    id: string;
    fault_point: string | null;
    mode: FailureMode;
    real_process: boolean;
    fault_observed: boolean;
    fault_evidence: string;
    invariants: InvariantCheck[];
    recovery: Json;
    integrity: Json;
    verdict: "invariant_preserved" | "invariant_violated" | "inconclusive";
}

export interface ReliabilityResult {
    result_version: number;
    scenario: { id: string; version: number; title: string };
    parameters: Record<string, number>;
    history: { events: number; requests_digest: string };
    cases: ReliabilityCase[];
    summary: {
        cases: number;
        invariant_preserved: number;
        invariant_violated: number;
        inconclusive: number;
        real_process_cases: number;
    };
    verdict: "invariants_preserved" | "invariant_violated" | "inconclusive";
    simulates: string[];
    does_not_simulate: string[];
    limitations: string[];
}

// ---------------------------------------------------------------- Phase 9

/** Labels of every reproduction attempt (`VerificationRecord.status` when `kind` is `reproduction`). */
export type ReproductionStatus =
    | "reproduced"
    | "differs"
    | "history_changed"
    | "configuration_mismatch"
    | "implementation_unavailable"
    | "artifact_missing"
    | "history_unavailable"
    | "not_reproducible"
    | "inconclusive"
    | "unsupported";

export interface Bound {
    default: number;
    min: number;
    max: number;
}

export interface WorkloadInfo {
    id: string;
    version: number;
    title: string;
    iteration: string;
    input_size_meaning: string;
    input_unit: string;
    input_size: Bound;
}

export interface RegressionPolicy {
    threshold_percent: number;
    alpha: number;
}

export interface WorkloadCatalog {
    workloads: WorkloadInfo[];
    warmup_iterations: Bound;
    measured_iterations: Bound;
    min_measurement_samples: number;
    default_policy: RegressionPolicy;
    build_profile: "debug" | "release";
    measuring: boolean;
}

export interface PerformanceSpec {
    workload: string;
    workload_version?: number | null;
    input_size?: number | null;
    warmup_iterations?: number | null;
    measured_iterations?: number | null;
    seed?: number | null;
}

export interface Interval {
    lower: number;
    upper: number;
    level: number;
    coverage: number;
    method: string;
}

export interface StatsSummary {
    count: number;
    mean: number;
    median: number;
    std_dev: number | null;
    mad: number;
    coefficient_of_variation: number | null;
    min: number;
    max: number;
    percentiles: { p: number; value: number }[];
    mean_ci: Interval | null;
    median_ci: Interval | null;
    notes: string[];
}

export interface PerformanceEnvironment {
    os: string;
    arch: string;
    available_parallelism: number | null;
    build_profile: string;
    build: BuildInfo;
}

export interface PerformanceResult {
    result_version: number;
    workload: { id: string; version: number; title: string; iteration: string; input_unit: string };
    configuration: PerformanceSpec;
    configuration_digest: string;
    environment: PerformanceEnvironment;
    evidence_level: "smoke" | "measurement";
    evidence_notes: string[];
    unit: string;
    warmup_samples_ns: number[];
    samples_ns: number[];
    summary: StatsSummary;
    throughput_per_second_at_median: number | null;
    limitations: string[];
}

export type ComparisonVerdict =
    | "incompatible"
    | "insufficient_samples"
    | "within_threshold"
    | "suspected_regression"
    | "suspected_improvement"
    | "regression"
    | "improvement";

export interface Comparison {
    verdict: ComparisonVerdict;
    compatibility: { incompatible: string[]; warnings: string[] };
    baseline_median_ns: number | null;
    candidate_median_ns: number | null;
    absolute_change_ns: number | null;
    relative_change: number | null;
    median_intervals: [Interval | null, Interval | null] | null;
    median_intervals_overlap: boolean | null;
    test: { u: number; z: number; p_value: number; method: string } | null;
    policy: RegressionPolicy;
    explanation: string;
}

export interface ComparedExperiment {
    experiment_id: string;
    created_at: string;
    configuration_digest: string;
    evidence_level: string;
    samples: number;
    build: BuildInfo;
    environment: PerformanceEnvironment;
}

export type CompareResponse =
    | { status: "no_baseline"; reason: string; baseline: null; comparisons: []; policy: RegressionPolicy }
    | {
          status: "compared";
          baseline_selection: "given" | "newest_compatible_before_candidate";
          baseline: ComparedExperiment;
          comparisons: { candidate: ComparedExperiment; comparison: Comparison }[];
          policy: RegressionPolicy;
      };

export interface BundleIssue {
    code: string;
    message: string;
}

export interface ValidationReport {
    valid: boolean;
    bundle_digest: string | null;
    format_version: number | null;
    experiment_id: string | null;
    kind: string | null;
    status: ExperimentStatus | null;
    outcome: string | null;
    errors: BundleIssue[];
    passed: string[];
    integrity: "verified" | "failed";
    scientific_correctness: "not_verified";
    history: "included_and_verified" | "not_included" | "invalid";
    independently_replayable: boolean;
    replayability: string;
    limitations: string[];
}

export interface ImportedBundleSummary {
    bundle_digest: string;
    experiment_id: string;
    kind: string;
    status: string;
    outcome: string | null;
    imported_at: string;
    sequence: number;
}

export interface ImportedBundle extends ImportedBundleSummary {
    format_version: number;
    document: {
        manifest: {
            experiment_id: string;
            kind: string;
            exporter: BuildInfo;
            document_digest: string;
            entries: { name: string; media_type: string; size: number; sha256: string }[];
        };
        document: {
            experiment: ExperimentRecord;
            verifications: VerificationRecord[];
            history: { included: boolean; entry: string | null; events: number | null; reason: string | null };
            provenance: JsonObject;
            limitations: string[];
        };
    };
    validation: ValidationReport;
}

export interface BundleVerification {
    id: string;
    bundle_digest: string;
    kind: "reproduction";
    status: ReproductionStatus;
    detail: Json;
    verified_at: string;
}

export interface ImportOutcome {
    bundle: ImportedBundle;
    newly_imported: boolean;
    local_experiment: { exists: boolean; same_spec_digest?: boolean; same_result_digest?: boolean };
}
