"use client";

import { type ReactNode, useState } from "react";

import { DiffView } from "@/components/diff-view";
import { Badge, Button, Digest, ErrorState, KeyValue, Loading, Mono, Table, Td, TextLink, Th } from "@/components/ui";
import { api } from "@/lib/api";
import type {
    DifferentialResult,
    DivergenceInterval,
    IdentityAssessment,
    LabComparison,
    LabEventRef,
    LabSide,
    Timeline,
} from "@/lib/types";
import { useApi } from "@/lib/use-api";

/** How a side is labelled: peers without a reference, expected/observed with one. */
export function sideLabel(side: LabSide, reference: LabSide | null): string {
    if (!reference) return side;
    return side === reference ? `${side} (expected)` : `${side} (observed)`;
}

/**
 * The timeline of a result. Sprint 7 results only list segments, without
 * how they ended; those are shown as intervals whose end is unknown.
 */
export function timelineOf(result: DifferentialResult): { timeline: Timeline; legacy: boolean } {
    if (result.timeline) return { timeline: result.timeline, legacy: false };
    const segments = result.divergent_segments ?? [];
    const last = result.input.from + result.boundaries_compared - 1;
    return {
        legacy: true,
        timeline: {
            first_boundary: result.input.from,
            last_boundary: result.boundaries_compared > 0 ? last : null,
            intervals: segments.map((s, i) => ({
                index: i + 1,
                first: s.first,
                last: s.last,
                boundaries: s.last - s.first + 1,
                end: s.last < last ? "converged" : "open_at_end",
                converged_at: s.last < last ? s.last + 1 : null,
                convergence_event: null,
                entry: i === 0 ? result.first_divergence : null,
                exit: null,
            })),
            intervals_total: result.divergent_segments_total ?? segments.length,
            intervals_truncated: (result.divergent_segments_total ?? segments.length) > segments.length,
            convergences: segments.filter((s) => s.last < last).length,
            final_equal: result.final_comparison?.equal ?? null,
        },
    };
}

function eventLabel(e: LabEventRef): string {
    return `#${e.sequence} ${e.event_type}`;
}

const END_LABELS: Record<DivergenceInterval["end"], string> = {
    converged: "converged",
    open_at_end: "still different at the end",
    stopped_by_failure: "stopped by a failure",
};

/** A horizontal bar over the compared boundaries, with divergence intervals marked. */
export function DivergenceTimeline({
    timeline,
    onSelect,
}: {
    timeline: Timeline;
    onSelect?: (boundary: number) => void;
}) {
    const first = timeline.first_boundary;
    const last = timeline.last_boundary;
    if (last === null) return <p className="text-sm text-muted">No boundary was compared.</p>;
    const span = Math.max(1, last - first + 1);
    const pct = (b: number) => ((b - first) / span) * 100;
    const summary =
        timeline.intervals_total === 0
            ? `States equal at every boundary from ${first} to ${last}.`
            : `${timeline.intervals_total} divergence interval${timeline.intervals_total === 1 ? "" : "s"} between boundaries ${first} and ${last}; ${timeline.convergences} convergence${timeline.convergences === 1 ? "" : "s"}.`;
    return (
        <div className="flex flex-col gap-1">
            <div
                role="img"
                aria-label={summary}
                className="relative h-6 w-full overflow-hidden rounded border border-border bg-ok-soft"
            >
                {timeline.intervals.map((i) => (
                    <button
                        key={i.index}
                        type="button"
                        title={`Interval ${i.index}: boundaries ${i.first}–${i.last}, ${END_LABELS[i.end]}`}
                        aria-label={`Interval ${i.index}: boundaries ${i.first} to ${i.last}`}
                        onClick={() => onSelect?.(i.first)}
                        className="absolute top-0 h-full bg-warn/70 hover:bg-warn focus:outline focus:outline-2 focus:outline-accent"
                        style={{ left: `${pct(i.first)}%`, width: `max(3px, ${(i.boundaries / span) * 100}%)` }}
                    />
                ))}
            </div>
            <div className="flex justify-between font-mono text-[11px] text-faint">
                <span>boundary {first}</span>
                <span>boundary {last}</span>
            </div>
            <p className="text-xs text-muted">
                {summary}
                {timeline.intervals_truncated
                    ? ` Showing the first ${timeline.intervals.length}; the trace has every boundary.`
                    : ""}
            </p>
        </div>
    );
}

/** One comparison: event context, both digests, the structural diff. */
export function ComparisonView({
    comparison,
    reference,
    stream,
}: {
    comparison: LabComparison;
    reference: LabSide | null;
    stream: string;
}) {
    const e = comparison.event;
    return (
        <div className="flex flex-col gap-3">
            <KeyValue
                items={[
                    ["Boundary", <Mono key="b">{comparison.boundary}</Mono>],
                    [
                        "Event",
                        e ? (
                            <span key="e" className="flex flex-wrap items-center gap-2">
                                <Mono>{eventLabel(e)}</Mono>
                                <span className="text-xs text-faint">schema v{e.schema_version}</span>
                                <TextLink href={`/events?stream=${encodeURIComponent(stream)}&seq=${e.sequence}`}>
                                    open event
                                </TextLink>
                            </span>
                        ) : (
                            "initial state (before any event)"
                        ),
                    ],
                    ...(e?.metadata_keys && e.metadata_keys.length > 0
                        ? ([["Metadata keys", <Mono key="m">{e.metadata_keys.join(", ")}</Mono>]] as [
                              string,
                              ReactNode,
                          ][])
                        : []),
                    [sideLabel("left", reference), <Digest key="l" value={comparison.left.digest} />],
                    [sideLabel("right", reference), <Digest key="r" value={comparison.right.digest} />],
                ]}
            />
            {comparison.equal ? null : (
                <>
                    <p className="text-xs text-muted">
                        Each change reads {sideLabel("left", reference)} → {sideLabel("right", reference)}
                        {comparison.change_summary
                            ? `: ${comparison.change_summary.added} only on the right, ${comparison.change_summary.removed} only on the left, ${comparison.change_summary.modified} different (${comparison.change_summary.type_changed} of another JSON type).`
                            : "."}
                    </p>
                    <DiffView changes={comparison.changes} />
                    {comparison.changes_total > comparison.changes.length ? (
                        <p className="text-xs text-muted">
                            Showing {comparison.changes.length} of {comparison.changes_total} changes.
                        </p>
                    ) : null}
                </>
            )}
        </div>
    );
}

/** Expandable list of divergence intervals. */
export function IntervalList({
    timeline,
    reference,
    stream,
    legacy,
    onSelect,
}: {
    timeline: Timeline;
    reference: LabSide | null;
    stream: string;
    legacy: boolean;
    onSelect: (boundary: number) => void;
}) {
    if (timeline.intervals.length === 0) return null;
    return (
        <ol aria-label="Divergence intervals" className="flex flex-col gap-2">
            {timeline.intervals.map((i) => (
                <li key={i.index} className="rounded-md border border-border">
                    <details>
                        <summary className="flex cursor-pointer flex-wrap items-center gap-2 px-3 py-2 text-sm">
                            <span className="font-medium text-text">Interval {i.index}</span>
                            <Mono>
                                boundaries {i.first}–{i.last}
                            </Mono>
                            <span className="text-xs text-faint">
                                ({i.boundaries} boundar{i.boundaries === 1 ? "y" : "ies"})
                            </span>
                            {legacy ? (
                                <Badge>end not recorded</Badge>
                            ) : (
                                <Badge tone={i.end === "converged" ? "ok" : "warn"}>
                                    {i.end === "converged" ? `converged at ${i.converged_at}` : END_LABELS[i.end]}
                                </Badge>
                            )}
                        </summary>
                        <div className="flex flex-col gap-4 border-t border-border px-3 py-3">
                            <div>
                                <Button type="button" onClick={() => onSelect(i.first)}>
                                    Show trace around boundary {i.first}
                                </Button>
                            </div>
                            {i.entry ? (
                                <div>
                                    <h4 className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted">
                                        Entry (first differing boundary)
                                    </h4>
                                    <ComparisonView comparison={i.entry} reference={reference} stream={stream} />
                                </div>
                            ) : (
                                <p className="text-xs text-muted">
                                    No structural diff recorded for this interval; its digests are in the trace.
                                </p>
                            )}
                            {i.exit ? (
                                <div>
                                    <h4 className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted">
                                        Exit (last differing boundary)
                                    </h4>
                                    <ComparisonView comparison={i.exit} reference={reference} stream={stream} />
                                </div>
                            ) : null}
                            {i.convergence_event ? (
                                <p className="text-sm text-muted">
                                    Equal again after <Mono>{eventLabel(i.convergence_event)}</Mono>{" "}
                                    <TextLink
                                        href={`/events?stream=${encodeURIComponent(stream)}&seq=${i.convergence_event.sequence}`}
                                    >
                                        open event
                                    </TextLink>
                                </p>
                            ) : null}
                        </div>
                    </details>
                </li>
            ))}
        </ol>
    );
}

const WINDOW = 5;

/** The per-boundary trace around a boundary, from the API. */
export function TraceWindow({
    experimentId,
    around,
    stream,
}: {
    experimentId: string;
    around: number;
    stream: string;
}) {
    const [center, setCenter] = useState(around);
    const [prevAround, setPrevAround] = useState(around);
    if (around !== prevAround) {
        setPrevAround(around);
        setCenter(around);
    }
    const from = Math.max(0, center - WINDOW);
    const trace = useApi(
        () => api.getLabTrace(experimentId, { from, limit: 2 * WINDOW + 1 }),
        `lab:trace:${experimentId}:${from}`,
    );
    if (trace.loading && !trace.data) return <Loading label="Loading trace" />;
    if (trace.error) return <ErrorState title="Trace unavailable" error={trace.error} onRetry={trace.reload} />;
    if (!trace.data) return null;
    const page = trace.data;
    if (page.entries.length === 0) return <p className="text-sm text-muted">No trace entries in this range.</p>;
    const firstShown = page.entries[0]!.boundary;
    return (
        <div className="flex flex-col gap-2">
            <Table label="Trace">
                <thead>
                    <tr>
                        <Th>Boundary</Th>
                        <Th>Left digest</Th>
                        <Th>Right digest</Th>
                        <Th>States</Th>
                        <Th>Event</Th>
                    </tr>
                </thead>
                <tbody>
                    {page.entries.map((e) => (
                        <tr key={e.boundary} className={e.boundary === around ? "bg-surface-2" : undefined}>
                            <Td className="font-mono">{e.boundary}</Td>
                            <Td>
                                <Digest value={e.left} />
                            </Td>
                            <Td>
                                <Digest value={e.right} />
                            </Td>
                            <Td>
                                <Badge tone={e.equal ? "ok" : "warn"}>{e.equal ? "equal" : "differ"}</Badge>
                            </Td>
                            <Td>
                                {e.boundary > 0 ? (
                                    <TextLink href={`/events?stream=${encodeURIComponent(stream)}&seq=${e.boundary}`}>
                                        #{e.boundary}
                                    </TextLink>
                                ) : (
                                    <span className="text-xs text-faint">initial</span>
                                )}
                            </Td>
                        </tr>
                    ))}
                </tbody>
            </Table>
            <div className="flex flex-wrap gap-2">
                <Button
                    type="button"
                    disabled={firstShown <= page.first_boundary}
                    onClick={() => setCenter(Math.max(page.first_boundary, center - (2 * WINDOW + 1)))}
                >
                    Earlier
                </Button>
                <Button
                    type="button"
                    disabled={page.next_from === null}
                    onClick={() => setCenter(center + 2 * WINDOW + 1)}
                >
                    Later
                </Button>
                <span className="self-center text-xs text-faint">
                    {page.boundaries_total} boundaries in total ({page.first_boundary}–{page.last_boundary})
                </span>
            </div>
        </div>
    );
}

const COMPLETENESS: Record<IdentityAssessment["completeness"], { label: string; tone: "ok" | "warn" | "fail" }> = {
    executable: { label: "executable digest", tone: "ok" },
    source_revision: { label: "source revision only", tone: "warn" },
    declared: { label: "declared metadata only", tone: "fail" },
};

/** Declared, configuration and build identity, with its gaps. */
export function IdentityView({ identity }: { identity: IdentityAssessment }) {
    const c = COMPLETENESS[identity.completeness];
    return (
        <div className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center gap-2 text-sm">
                <span className="text-muted">Identity completeness</span>
                <Badge tone={c.tone}>{c.label}</Badge>
            </div>
            <KeyValue
                items={[
                    ...identity.sides.map(
                        (s) =>
                            [
                                `${s.side} configuration`,
                                s.config_digest ? <Digest key={s.side} value={s.config_digest} /> : "—",
                            ] as [string, ReactNode],
                    ),
                    [
                        "Git commit",
                        identity.build.git_commit ? (
                            <Mono key="g">{identity.build.git_commit.slice(0, 12)}</Mono>
                        ) : (
                            "not recorded"
                        ),
                    ],
                    [
                        "Executable",
                        identity.build.executable_digest ? (
                            <Digest key="x" value={identity.build.executable_digest} />
                        ) : (
                            "not recorded"
                        ),
                    ],
                ]}
            />
            <ul aria-label="Identity gaps" className="list-disc pl-5 text-xs text-muted">
                {identity.gaps.map((g) => (
                    <li key={g}>{g}</li>
                ))}
            </ul>
        </div>
    );
}

/** Evidence, kept apart from what it does not establish. */
export function DiagnosisView({ diagnosis }: { diagnosis: NonNullable<DifferentialResult["diagnosis"]> }) {
    return (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
            <div>
                <h4 className="mb-1 text-xs font-semibold uppercase tracking-wider text-muted">Observed</h4>
                <ul aria-label="Observed evidence" className="list-disc pl-5 text-sm text-text">
                    {diagnosis.evidence.map((e) => (
                        <li key={e}>{e}</li>
                    ))}
                </ul>
            </div>
            {diagnosis.not_established.length > 0 ? (
                <div>
                    <h4 className="mb-1 text-xs font-semibold uppercase tracking-wider text-muted">Not established</h4>
                    <ul aria-label="Not established" className="list-disc pl-5 text-sm text-muted">
                        {diagnosis.not_established.map((e) => (
                            <li key={e}>{e}</li>
                        ))}
                    </ul>
                </div>
            ) : null}
        </div>
    );
}
