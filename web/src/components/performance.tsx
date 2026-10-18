import { Badge, Card, Digest, KeyValue, Mono, Table, Td, Th } from "@/components/ui";
import {
    LAB_OUTCOME_LABELS,
    VERDICT_LABELS,
    formatNanos,
    formatPercent,
    labOutcomeTone,
    verdictTone,
} from "@/lib/format";
import type { Comparison, ExperimentRecord, Interval, PerformanceResult, StatsSummary } from "@/lib/types";

/** Narrows an experiment's result to a performance result. */
export function performanceResult(record: Pick<ExperimentRecord, "kind" | "result">): PerformanceResult | null {
    return record.kind === "performance" && record.result ? (record.result as unknown as PerformanceResult) : null;
}

function interval(i: Interval | null): string {
    if (!i) return "—";
    const coverage = Math.abs(i.coverage - i.level) > 1e-9 ? ` (coverage ${(i.coverage * 100).toFixed(1)} %)` : "";
    return `${formatNanos(i.lower)} – ${formatNanos(i.upper)}${coverage}`;
}

/** The recorded samples, in iteration order, with the median as a line. Drawn only from real samples. */
export function SamplesChart({ samples, median }: { samples: number[]; median: number }) {
    const max = Math.max(...samples, 1);
    const width = 320;
    const height = 80;
    const bar = width / Math.max(samples.length, 1);
    const y = (v: number) => height - (v / max) * (height - 4);
    return (
        <svg
            role="img"
            aria-label={`${samples.length} measured iterations from ${formatNanos(Math.min(...samples))} to ${formatNanos(max)}; median ${formatNanos(median)}`}
            viewBox={`0 0 ${width} ${height}`}
            className="h-20 w-full max-w-md"
            preserveAspectRatio="none"
        >
            {samples.map((s, i) => (
                <rect
                    key={i}
                    x={i * bar + bar * 0.1}
                    width={Math.max(bar * 0.8, 0.5)}
                    y={y(s)}
                    height={height - y(s)}
                    className="fill-accent/60"
                />
            ))}
            <line x1={0} x2={width} y1={y(median)} y2={y(median)} className="stroke-text" strokeDasharray="4 3" />
        </svg>
    );
}

export function StatsTable({ summary, label }: { summary: StatsSummary; label: string }) {
    const rows: [string, string][] = [
        ["Samples", String(summary.count)],
        ["Median", formatNanos(summary.median)],
        ["Mean", formatNanos(summary.mean)],
        ["Standard deviation", summary.std_dev === null ? "—" : formatNanos(summary.std_dev)],
        ["Median absolute deviation", formatNanos(summary.mad)],
        [
            "Coefficient of variation",
            summary.coefficient_of_variation === null
                ? "—"
                : `${(summary.coefficient_of_variation * 100).toFixed(1)} %`,
        ],
        ["Minimum", formatNanos(summary.min)],
        ["Maximum", formatNanos(summary.max)],
        ...summary.percentiles.map((p) => [`p${Math.round(p.p * 100)}`, formatNanos(p.value)] as [string, string]),
        ["95 % interval of the median (distribution-free)", interval(summary.median_ci)],
        ["95 % interval of the mean (Student t)", interval(summary.mean_ci)],
    ];
    return (
        <Table label={label}>
            <thead>
                <tr>
                    <Th>Statistic</Th>
                    <Th>Value</Th>
                </tr>
            </thead>
            <tbody>
                {rows.map(([k, v]) => (
                    <tr key={k}>
                        <Td className="text-xs">{k}</Td>
                        <Td className="font-mono text-xs">{v}</Td>
                    </tr>
                ))}
            </tbody>
        </Table>
    );
}

/** A stored performance experiment. */
export function PerformanceSummary({ record }: { record: ExperimentRecord }) {
    const result = performanceResult(record);
    const spec = record.spec as unknown as Record<string, number | string | null>;
    const pending = record.status === "created" || record.status === "running";
    return (
        <>
            <Card
                title="Performance experiment"
                actions={
                    result ? (
                        <Badge tone={labOutcomeTone(result.evidence_level)}>
                            {LAB_OUTCOME_LABELS[result.evidence_level] ?? result.evidence_level}
                        </Badge>
                    ) : null
                }
            >
                <KeyValue
                    items={[
                        ["Id", <Mono key="id">{record.id}</Mono>],
                        ["Workload", <Mono key="w">{`${spec.workload ?? "?"} v${spec.workload_version ?? "?"}`}</Mono>],
                        [
                            "Configuration",
                            <Mono key="c">
                                {`input ${spec.input_size} · warm-up ${spec.warmup_iterations} · measured ${spec.measured_iterations} · seed ${spec.seed}`}
                            </Mono>,
                        ],
                        ["Status", record.status],
                    ]}
                />
                {pending ? (
                    <p role="status" className="mt-3 text-sm text-muted">
                        Measuring in the background on the server. This page refreshes until it finishes; the
                        measurement cannot be cancelled.
                    </p>
                ) : null}
                {record.failure ? (
                    <div role="alert" className="mt-4 rounded-md border border-fail/30 bg-fail-soft p-3 text-sm">
                        <p className="font-medium text-fail">{record.failure.kind.replaceAll("_", " ")}</p>
                        <p className="text-text">{record.failure.message}</p>
                        {record.failure.kind === "infrastructure" ? (
                            <p className="mt-1 text-xs text-muted">
                                The measurement could not be carried out. No numbers were recorded.
                            </p>
                        ) : null}
                    </div>
                ) : null}
            </Card>
            {result ? (
                <Card title="Measurements">
                    <div className="flex flex-col gap-4">
                        <p className="text-sm text-muted">{result.workload.iteration}.</p>
                        {result.evidence_level === "smoke" ? (
                            <p role="note" className="rounded-md border border-warn/30 bg-warn-soft p-3 text-sm">
                                Smoke run: {result.evidence_notes.join("; ")}. It shows that the workload runs, not how
                                fast it is.
                            </p>
                        ) : (
                            <p role="note" className="text-xs text-muted">
                                One run in one process on one machine: the intervals describe variability within this
                                run only.
                            </p>
                        )}
                        <SamplesChart samples={result.samples_ns} median={result.summary.median} />
                        <StatsTable summary={result.summary} label="Statistics" />
                        {result.throughput_per_second_at_median !== null ? (
                            <p className="text-sm">
                                Throughput at the median:{" "}
                                <Mono>
                                    {result.throughput_per_second_at_median.toPrecision(4)} {result.workload.input_unit}
                                    /s
                                </Mono>
                            </p>
                        ) : null}
                        {result.summary.notes.length > 0 ? (
                            <ul aria-label="Not computed" className="list-disc pl-5 text-xs text-muted">
                                {result.summary.notes.map((n) => (
                                    <li key={n}>{n}</li>
                                ))}
                            </ul>
                        ) : null}
                        <KeyValue
                            items={[
                                [
                                    "Environment",
                                    <Mono key="env">
                                        {`${result.environment.os}/${result.environment.arch} · ${result.environment.build_profile} build · ${result.environment.available_parallelism ?? "?"} CPUs`}
                                    </Mono>,
                                ],
                                [
                                    "Measured by",
                                    <span key="proc" className="flex flex-col">
                                        <Mono>{result.environment.process ?? "not recorded"}</Mono>
                                        {result.environment.process === "orbit-api" ? (
                                            <span className="text-xs text-warn">
                                                Inside the API server, which also serves requests. The CLI (orbit-lab
                                                measure) measures in a process of its own.
                                            </span>
                                        ) : null}
                                    </span>,
                                ],
                                [
                                    "Orchestration overhead",
                                    <Mono key="ovh">
                                        {result.overhead
                                            ? `prepare ${formatNanos(result.overhead.prepare_ns)} · untimed setup and checks ${formatNanos(result.overhead.untimed_ns)} of ${formatNanos(result.overhead.iterations_wall_ns)} loop time`
                                            : "not recorded"}
                                    </Mono>,
                                ],
                                ["Configuration digest", <Digest key="cd" value={result.configuration_digest} />],
                                [
                                    "Warm-up samples",
                                    <Mono key="wu">
                                        {result.warmup_samples_ns.length === 0
                                            ? "none"
                                            : result.warmup_samples_ns.map((s) => formatNanos(s)).join(", ")}
                                    </Mono>,
                                ],
                            ]}
                        />
                        <ul aria-label="Limitations" className="list-disc pl-5 text-xs text-muted">
                            {result.limitations.map((l) => (
                                <li key={l}>{l}</li>
                            ))}
                        </ul>
                    </div>
                </Card>
            ) : null}
        </>
    );
}

/** One comparison of a candidate against a baseline. */
export function ComparisonView({ comparison }: { comparison: Comparison }) {
    const c = comparison;
    return (
        <div className="flex flex-col gap-2 text-sm">
            <span className="flex flex-wrap items-center gap-2">
                <Badge tone={verdictTone(c.verdict)}>{VERDICT_LABELS[c.verdict] ?? c.verdict}</Badge>
                {c.relative_change !== null ? (
                    <Mono>
                        median {formatNanos(c.baseline_median_ns ?? NaN)} → {formatNanos(c.candidate_median_ns ?? NaN)}{" "}
                        ({formatPercent(c.relative_change)})
                    </Mono>
                ) : null}
            </span>
            <p className="text-muted">{c.explanation}</p>
            {c.test ? (
                <p className="text-xs text-muted">
                    Mann–Whitney U = {c.test.u}, two-sided p = {c.test.p_value.toExponential(2)} (α = {c.policy.alpha},
                    threshold {c.policy.threshold_percent} %).
                    {c.median_intervals_overlap === null
                        ? null
                        : c.median_intervals_overlap
                          ? " The two median intervals overlap."
                          : " The two median intervals do not overlap."}
                </p>
            ) : null}
            {c.compatibility.incompatible.length > 0 ? (
                <ul aria-label="Incompatible" className="list-disc pl-5 text-xs text-fail">
                    {c.compatibility.incompatible.map((w) => (
                        <li key={w}>{w}</li>
                    ))}
                </ul>
            ) : null}
            {c.compatibility.warnings.length > 0 ? (
                <ul aria-label="Comparison warnings" className="list-disc pl-5 text-xs text-muted">
                    {c.compatibility.warnings.map((w) => (
                        <li key={w}>{w}</li>
                    ))}
                </ul>
            ) : null}
        </div>
    );
}
