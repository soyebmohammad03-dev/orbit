"use client";

import { Badge, Card, EmptyState, ErrorState, Loading, Mono, PageHeader, Table, Td, Th } from "@/components/ui";
import { api } from "@/lib/api";
import { formatNanos, formatTimestamp } from "@/lib/format";
import type { BenchmarkResult } from "@/lib/types";
import { useApi } from "@/lib/use-api";

function groupBy(results: BenchmarkResult[]): [string, BenchmarkResult[]][] {
    const groups = new Map<string, BenchmarkResult[]>();
    for (const r of results) {
        const list = groups.get(r.group) ?? [];
        list.push(r);
        groups.set(r.group, list);
    }
    return [...groups.entries()];
}

export function BenchmarksView() {
    const data = useApi(() => api.benchmarks(), "benchmarks");
    return (
        <>
            <PageHeader
                title="Benchmarks"
                description={
                    <>
                        Criterion measurements from <Mono>cargo bench</Mono>, read from the results directory configured
                        on the API server. These are offline benchmark results, separate from the live request timings
                        shown on other pages (which come from the server&apos;s own measurement of each request).
                    </>
                }
            />
            {data.loading ? <Loading /> : null}
            {data.error ? <ErrorState error={data.error} onRetry={data.reload} /> : null}
            {data.data && data.data.results.length === 0 ? (
                <EmptyState
                    title={
                        data.data.available
                            ? "The results directory has no benchmark results."
                            : "No benchmark results are available."
                    }
                >
                    <p>{data.data.note}</p>
                    <p className="mt-2">
                        For example <Mono>cargo bench -p orbit-core --bench crash_lab</Mono>, then restart{" "}
                        <Mono>orbit-api</Mono> from the repository root (or set <Mono>ORBIT_BENCHMARK_DIR</Mono>).
                        Workloads and dataset sizes are documented in docs/DEVELOPMENT.md.
                    </p>
                </EmptyState>
            ) : null}
            {data.data && data.data.results.length > 0 ? (
                <div className="flex flex-col gap-6">
                    {groupBy(data.data.results).map(([group, results]) => (
                        <Card key={group} title={group} padded={false}>
                            <Table label={`Benchmark group ${group}`}>
                                <thead>
                                    <tr>
                                        <Th>Benchmark</Th>
                                        <Th className="text-right">Median</Th>
                                        <Th className="text-right">Confidence interval</Th>
                                        <Th className="text-right">Mean</Th>
                                        <Th>Throughput</Th>
                                        <Th>Measured</Th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {results.map((r) => (
                                        <tr key={r.id}>
                                            <Td>
                                                <Mono>{r.function}</Mono>
                                            </Td>
                                            <Td className="text-right font-mono">{formatNanos(r.median_ns)}</Td>
                                            <Td className="text-right font-mono text-xs text-muted">
                                                {formatNanos(r.median_lower_ns)} – {formatNanos(r.median_upper_ns)}
                                                {Number.isFinite(r.confidence_level)
                                                    ? ` (${Math.round(r.confidence_level * 100)}%)`
                                                    : ""}
                                            </Td>
                                            <Td className="text-right font-mono text-muted">
                                                {formatNanos(r.mean_ns)}
                                            </Td>
                                            <Td>
                                                {r.throughput ? (
                                                    <Badge>
                                                        {Object.entries(r.throughput)
                                                            .map(([k, v]) => `${String(v)} ${k.toLowerCase()}/iter`)
                                                            .join(", ")}
                                                    </Badge>
                                                ) : (
                                                    <span className="text-faint">—</span>
                                                )}
                                            </Td>
                                            <Td className="whitespace-nowrap text-xs text-muted">
                                                {formatTimestamp(r.measured_at)}
                                            </Td>
                                        </tr>
                                    ))}
                                </tbody>
                            </Table>
                        </Card>
                    ))}
                    <p className="text-xs text-faint">
                        Units: wall-clock time per iteration. Hardware and conditions for the recorded runs are listed
                        in docs/DEVELOPMENT.md.
                    </p>
                </div>
            ) : null}
        </>
    );
}
