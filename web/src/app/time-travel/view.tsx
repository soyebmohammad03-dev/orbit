"use client";

import { useState } from "react";

import { DiffView } from "@/components/diff-view";
import { JsonView } from "@/components/json-view";
import { ProjectionPicker, StreamPicker, useQueryParam } from "@/components/stream-picker";
import {
    Badge,
    Button,
    Card,
    EmptyState,
    ErrorState,
    Input,
    KeyValue,
    Loading,
    Mono,
    PageHeader,
    TextLink,
} from "@/components/ui";
import { api } from "@/lib/api";
import { formatMicros } from "@/lib/format";
import type { ReconstructionReport } from "@/lib/types";
import { useApi } from "@/lib/use-api";

export function TimeTravelView() {
    const [stream, setStream] = useQueryParam("stream");
    const [projectionParam, setProjection] = useQueryParam("projection");
    const [atParam, setAt] = useQueryParam("at");
    const projection = projectionParam ?? "merge-patch";
    const detail = useApi(() => api.getStream(stream ?? ""), stream ? `stream:${stream}` : null);
    const head = detail.data?.head_sequence;
    const at = atParam !== null && /^\d+$/.test(atParam) ? Number(atParam) : head;
    // The draft belongs to the position it was typed for; a new `at` resets it.
    const [draftState, setDraftState] = useState<{ for: number | undefined; value: string }>({
        for: undefined,
        value: "",
    });
    const draft = draftState.for === at ? draftState.value : at === undefined ? "" : String(at);
    const setDraft = (value: string) => setDraftState({ for: at, value });
    const [showExplain, setShowExplain] = useState(false);

    const ready = stream && at !== undefined && head !== undefined;
    const key = ready ? `${stream}:${projection}:${at}:${head}` : null;
    const historical = useApi(() => api.getState(stream ?? "", { projection, at }), key ? `state:${key}` : null);
    const current = useApi(
        () => api.getState(stream ?? "", { projection }),
        ready ? `current:${stream}:${projection}:${head}` : null,
    );
    const diff = useApi(
        () => api.getDiff(stream ?? "", { projection, from: at ?? 0, to: head }),
        key ? `diff:${key}` : null,
    );
    const explainKey = key && showExplain ? `explain:${key}` : null;
    const explain = useApi(() => api.getExplain(stream ?? "", { projection, from: at ?? 0, to: head }), explainKey);

    return (
        <>
            <PageHeader
                title="Time Travel"
                description="Reconstruct a stream's state at any historical sequence with the real engine, and compare it with the current state."
                actions={
                    <>
                        <StreamPicker value={stream} onChange={(id) => setStream(id, ["at"])} />
                        <ProjectionPicker value={projection} onChange={(p) => setProjection(p)} />
                    </>
                }
            />
            {!stream ? (
                <EmptyState title="Select a stream to travel through its history." />
            ) : detail.error ? (
                <ErrorState error={detail.error} onRetry={detail.reload} />
            ) : head === undefined ? (
                <Loading />
            ) : head === 0 ? (
                <EmptyState title="This stream has no events, so its only state is the initial state.">
                    Append events in the{" "}
                    <TextLink href={`/events?stream=${encodeURIComponent(stream)}`}>Event Explorer</TextLink>.
                </EmptyState>
            ) : (
                <div className="flex flex-col gap-6">
                    <Card>
                        <form
                            className="flex flex-wrap items-end gap-4"
                            onSubmit={(e) => {
                                e.preventDefault();
                                setAt(draft);
                            }}
                        >
                            <div className="flex min-w-72 flex-1 flex-col gap-1">
                                <label htmlFor="at-range" className="text-xs font-medium text-muted">
                                    Historical sequence (0 = initial state, {head} = head)
                                </label>
                                <input
                                    id="at-range"
                                    type="range"
                                    min={0}
                                    max={head}
                                    value={Number(draft) || 0}
                                    onChange={(e) => setDraft(e.target.value)}
                                    onMouseUp={() => setAt(draft)}
                                    onKeyUp={() => setAt(draft)}
                                    className="accent-[var(--accent)]"
                                />
                            </div>
                            <div className="flex items-end gap-2">
                                <Input
                                    aria-label="Sequence"
                                    type="number"
                                    min={0}
                                    max={head}
                                    value={draft}
                                    onChange={(e) => setDraft(e.target.value)}
                                    className="w-24 font-mono"
                                />
                                <Button type="submit" variant="primary">
                                    Reconstruct
                                </Button>
                            </div>
                        </form>
                    </Card>

                    <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
                        <Card title={`State at #${at}`}>
                            <StatePanel
                                loading={historical.loading}
                                error={historical.error}
                                reload={historical.reload}
                                state={historical.data?.data.state}
                                report={historical.data?.data.report}
                                serverMicros={historical.data?.serverMicros ?? null}
                            />
                        </Card>
                        <Card title={`Current state (head #${head})`}>
                            <StatePanel
                                loading={current.loading}
                                error={current.error}
                                reload={current.reload}
                                state={current.data?.data.state}
                                report={current.data?.data.report}
                                serverMicros={current.data?.serverMicros ?? null}
                            />
                        </Card>
                    </div>

                    <Card
                        title={`Changes from #${at} to #${head}`}
                        actions={
                            <Button variant="ghost" onClick={() => setShowExplain((s) => !s)}>
                                {showExplain ? "Hide per-event changes" : "Show per-event changes"}
                            </Button>
                        }
                    >
                        {diff.loading ? <Loading /> : null}
                        {diff.error ? <ErrorState error={diff.error} onRetry={diff.reload} /> : null}
                        {diff.data ? (
                            <div className="flex flex-col gap-3">
                                <p className="text-xs text-muted">
                                    {diff.data.changes.length} changed paths across {diff.data.events.length} events.
                                    Paths are JSON Pointers; arrays are compared by index.
                                </p>
                                <DiffView changes={diff.data.changes} />
                            </div>
                        ) : null}
                        {showExplain ? (
                            <div className="mt-6 border-t border-border pt-4">
                                <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-muted">
                                    Which event changed what
                                </h3>
                                {explain.loading ? <Loading /> : null}
                                {explain.error ? <ErrorState error={explain.error} onRetry={explain.reload} /> : null}
                                {explain.data ? (
                                    explain.data.transitions.length === 0 ? (
                                        <p className="text-sm text-muted">No events in this interval.</p>
                                    ) : (
                                        <ol className="flex flex-col gap-3">
                                            {explain.data.transitions.map((t) => (
                                                <li
                                                    key={t.event.event_id}
                                                    className="rounded-md border border-border p-3"
                                                >
                                                    <p className="mb-2 flex items-center gap-2 text-sm">
                                                        <span className="font-mono text-text">#{t.event.sequence}</span>
                                                        <Mono className="text-muted">{t.event.event_type}</Mono>
                                                        {t.changes.length === 0 ? (
                                                            <Badge>no visible effect</Badge>
                                                        ) : null}
                                                    </p>
                                                    {t.changes.length > 0 ? <DiffView changes={t.changes} /> : null}
                                                </li>
                                            ))}
                                        </ol>
                                    )
                                ) : null}
                            </div>
                        ) : null}
                    </Card>
                </div>
            )}
        </>
    );
}

function StatePanel({
    loading,
    error,
    reload,
    state,
    report,
    serverMicros,
}: {
    loading: boolean;
    error: { message: string; code?: string; status?: number } | undefined;
    reload: () => void;
    state: import("@/lib/types").Json | undefined;
    report: ReconstructionReport | undefined;
    serverMicros: number | null;
}) {
    if (loading && !report) return <Loading label="Reconstructing" />;
    if (error) return <ErrorState error={error} onRetry={reload} title="Reconstruction failed" />;
    if (!report) return null;
    return (
        <div className="flex flex-col gap-3">
            <KeyValue
                items={[
                    [
                        "Reconstructed",
                        <span key="r" className="font-mono">
                            #{report.reconstructed}
                        </span>,
                    ],
                    [
                        "Start point",
                        report.start.kind === "snapshot" ? (
                            <Badge key="s" tone="info">
                                snapshot #{report.start.sequence}
                            </Badge>
                        ) : (
                            <Badge key="s">initial state</Badge>
                        ),
                    ],
                    ["Events applied", report.events_applied],
                    ["Engine time", `${formatMicros(report.elapsed_micros)} (measured)`],
                    ["Request time", serverMicros === null ? "—" : `${formatMicros(serverMicros)} (server)`],
                ]}
            />
            {report.snapshot_warnings.length > 0 ? (
                <div role="alert" className="rounded-md border border-warn/40 bg-warn-soft px-3 py-2 text-sm text-warn">
                    {report.snapshot_warnings.length} snapshot(s) rejected during selection:{" "}
                    {report.snapshot_warnings.map((w) => `#${w.sequence} (${w.problem.kind})`).join(", ")}
                </div>
            ) : null}
            <JsonView value={state} label="Reconstructed state" />
        </div>
    );
}
