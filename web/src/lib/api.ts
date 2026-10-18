import { API_BASE_URL } from "./config";
import type {
    AppendEventRequest,
    ArtifactVerificationResponse,
    DifferentialSpec,
    ExperimentDetail,
    ExperimentRecord,
    ExperimentSummary,
    ImplementationDescriptor,
    VerificationRecord,
    BenchmarksResponse,
    CapabilitiesResponse,
    Checkpoint,
    CreateSnapshotResponse,
    ErrorBody,
    EventPage,
    ExperimentReport,
    Explanation,
    HealthResponse,
    IntegrityReport,
    JsonObject,
    OverviewResponse,
    ReplayExecution,
    ReplayInspection,
    ReplayRun,
    ReplayRunSummary,
    ReplayStart,
    Snapshot,
    SnapshotPolicyParam,
    StateDiff,
    StateResponse,
    StoreIntegrityReport,
    StoredEvent,
    StreamDetail,
    StreamPage,
} from "./types";

/** An error returned by the API (structured envelope) or a transport failure. */
export class ApiError extends Error {
    readonly status: number;
    readonly code: string;
    readonly details: ErrorBody["details"];
    readonly errorClass: ErrorBody["class"];

    constructor(status: number, body: ErrorBody) {
        super(body.message);
        this.name = "ApiError";
        this.status = status;
        this.code = body.code;
        this.details = body.details;
        this.errorClass = body.class;
    }

    /** `true` when the API could not be reached at all. */
    get isNetworkError(): boolean {
        return this.status === 0;
    }
}

/** Measured server-side handling time, from the `X-Orbit-Elapsed-Micros` header. */
export interface Timed<T> {
    data: T;
    serverMicros: number | null;
}

type Fetch = typeof fetch;

export interface ClientOptions {
    baseUrl?: string;
    fetch?: Fetch;
}

function query(params: Record<string, string | number | boolean | undefined | null>): string {
    const entries = Object.entries(params).filter(([, v]) => v !== undefined && v !== null && v !== "");
    if (entries.length === 0) return "";
    return "?" + new URLSearchParams(entries.map(([k, v]) => [k, String(v)])).toString();
}

const seg = encodeURIComponent;

/** Typed client for every `/api/v1` endpoint. */
export function createClient(options: ClientOptions = {}) {
    const baseUrl = (options.baseUrl ?? API_BASE_URL).replace(/\/+$/, "");
    const doFetch: Fetch = options.fetch ?? ((...args) => fetch(...args));

    async function timed<T>(method: "GET" | "POST", path: string, body?: unknown): Promise<Timed<T>> {
        let response: Response;
        try {
            response = await doFetch(baseUrl + path, {
                method,
                headers: body === undefined ? undefined : { "content-type": "application/json" },
                body: body === undefined ? undefined : JSON.stringify(body),
                cache: "no-store",
            });
        } catch (cause) {
            throw new ApiError(0, {
                code: "network_error",
                message: `Cannot reach the ORBIT API at ${baseUrl}. Is orbit-api running? (${cause instanceof Error ? cause.message : String(cause)})`,
                class: null,
                details: null,
            });
        }
        const text = await response.text();
        let parsed: unknown = null;
        if (text) {
            try {
                parsed = JSON.parse(text);
            } catch {
                parsed = null;
            }
        }
        if (!response.ok) {
            const envelope = parsed as { error?: ErrorBody } | null;
            throw new ApiError(
                response.status,
                envelope?.error ?? {
                    code: "http_error",
                    message: `HTTP ${response.status}${text ? `: ${text.slice(0, 200)}` : ""}`,
                    class: null,
                    details: null,
                },
            );
        }
        const header = response.headers.get("x-orbit-elapsed-micros");
        const serverMicros = header !== null && /^\d+$/.test(header) ? Number(header) : null;
        return { data: parsed as T, serverMicros };
    }

    const get = async <T>(path: string) => (await timed<T>("GET", path)).data;
    const post = async <T>(path: string, body: unknown = {}) => (await timed<T>("POST", path, body)).data;

    return {
        baseUrl,
        timed,
        health: () => get<HealthResponse>("/health"),
        capabilities: () => get<CapabilitiesResponse>("/capabilities"),
        overview: () => get<OverviewResponse>("/overview"),
        benchmarks: () => get<BenchmarksResponse>("/benchmarks"),

        listStreams: (p: { after?: string; limit?: number } = {}) => get<StreamPage>(`/streams${query(p)}`),
        createStream: (stream_id: string, metadata?: JsonObject) =>
            post<StreamDetail>("/streams", metadata ? { stream_id, metadata } : { stream_id }),
        getStream: (id: string) => get<StreamDetail>(`/streams/${seg(id)}`),
        listEvents: (id: string, p: { after?: number; limit?: number } = {}) =>
            get<EventPage>(`/streams/${seg(id)}/events${query(p)}`),
        appendEvent: (id: string, body: AppendEventRequest) => post<StoredEvent>(`/streams/${seg(id)}/events`, body),
        getEvent: (id: string, sequence: number) => get<StoredEvent>(`/streams/${seg(id)}/events/${sequence}`),

        getState: (
            id: string,
            p: { projection?: string; at?: number; snapshots?: SnapshotPolicyParam; record_events?: boolean } = {},
        ) => timed<StateResponse>("GET", `/streams/${seg(id)}/state${query(p)}`),
        getDiff: (id: string, p: { projection?: string; from: number; to?: number; snapshots?: SnapshotPolicyParam }) =>
            get<StateDiff>(`/streams/${seg(id)}/diff${query(p)}`),
        getExplain: (id: string, p: { projection?: string; from: number; to?: number }) =>
            get<Explanation>(`/streams/${seg(id)}/explain${query(p)}`),

        listSnapshots: (id: string) => get<Snapshot[]>(`/streams/${seg(id)}/snapshots`),
        createSnapshot: (id: string, body: { projection?: string; at?: number }) =>
            post<CreateSnapshotResponse>(`/streams/${seg(id)}/snapshots`, body),
        getSnapshot: (snapshotId: string) => get<Snapshot>(`/snapshots/${seg(snapshotId)}`),

        checkpoint: (id: string) => get<Checkpoint>(`/streams/${seg(id)}/checkpoint`),
        verifyStream: (id: string, checkpoints: Checkpoint[] = []) =>
            post<IntegrityReport>(`/streams/${seg(id)}/verify`, { checkpoints }),
        verifyStore: () => post<StoreIntegrityReport>("/verify"),

        listReplays: (id: string) => get<ReplayRunSummary[]>(`/streams/${seg(id)}/replays`),
        createReplay: (id: string, body: { projection?: string; start?: ReplayStart; to?: number }) =>
            post<ReplayRun>(`/streams/${seg(id)}/replays`, body),
        getReplay: (runId: string) => get<ReplayInspection>(`/replays/${seg(runId)}`),
        executeReplay: (runId: string, projection?: string) =>
            post<ReplayExecution>(`/replays/${seg(runId)}/executions`, projection ? { projection } : {}),
        runExperiment: (
            id: string,
            body: { projection?: string; to?: number; snapshot_policy?: SnapshotPolicyParam },
        ) => post<ExperimentReport>(`/streams/${seg(id)}/experiments`, body),

        labImplementations: () => get<{ implementations: ImplementationDescriptor[] }>("/lab/implementations"),
        listLabExperiments: (p: { before?: string; limit?: number } = {}) =>
            get<{ experiments: ExperimentSummary[]; next_before: string | null }>(`/lab/experiments${query(p)}`),
        createDifferential: (spec: DifferentialSpec) =>
            post<ExperimentRecord>("/lab/experiments", { kind: "differential_replay", spec }),
        getLabExperiment: (id: string) => get<ExperimentDetail>(`/lab/experiments/${seg(id)}`),
        verifyLabExperiment: (id: string) => post<ArtifactVerificationResponse>(`/lab/experiments/${seg(id)}/verify`),
        reproduceLabExperiment: (id: string) => post<VerificationRecord>(`/lab/experiments/${seg(id)}/reproduce`),
    };
}

export type OrbitClient = ReturnType<typeof createClient>;

/** The client used by the application. */
export const api: OrbitClient = createClient();
