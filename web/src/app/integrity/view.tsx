"use client";

import { useState } from "react";

import { JsonView } from "@/components/json-view";
import { StreamPicker, useQueryParam } from "@/components/stream-picker";
import {
    Badge,
    Button,
    Card,
    Digest,
    EmptyState,
    ErrorState,
    KeyValue,
    Mono,
    PageHeader,
    Table,
    Td,
    Th,
} from "@/components/ui";
import { api } from "@/lib/api";
import { formatMicros, formatTimestamp } from "@/lib/format";
import type { Checkpoint, IntegrityReport, StoreIntegrityReport } from "@/lib/types";
import { useAction } from "@/lib/use-api";

const STORAGE_KEY = "orbit.checkpoints.v1";

type SavedCheckpoint = Checkpoint & { saved_at: string };

function loadCheckpoints(): Record<string, SavedCheckpoint[]> {
    try {
        const raw = window.localStorage.getItem(STORAGE_KEY);
        return raw ? (JSON.parse(raw) as Record<string, SavedCheckpoint[]>) : {};
    } catch {
        return {};
    }
}

function saveCheckpoints(all: Record<string, SavedCheckpoint[]>) {
    try {
        window.localStorage.setItem(STORAGE_KEY, JSON.stringify(all));
    } catch {
        // Storage unavailable (private mode); checkpoints then only live for this page view.
    }
}

export function IntegrityView() {
    const [stream, setStream] = useQueryParam("stream");
    const [storeReport, setStoreReport] = useState<StoreIntegrityReport | null>(null);
    const verifyStore = useAction(async () => setStoreReport(await api.verifyStore()));

    return (
        <>
            <PageHeader
                title="Integrity Center"
                description="Run the engine's verification on demand. Nothing here is shown as passing until a check has actually run."
            />
            <div className="flex flex-col gap-6">
                <Card
                    title="Whole store"
                    actions={
                        <Button variant="primary" onClick={() => void verifyStore.run()} busy={verifyStore.pending}>
                            {storeReport ? "Verify again" : "Verify store"}
                        </Button>
                    }
                >
                    {verifyStore.error ? (
                        <ErrorState title="Verification could not run" error={verifyStore.error} />
                    ) : null}
                    {!storeReport && !verifyStore.error ? (
                        <p className="text-sm text-muted">
                            Not run yet. This runs SQLite&apos;s integrity check and foreign-key check, looks for
                            missing protective triggers, and verifies every stream&apos;s hash chain and head, every
                            snapshot and every replay run.
                        </p>
                    ) : null}
                    {storeReport ? <StoreReport report={storeReport} /> : null}
                </Card>

                <Card
                    title="One stream"
                    actions={<StreamPicker value={stream} onChange={(id) => setStream(id)} id="integrity-stream" />}
                >
                    {stream ? (
                        <StreamIntegrity key={stream} stream={stream} />
                    ) : (
                        <EmptyState title="Select a stream to verify it against its own chain and any saved checkpoints." />
                    )}
                </Card>
            </div>
        </>
    );
}

function StoreReport({ report }: { report: StoreIntegrityReport }) {
    const invalid = report.streams.filter((s) => s.total_issues > 0);
    const sqliteOk = report.sqlite_integrity.length === 1 && report.sqlite_integrity[0] === "ok";
    const rows: [string, boolean, string][] = [
        ["SQLite integrity_check", sqliteOk, sqliteOk ? "ok" : report.sqlite_integrity.join("; ")],
        ["Foreign keys", report.foreign_key_violations === 0, `${report.foreign_key_violations} violations`],
        [
            "Protective triggers",
            report.missing_safeguards.length === 0,
            report.missing_safeguards.length ? `missing: ${report.missing_safeguards.join(", ")}` : "all present",
        ],
        ["Streams", invalid.length === 0, `${report.streams.length} checked, ${invalid.length} with issues`],
        [
            "Snapshots",
            report.snapshot_issues.length === 0,
            `${report.snapshots_checked} checked, ${report.snapshot_issues.length} unusable`,
        ],
        [
            "Replay runs",
            report.replay_run_issues.length === 0,
            `${report.replay_runs_checked} checked, ${report.replay_run_issues.length} invalid`,
        ],
    ];
    return (
        <div className="flex flex-col gap-4">
            <p className="flex items-center gap-3 text-sm">
                {report.valid ? (
                    <Badge tone="ok">store valid</Badge>
                ) : (
                    <Badge tone="fail">integrity issues found</Badge>
                )}
                <span className="text-muted">
                    schema v{report.schema_version} · {formatMicros(report.elapsed_micros)} (measured on server)
                </span>
            </p>
            <Table label="Store verification">
                <tbody>
                    {rows.map(([name, ok, detail]) => (
                        <tr key={name}>
                            <Td className="w-48 text-muted">{name}</Td>
                            <Td className="w-20">
                                {ok ? <Badge tone="ok">pass</Badge> : <Badge tone="fail">fail</Badge>}
                            </Td>
                            <Td className="text-sm">{detail}</Td>
                        </tr>
                    ))}
                </tbody>
            </Table>
            {invalid.map((s) => (
                <IssueTable key={s.stream_id} report={s} />
            ))}
            {report.snapshot_issues.length > 0 ? (
                <div>
                    <p className="mb-1 text-xs font-medium text-muted">Unusable snapshots</p>
                    <JsonView
                        value={report.snapshot_issues as unknown as import("@/lib/types").Json}
                        label="Snapshot issues"
                        maxHeight="14rem"
                    />
                </div>
            ) : null}
            {report.replay_run_issues.length > 0 ? (
                <div>
                    <p className="mb-1 text-xs font-medium text-muted">Invalid replay runs</p>
                    <JsonView
                        value={report.replay_run_issues as unknown as import("@/lib/types").Json}
                        label="Replay run issues"
                        maxHeight="14rem"
                    />
                </div>
            ) : null}
        </div>
    );
}

function IssueTable({ report }: { report: IntegrityReport }) {
    return (
        <div>
            <p className="mb-2 text-sm">
                <Mono>{report.stream_id}</Mono>: <span className="text-fail">{report.total_issues} issue(s)</span> in{" "}
                {report.events_checked} events
            </p>
            <Table label={`Integrity issues of ${report.stream_id}`}>
                <thead>
                    <tr>
                        <Th>Kind</Th>
                        <Th>Sequence</Th>
                        <Th>Event</Th>
                        <Th>Details</Th>
                    </tr>
                </thead>
                <tbody>
                    {report.issues.map((issue, i) => {
                        const seq = (issue.sequence ?? issue.found ?? issue.recorded_sequence ?? null) as number | null;
                        const affected = report.affected_events.find((a) => a.sequence === seq);
                        return (
                            <tr key={i}>
                                <Td>
                                    <Badge tone="fail">{issue.kind}</Badge>
                                </Td>
                                <Td className="font-mono">{seq ?? "—"}</Td>
                                <Td>{affected ? <Mono className="text-muted">{affected.event_id}</Mono> : "—"}</Td>
                                <Td className="max-w-md break-all font-mono text-xs text-muted">
                                    {JSON.stringify(issue)}
                                </Td>
                            </tr>
                        );
                    })}
                </tbody>
            </Table>
        </div>
    );
}

function StreamIntegrity({ stream }: { stream: string }) {
    // Remounted per stream (see `key`), so this initial state is per stream.
    const [saved, setSaved] = useState<SavedCheckpoint[]>(() => loadCheckpoints()[stream] ?? []);
    const [report, setReport] = useState<IntegrityReport | null>(null);
    const verify = useAction(async () =>
        setReport(
            await api.verifyStream(
                stream,
                saved.map(({ sequence, digest }) => ({ sequence, digest })),
            ),
        ),
    );
    const take = useAction(async () => {
        const checkpoint = await api.checkpoint(stream);
        const all = loadCheckpoints();
        const list = (all[stream] ?? []).filter((c) => c.sequence !== checkpoint.sequence);
        const next = [...list, { ...checkpoint, saved_at: new Date().toISOString() }].sort(
            (a, b) => a.sequence - b.sequence,
        );
        all[stream] = next;
        saveCheckpoints(all);
        setSaved(next);
    });

    return (
        <div className="flex flex-col gap-4">
            <div className="flex flex-wrap gap-2">
                <Button variant="primary" onClick={() => void verify.run()} busy={verify.pending}>
                    Verify stream{saved.length ? ` against ${saved.length} checkpoint(s)` : ""}
                </Button>
                <Button onClick={() => void take.run()} busy={take.pending}>
                    Save checkpoint of current head
                </Button>
            </div>
            <p className="text-xs text-faint">
                Checkpoints are kept in this browser&apos;s local storage, outside the database. Verifying against them
                detects even a history rewritten with recomputed digests, up to the checkpoint.
            </p>
            {take.error ? <ErrorState title="Checkpoint not taken" error={take.error} /> : null}
            {verify.error ? <ErrorState title="Verification could not run" error={verify.error} /> : null}
            {saved.length > 0 ? (
                <Table label="Saved checkpoints">
                    <thead>
                        <tr>
                            <Th>Sequence</Th>
                            <Th>Digest</Th>
                            <Th>Saved</Th>
                        </tr>
                    </thead>
                    <tbody>
                        {saved.map((c) => (
                            <tr key={c.sequence}>
                                <Td className="font-mono">#{c.sequence}</Td>
                                <Td>
                                    <Digest value={c.digest} />
                                </Td>
                                <Td className="text-xs text-muted">{formatTimestamp(c.saved_at)}</Td>
                            </tr>
                        ))}
                    </tbody>
                </Table>
            ) : null}
            {report ? (
                <div className="flex flex-col gap-3">
                    <KeyValue
                        items={[
                            [
                                "Result",
                                report.total_issues === 0 ? (
                                    <Badge key="r" tone="ok">
                                        valid
                                    </Badge>
                                ) : (
                                    <Badge key="r" tone="fail">
                                        {report.total_issues} issue(s)
                                    </Badge>
                                ),
                            ],
                            ["Events checked", report.events_checked],
                            [
                                "Head",
                                <span key="h" className="font-mono">
                                    #{report.head.sequence} <Digest value={report.head.digest} />
                                </span>,
                            ],
                        ]}
                    />
                    {report.total_issues > 0 ? <IssueTable report={report} /> : null}
                </div>
            ) : null}
        </div>
    );
}
