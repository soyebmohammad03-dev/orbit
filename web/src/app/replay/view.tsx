"use client";

import { useState } from "react";

import { DiffView } from "@/components/diff-view";
import { JsonView } from "@/components/json-view";
import { ProjectionPicker, StreamPicker, useQueryParam } from "@/components/stream-picker";
import {
    Badge,
    Button,
    Card,
    Digest,
    EmptyState,
    ErrorState,
    Field,
    Input,
    KeyValue,
    Loading,
    Mono,
    PageHeader,
    Select,
    Table,
    Td,
    Th,
} from "@/components/ui";
import { api } from "@/lib/api";
import { OUTCOME_LABELS, formatMicros, formatTimestamp, outcomeTone, shortDigest } from "@/lib/format";
import type {
    ExperimentReport,
    Json,
    ReplayExecution,
    ReplayInspection,
    ReplayStart,
    SnapshotPolicyParam,
} from "@/lib/types";
import { useAction, useApi } from "@/lib/use-api";

export function OutcomeBadge({ status }: { status: string | null | undefined }) {
    if (!status) return <Badge>not executed</Badge>;
    return <Badge tone={outcomeTone(status)}>{OUTCOME_LABELS[status] ?? status}</Badge>;
}

export function ReplayView() {
    const [stream, setStream] = useQueryParam("stream");
    const [runId, setRun] = useQueryParam("run");
    const [refresh, setRefresh] = useState(0);
    const runs = useApi(() => api.listReplays(stream ?? ""), stream ? `runs:${stream}:${refresh}` : null);

    return (
        <>
            <PageHeader
                title="Replay Lab"
                description="Recorded, self-contained replay runs re-executed with the current projection code. Every execution is persisted and classified; divergence names the first event after which the state digest differed."
                actions={<StreamPicker value={stream} onChange={(id) => setStream(id, ["run"])} />}
            />
            {!stream ? (
                <EmptyState title="Select a stream to record or inspect replay runs." />
            ) : (
                <div className="grid grid-cols-1 gap-6 xl:grid-cols-[minmax(0,1fr)_24rem]">
                    <div className="flex flex-col gap-6">
                        <Card title="Replay runs" padded={false}>
                            {runs.loading && !runs.data ? (
                                <div className="px-4">
                                    <Loading />
                                </div>
                            ) : null}
                            {runs.error ? (
                                <div className="p-4">
                                    <ErrorState error={runs.error} onRetry={runs.reload} />
                                </div>
                            ) : null}
                            {runs.data && runs.data.length === 0 ? (
                                <div className="p-4">
                                    <EmptyState title="No replay runs recorded for this stream yet.">
                                        Record one, or run an experiment, with the forms.
                                    </EmptyState>
                                </div>
                            ) : null}
                            {runs.data && runs.data.length > 0 ? (
                                <Table label="Replay runs">
                                    <thead>
                                        <tr>
                                            <Th>Run</Th>
                                            <Th>Interval</Th>
                                            <Th>Projection</Th>
                                            <Th>Start</Th>
                                            <Th className="text-right">Executions</Th>
                                            <Th>Last outcome</Th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {runs.data.map((r) => (
                                            <tr
                                                key={r.run_id}
                                                className={r.run_id === runId ? "bg-surface-2" : undefined}
                                            >
                                                <Td>
                                                    <button
                                                        className="font-mono text-xs text-accent hover:underline"
                                                        onClick={() => setRun(r.run_id)}
                                                    >
                                                        {shortDigest(r.run_id, 8, 4)}
                                                    </button>
                                                </Td>
                                                <Td className="font-mono text-xs">
                                                    ({r.from}, {r.to}] · {r.events} events
                                                </Td>
                                                <Td className="font-mono text-xs">{r.projection}</Td>
                                                <Td className="text-xs text-muted">{r.start_source.kind}</Td>
                                                <Td className="text-right font-mono">{r.execution_count}</Td>
                                                <Td>
                                                    <OutcomeBadge status={r.last_outcome} />
                                                </Td>
                                            </tr>
                                        ))}
                                    </tbody>
                                </Table>
                            ) : null}
                        </Card>
                        {runId ? <RunDetail runId={runId} onExecuted={() => setRefresh((n) => n + 1)} /> : null}
                    </div>
                    <div className="flex flex-col gap-6">
                        <RecordRun
                            stream={stream}
                            onRecorded={(id) => {
                                setRefresh((n) => n + 1);
                                setRun(id);
                            }}
                        />
                        <Experiment
                            stream={stream}
                            onDone={(id) => {
                                setRefresh((n) => n + 1);
                                if (id) setRun(id);
                            }}
                        />
                    </div>
                </div>
            )}
        </>
    );
}

function RecordRun({ stream, onRecorded }: { stream: string; onRecorded: (runId: string) => void }) {
    const [projection, setProjection] = useState("merge-patch");
    const [startKind, setStartKind] = useState<"initial" | "snapshot" | "reconstructed">("initial");
    const [snapshotId, setSnapshotId] = useState("");
    const [at, setAt] = useState("");
    const [to, setTo] = useState("");
    const snapshots = useApi(() => api.listSnapshots(stream), startKind === "snapshot" ? `snaps:${stream}` : null);
    const record = useAction(() => {
        const start: ReplayStart =
            startKind === "initial"
                ? { kind: "initial" }
                : startKind === "snapshot"
                  ? { kind: "snapshot", snapshot_id: snapshotId }
                  : { kind: "reconstructed", at: Number(at || 0), snapshots: { kind: "prefer" } };
        return api.createReplay(stream, { projection, start, to: to ? Number(to) : undefined });
    });
    return (
        <Card title="Record a replay run">
            <form
                className="flex flex-col gap-3"
                onSubmit={async (e) => {
                    e.preventDefault();
                    const run = await record.run();
                    if (run) onRecorded(run.id);
                }}
            >
                <ProjectionPicker value={projection} onChange={setProjection} id="record-projection" />
                <Field label="Start from" htmlFor="record-start">
                    <Select
                        id="record-start"
                        value={startKind}
                        onChange={(e) => setStartKind(e.target.value as typeof startKind)}
                    >
                        <option value="initial">Initial state (sequence 0)</option>
                        <option value="snapshot">A validated snapshot</option>
                        <option value="reconstructed">State reconstructed at a sequence</option>
                    </Select>
                </Field>
                {startKind === "snapshot" ? (
                    <Field label="Snapshot" htmlFor="record-snapshot">
                        <Select
                            id="record-snapshot"
                            value={snapshotId}
                            onChange={(e) => setSnapshotId(e.target.value)}
                            required
                        >
                            <option value="" disabled>
                                {snapshots.data?.length === 0 ? "No snapshots" : "Select a snapshot"}
                            </option>
                            {(snapshots.data ?? []).map((s) => (
                                <option key={s.snapshot_id} value={s.snapshot_id}>
                                    #{s.sequence} {s.projection} ({s.validation.status})
                                </option>
                            ))}
                        </Select>
                    </Field>
                ) : null}
                {startKind === "reconstructed" ? (
                    <Field label="Start sequence" htmlFor="record-at">
                        <Input
                            id="record-at"
                            type="number"
                            min={0}
                            value={at}
                            onChange={(e) => setAt(e.target.value)}
                            required
                        />
                    </Field>
                ) : null}
                <Field label="End sequence (default: head)" htmlFor="record-to">
                    <Input id="record-to" type="number" min={0} value={to} onChange={(e) => setTo(e.target.value)} />
                </Field>
                {record.error ? <ErrorState title="Run not recorded" error={record.error} /> : null}
                <div>
                    <Button type="submit" variant="primary" busy={record.pending}>
                        Record run
                    </Button>
                </div>
            </form>
        </Card>
    );
}

function Experiment({ stream, onDone }: { stream: string; onDone: (runId: string | null) => void }) {
    const [projection, setProjection] = useState("merge-patch");
    const [policy, setPolicy] = useState<SnapshotPolicyParam>("prefer");
    const [report, setReport] = useState<ExperimentReport | null>(null);
    const run = useAction(() => api.runExperiment(stream, { projection, snapshot_policy: policy }));
    return (
        <Card title="Run an experiment">
            <form
                className="flex flex-col gap-3"
                onSubmit={async (e) => {
                    e.preventDefault();
                    const r = await run.run();
                    if (r) {
                        setReport(r);
                        onDone(r.run_id);
                    }
                }}
            >
                <p className="text-xs text-muted">
                    Verifies the stream, compares snapshot-assisted reconstruction with full replay, then records and
                    executes a replay of the whole stream.
                </p>
                <ProjectionPicker value={projection} onChange={setProjection} id="experiment-projection" />
                <Field label="Snapshot policy" htmlFor="experiment-policy">
                    <Select
                        id="experiment-policy"
                        value={policy}
                        onChange={(e) => setPolicy(e.target.value as SnapshotPolicyParam)}
                    >
                        <option value="prefer">prefer</option>
                        <option value="strict">strict</option>
                        <option value="ignore">ignore</option>
                    </Select>
                </Field>
                {run.error ? <ErrorState title="Experiment failed" error={run.error} /> : null}
                <div>
                    <Button type="submit" busy={run.pending}>
                        Run experiment
                    </Button>
                </div>
            </form>
            {report ? (
                <div className="mt-4 flex flex-col gap-3 border-t border-border pt-4">
                    <KeyValue
                        items={[
                            [
                                "Integrity",
                                report.integrity.valid ? (
                                    <Badge key="i" tone="ok">
                                        valid ({report.integrity.events_checked} events)
                                    </Badge>
                                ) : (
                                    <Badge key="i" tone="fail">
                                        {report.integrity.total_issues} issues
                                    </Badge>
                                ),
                            ],
                            [
                                "Snapshot vs full",
                                report.equivalence ? (
                                    report.equivalence.equivalent ? (
                                        <Badge key="e" tone="ok">
                                            equivalent
                                        </Badge>
                                    ) : (
                                        <Badge key="e" tone="fail">
                                            {report.equivalence.differences} differences
                                        </Badge>
                                    )
                                ) : (
                                    "skipped"
                                ),
                            ],
                            [
                                "Assisted start",
                                report.equivalence
                                    ? report.equivalence.assisted_start.kind === "snapshot"
                                        ? `snapshot #${report.equivalence.assisted_start.sequence}`
                                        : "initial"
                                    : "—",
                            ],
                            ["Replay", <OutcomeBadge key="r" status={report.replay_outcome} />],
                            [
                                "Timings (measured)",
                                `verify ${formatMicros(report.timings.verify_micros)} · compare ${formatMicros(report.timings.equivalence_micros)} · record ${formatMicros(report.timings.record_micros)} · replay ${formatMicros(report.timings.replay_micros)}`,
                            ],
                        ]}
                    />
                    {report.notes.map((n) => (
                        <p key={n} className="text-sm text-warn">
                            {n}
                        </p>
                    ))}
                </div>
            ) : null}
        </Card>
    );
}

function RunDetail({ runId, onExecuted }: { runId: string; onExecuted: () => void }) {
    const [refresh, setRefresh] = useState(0);
    const inspection = useApi(() => api.getReplay(runId), `run:${runId}:${refresh}`);
    const [projection, setProjection] = useState("");
    const [last, setLast] = useState<ReplayExecution | null>(null);
    const execute = useAction(() => api.executeReplay(runId, projection || undefined));
    const capabilities = useApi(() => api.capabilities(), "capabilities");

    if (inspection.loading && !inspection.data) return <Loading label="Loading run" />;
    if (inspection.error) return <ErrorState error={inspection.error} onRetry={inspection.reload} />;
    if (!inspection.data) return null;
    const { run, executions } = inspection.data;
    const latest = last ?? executions[executions.length - 1]?.report ?? null;

    return (
        <>
            <Card title={`Run ${shortDigest(run.id, 8, 4)}`}>
                <div className="flex flex-col gap-4">
                    <KeyValue
                        items={[
                            [
                                "Interval",
                                <span key="i" className="font-mono">
                                    ({run.from}, {run.to}] · {run.trace.length} events
                                </span>,
                            ],
                            ["Projection", `${run.projection.id}@v${run.projection.version}`],
                            [
                                "Start",
                                run.start_source.kind +
                                    (run.start_source.snapshot_id
                                        ? ` ${shortDigest(run.start_source.snapshot_id, 8, 4)}`
                                        : ""),
                            ],
                            [
                                "Boundary digests",
                                <span key="b">
                                    <Digest value={run.from_event_digest} /> → <Digest value={run.to_event_digest} />
                                </span>,
                            ],
                            ["Expected state digest", <Digest key="e" value={run.expected_state_digest} />],
                            ["Recorded", formatTimestamp(run.created_at)],
                        ]}
                    />
                    <form
                        className="flex flex-wrap items-end gap-3"
                        onSubmit={async (e) => {
                            e.preventDefault();
                            const result = await execute.run();
                            if (result) {
                                setLast(result);
                                setRefresh((n) => n + 1);
                                onExecuted();
                            }
                        }}
                    >
                        <div className="flex items-center gap-2">
                            <label htmlFor="execute-projection" className="text-xs font-medium text-muted">
                                Execute with
                            </label>
                            <Select
                                id="execute-projection"
                                value={projection}
                                onChange={(e) => setProjection(e.target.value)}
                            >
                                <option value="">
                                    recorded projection ({run.projection.id}@v{run.projection.version})
                                </option>
                                {(capabilities.data?.projections ?? []).map((p) => (
                                    <option key={p.key} value={p.id} title={p.description}>
                                        {p.key}
                                    </option>
                                ))}
                            </Select>
                        </div>
                        <Button type="submit" variant="primary" busy={execute.pending}>
                            Execute replay
                        </Button>
                    </form>
                    {execute.error ? <ErrorState title="Execution could not run" error={execute.error} /> : null}
                </div>
            </Card>
            {latest ? (
                <Evidence execution={latest} inspection={inspection.data} />
            ) : (
                <EmptyState title="Not executed yet.">Execute the run to compare it with the recording.</EmptyState>
            )}
            {executions.length > 0 ? (
                <Card title="Execution history" padded={false}>
                    <Table label="Executions">
                        <thead>
                            <tr>
                                <Th>Executed</Th>
                                <Th>Outcome</Th>
                                <Th>Projection</Th>
                                <Th>Actual digest</Th>
                                <Th className="text-right">Time</Th>
                            </tr>
                        </thead>
                        <tbody>
                            {[...executions].reverse().map((e) => (
                                <tr key={e.id}>
                                    <Td className="whitespace-nowrap text-xs text-muted">
                                        {formatTimestamp(e.executed_at)}
                                    </Td>
                                    <Td>
                                        <OutcomeBadge status={e.outcome} />
                                    </Td>
                                    <Td className="font-mono text-xs">
                                        {e.report.projection.id}@v{e.report.projection.version}
                                    </Td>
                                    <Td>
                                        {e.actual_state_digest ? (
                                            <Digest value={e.actual_state_digest} />
                                        ) : (
                                            <span className="text-faint">—</span>
                                        )}
                                    </Td>
                                    <Td className="text-right text-xs text-muted">
                                        {formatMicros(e.report.elapsed_micros)}
                                    </Td>
                                </tr>
                            ))}
                        </tbody>
                    </Table>
                </Card>
            ) : null}
        </>
    );
}

function Evidence({ execution, inspection }: { execution: ReplayExecution; inspection: ReplayInspection }) {
    const { run } = inspection;
    const outcome = execution.outcome;
    const divergence = "first_divergence" in outcome ? outcome.first_divergence : null;
    const divergedAt = divergence?.sequence ?? null;
    // Show a window of the trace around the first divergence, or its beginning.
    const start = divergedAt !== null ? Math.max(run.from + 1, divergedAt - 5) : run.from + 1;
    const end = Math.min(run.to, divergedAt !== null ? divergedAt + 5 : run.from + 200);
    const rows: number[] = [];
    for (let s = start; s <= end; s++) rows.push(s);

    return (
        <Card title="Latest execution evidence">
            <div className="flex flex-col gap-4">
                <div className="flex flex-wrap items-center gap-3 text-sm">
                    <OutcomeBadge status={outcome.status} />
                    <span className="text-muted">
                        {execution.events_applied} events applied · {formatMicros(execution.elapsed_micros)} (measured)
                        · executed {formatTimestamp(execution.executed_at)}
                    </span>
                </div>
                <KeyValue
                    items={[
                        [
                            "Expected final digest",
                            execution.expected_state_digest ? (
                                <Digest key="e" value={execution.expected_state_digest} full />
                            ) : (
                                "—"
                            ),
                        ],
                        [
                            "Actual final digest",
                            execution.actual_state_digest ? (
                                <Digest key="a" value={execution.actual_state_digest} full />
                            ) : (
                                <span key="a" className="text-faint">
                                    not computed
                                </span>
                            ),
                        ],
                        ["Executed with", `${execution.projection.id}@v${execution.projection.version}`],
                    ]}
                />
                {divergence ? (
                    <div role="alert" className="rounded-md border border-fail/40 bg-fail-soft p-3 text-sm">
                        <p className="font-semibold text-fail">First divergence after event #{divergence.sequence}</p>
                        <p className="mt-1 text-text/90">
                            <Mono>{divergence.event_type}</Mono> (event <Mono>{divergence.event_id}</Mono>).{" "}
                            {divergence.matched_events} earlier event(s) matched the recording. This is where the
                            difference became observable; it is not necessarily its cause.
                        </p>
                        <p className="mt-2 font-mono text-xs text-muted">
                            expected {shortDigest(divergence.expected_state_digest)} · actual{" "}
                            {shortDigest(divergence.actual_state_digest)}
                        </p>
                    </div>
                ) : null}
                {outcome.status === "diverged" ? (
                    <div>
                        <p className="mb-1 text-xs font-medium text-muted">Final state: expected → actual</p>
                        <DiffView changes={outcome.changes} />
                    </div>
                ) : null}
                {outcome.status === "projection_failed" ? (
                    <JsonView value={outcome.failure} label="Projection failure" maxHeight="12rem" />
                ) : null}
                {outcome.status === "unavailable" ? (
                    <JsonView value={outcome.reason} label="Why the replay is unavailable" maxHeight="12rem" />
                ) : null}
                {outcome.status === "invalid_input" ? <p className="text-sm text-warn">{outcome.reason}</p> : null}
                {outcome.status === "integrity_failure" ? (
                    <JsonView
                        value={outcome.violations as unknown as Json}
                        label="Integrity violations"
                        maxHeight="16rem"
                    />
                ) : null}
                {execution.warnings.map((w) => (
                    <p key={w} className="text-sm text-warn">
                        {w}
                    </p>
                ))}
                {run.trace.length > 0 ? (
                    <div>
                        <p className="mb-1 text-xs font-medium text-muted">
                            Recorded trace: state digest after each event (showing #{start}–#{end} of ({run.from},{" "}
                            {run.to}])
                        </p>
                        <Table label="Replay trace">
                            <thead>
                                <tr>
                                    <Th>Event</Th>
                                    <Th>Recorded state digest</Th>
                                    <Th>Comparison</Th>
                                </tr>
                            </thead>
                            <tbody>
                                {rows.map((s) => {
                                    const digest = run.trace[s - run.from - 1];
                                    const status =
                                        divergedAt === null
                                            ? outcome.status === "exact_match" || outcome.status === "reconverged"
                                                ? "match"
                                                : "not compared"
                                            : s < divergedAt
                                              ? "match"
                                              : s === divergedAt
                                                ? "first divergence"
                                                : "after divergence";
                                    return (
                                        <tr key={s} className={s === divergedAt ? "bg-fail-soft" : undefined}>
                                            <Td className="font-mono">#{s}</Td>
                                            <Td>{digest ? <Digest value={digest} /> : "—"}</Td>
                                            <Td>
                                                {status === "match" ? (
                                                    <Badge tone="ok">match</Badge>
                                                ) : status === "first divergence" ? (
                                                    <Badge tone="fail">first divergence</Badge>
                                                ) : (
                                                    <Badge>{status}</Badge>
                                                )}
                                            </Td>
                                        </tr>
                                    );
                                })}
                            </tbody>
                        </Table>
                    </div>
                ) : null}
            </div>
        </Card>
    );
}
