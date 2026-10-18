"use client";

import Link from "next/link";
import { useEffect, useState } from "react";

import { ComparisonView } from "@/components/performance";
import {
    Badge,
    Button,
    Card,
    EmptyState,
    ErrorState,
    Field,
    Input,
    Loading,
    Mono,
    PageHeader,
    Select,
    Table,
    Td,
    Th,
} from "@/components/ui";
import { api } from "@/lib/api";
import { LAB_OUTCOME_LABELS, formatTimestamp, labOutcomeTone, shortDigest } from "@/lib/format";
import type { CompareResponse, ExperimentSummary, WorkloadCatalog } from "@/lib/types";
import { useAction, useApi } from "@/lib/use-api";

const PAGE = 50;

export function PerformanceView() {
    const catalog = useApi(() => api.performanceWorkloads(), "perf:catalog");
    const list = useApi(() => api.listLabExperiments({ kind: "performance", limit: PAGE }), "perf:list");
    const pending = list.data?.experiments.some((e) => e.status === "created" || e.status === "running") ?? false;
    const refresh = () => {
        list.reload();
        catalog.reload();
    };

    // Poll while a measurement runs on the server; stop when none does.
    const { reload: reloadList } = list;
    const { reload: reloadCatalog } = catalog;
    useEffect(() => {
        if (!pending) return;
        const timer = setInterval(() => {
            reloadList();
            reloadCatalog();
        }, 1000);
        return () => clearInterval(timer);
    }, [pending, reloadList, reloadCatalog]);

    return (
        <>
            <PageHeader
                title="Performance"
                description="Repeatable measurements of ORBIT operations, recorded as experiments with every sample, statistics, configuration and environment. Comparisons refuse incompatible configurations and separate suspected changes from statistically supported ones."
            />
            <div className="grid grid-cols-1 gap-6 xl:grid-cols-[minmax(0,1fr)_24rem]">
                <div className="flex min-w-0 flex-col gap-6">
                    <Card title="Measured experiments" padded={false}>
                        {list.loading && !list.data ? (
                            <div className="px-4">
                                <Loading />
                            </div>
                        ) : null}
                        {list.error ? (
                            <div className="p-4">
                                <ErrorState error={list.error} onRetry={list.reload} />
                            </div>
                        ) : null}
                        {list.data && list.data.experiments.length === 0 ? (
                            <div className="p-4">
                                <EmptyState title="No measurements yet.">Run a workload with the form.</EmptyState>
                            </div>
                        ) : null}
                        {list.data && list.data.experiments.length > 0 ? (
                            <CompareTable
                                experiments={list.data.experiments}
                                policy={catalog.data?.default_policy ?? { threshold_percent: 10, alpha: 0.01 }}
                                truncated={list.data.next_before !== null}
                            />
                        ) : null}
                    </Card>
                </div>
                <div className="flex min-w-0 flex-col gap-6">
                    {catalog.error ? <ErrorState error={catalog.error} onRetry={catalog.reload} /> : null}
                    {catalog.data ? <NewMeasurement catalog={catalog.data} onCreated={refresh} /> : null}
                    <Card title="Criterion benchmarks">
                        <p className="text-sm text-muted">
                            Microbenchmarks from <Mono>cargo bench</Mono> are read from disk on the{" "}
                            <Link className="text-accent hover:underline" href="/benchmarks">
                                Benchmarks
                            </Link>{" "}
                            page. They are not registered experiments and are not compared here.
                        </p>
                    </Card>
                </div>
            </div>
        </>
    );
}

function NewMeasurement({ catalog, onCreated }: { catalog: WorkloadCatalog; onCreated: () => void }) {
    const [workloadId, setWorkload] = useState(catalog.workloads[0]?.id ?? "");
    const workload = catalog.workloads.find((w) => w.id === workloadId) ?? catalog.workloads[0];
    const [input, setInput] = useState("");
    const [warmup, setWarmup] = useState("");
    const [measured, setMeasured] = useState("");
    const [seed, setSeed] = useState("");
    const [accepted, setAccepted] = useState<string | null>(null);
    const create = useAction(() =>
        api.createPerformance({
            workload: workloadId,
            input_size: input ? Number(input) : null,
            warmup_iterations: warmup ? Number(warmup) : null,
            measured_iterations: measured ? Number(measured) : null,
            seed: seed ? Number(seed) : null,
        }),
    );
    if (!workload) return null;
    const outOfRange = (value: string, b: { min: number; max: number }) =>
        value !== "" && (!Number.isInteger(Number(value)) || Number(value) < b.min || Number(value) > b.max);
    const invalid =
        outOfRange(input, workload.input_size) ||
        outOfRange(warmup, catalog.warmup_iterations) ||
        outOfRange(measured, catalog.measured_iterations) ||
        (seed !== "" && (!Number.isInteger(Number(seed)) || Number(seed) < 0));
    return (
        <Card title="New measurement">
            <form
                className="flex flex-col gap-3"
                onSubmit={async (e) => {
                    e.preventDefault();
                    const record = await create.run();
                    if (record) {
                        setAccepted(record.id);
                        onCreated();
                    }
                }}
            >
                <Field label="Workload" htmlFor="perf-workload" hint={workload.iteration}>
                    <Select id="perf-workload" value={workloadId} onChange={(e) => setWorkload(e.target.value)}>
                        {catalog.workloads.map((w) => (
                            <option key={w.id} value={w.id}>
                                {w.title}
                            </option>
                        ))}
                    </Select>
                </Field>
                <Field
                    label={`Input size (${workload.input_size_meaning})`}
                    htmlFor="perf-input"
                    hint={`${workload.input_size.min}–${workload.input_size.max}, default ${workload.input_size.default}`}
                >
                    <Input
                        id="perf-input"
                        type="number"
                        min={workload.input_size.min}
                        max={workload.input_size.max}
                        value={input}
                        onChange={(e) => setInput(e.target.value)}
                    />
                </Field>
                <div className="grid grid-cols-2 gap-3">
                    <Field
                        label="Warm-up iterations"
                        htmlFor="perf-warmup"
                        hint={`${catalog.warmup_iterations.min}–${catalog.warmup_iterations.max}, default ${catalog.warmup_iterations.default}`}
                    >
                        <Input
                            id="perf-warmup"
                            type="number"
                            min={catalog.warmup_iterations.min}
                            max={catalog.warmup_iterations.max}
                            value={warmup}
                            onChange={(e) => setWarmup(e.target.value)}
                        />
                    </Field>
                    <Field
                        label="Measured iterations"
                        htmlFor="perf-measured"
                        hint={`${catalog.measured_iterations.min}–${catalog.measured_iterations.max}, default ${catalog.measured_iterations.default}`}
                    >
                        <Input
                            id="perf-measured"
                            type="number"
                            min={catalog.measured_iterations.min}
                            max={catalog.measured_iterations.max}
                            value={measured}
                            onChange={(e) => setMeasured(e.target.value)}
                        />
                    </Field>
                </div>
                <Field label="Seed" htmlFor="perf-seed" hint="Generated inputs depend on it; default 1.">
                    <Input
                        id="perf-seed"
                        type="number"
                        min={0}
                        value={seed}
                        onChange={(e) => setSeed(e.target.value)}
                    />
                </Field>
                {catalog.build_profile === "debug" ? (
                    <p role="note" className="text-xs text-warn">
                        This server is an unoptimized (debug) build, so every result is labelled smoke. Run{" "}
                        <Mono>cargo run --release -p orbit-api</Mono> to measure.
                    </p>
                ) : null}
                {invalid ? (
                    <p role="alert" className="text-xs text-fail">
                        A value is outside its allowed range.
                    </p>
                ) : null}
                {create.error ? <ErrorState title="Measurement not started" error={create.error} /> : null}
                {accepted ? (
                    <p role="status" className="text-xs text-muted">
                        Accepted as <Mono>{shortDigest(accepted, 8, 4)}</Mono>; it is measured in the background.
                    </p>
                ) : null}
                <div>
                    <Button
                        type="submit"
                        variant="primary"
                        busy={create.pending}
                        disabled={invalid || catalog.measuring}
                    >
                        {catalog.measuring ? "A measurement is running" : "Measure"}
                    </Button>
                </div>
            </form>
        </Card>
    );
}

function CompareTable({
    experiments,
    policy,
    truncated,
}: {
    experiments: ExperimentSummary[];
    policy: { threshold_percent: number; alpha: number };
    truncated: boolean;
}) {
    const [baseline, setBaseline] = useState<string | null>(null);
    const [candidates, setCandidates] = useState<string[]>([]);
    const [threshold, setThreshold] = useState(String(policy.threshold_percent));
    const [alpha, setAlpha] = useState(String(policy.alpha));
    const [result, setResult] = useState<CompareResponse | null>(null);
    const compare = useAction(() =>
        api.comparePerformance({
            baseline,
            candidates,
            threshold_percent: Number(threshold),
            alpha: Number(alpha),
        }),
    );
    const toggle = (id: string) =>
        setCandidates((c) => (c.includes(id) ? c.filter((x) => x !== id) : [...c, id].slice(-10)));
    return (
        <div className="flex flex-col">
            <Table label="Performance experiments">
                <thead>
                    <tr>
                        <Th>Experiment</Th>
                        <Th>Created</Th>
                        <Th>Status</Th>
                        <Th>Baseline</Th>
                        <Th>Candidate</Th>
                    </tr>
                </thead>
                <tbody>
                    {experiments.map((e) => (
                        <tr key={e.id}>
                            <Td>
                                <Link
                                    className="font-mono text-xs text-accent hover:underline"
                                    href={`/experiments?experiment=${e.id}`}
                                >
                                    {shortDigest(e.id, 8, 4)}
                                </Link>
                            </Td>
                            <Td className="text-xs">{formatTimestamp(e.created_at)}</Td>
                            <Td>
                                {e.status === "completed" && e.outcome ? (
                                    <Badge tone={labOutcomeTone(e.outcome)}>
                                        {LAB_OUTCOME_LABELS[e.outcome] ?? e.outcome}
                                    </Badge>
                                ) : (
                                    <Badge tone={e.status === "failed" ? "fail" : "neutral"}>{e.status}</Badge>
                                )}
                            </Td>
                            <Td>
                                <input
                                    type="radio"
                                    name="baseline"
                                    aria-label={`Use ${e.id} as the baseline`}
                                    disabled={e.status !== "completed"}
                                    checked={baseline === e.id}
                                    onChange={() => setBaseline(e.id)}
                                />
                            </Td>
                            <Td>
                                <input
                                    type="checkbox"
                                    aria-label={`Compare ${e.id}`}
                                    disabled={e.status !== "completed"}
                                    checked={candidates.includes(e.id)}
                                    onChange={() => toggle(e.id)}
                                />
                            </Td>
                        </tr>
                    ))}
                </tbody>
            </Table>
            {truncated ? <p className="px-4 pt-2 text-xs text-muted">Showing the newest {PAGE} experiments.</p> : null}
            <form
                className="flex flex-wrap items-end gap-3 p-4"
                onSubmit={async (e) => {
                    e.preventDefault();
                    const r = await compare.run();
                    if (r) setResult(r);
                }}
            >
                <Field label="Threshold (%)" htmlFor="perf-threshold">
                    <Input
                        id="perf-threshold"
                        type="number"
                        step="any"
                        min={0.1}
                        max={100}
                        value={threshold}
                        onChange={(e) => setThreshold(e.target.value)}
                    />
                </Field>
                <Field label="Significance level α" htmlFor="perf-alpha">
                    <Input
                        id="perf-alpha"
                        type="number"
                        step="any"
                        min={0.0001}
                        max={0.1}
                        value={alpha}
                        onChange={(e) => setAlpha(e.target.value)}
                    />
                </Field>
                <Button type="submit" busy={compare.pending} disabled={candidates.length === 0}>
                    Compare{" "}
                    {candidates.length > 0
                        ? `${candidates.length} with ${baseline ? "the baseline" : "the newest compatible baseline"}`
                        : ""}
                </Button>
                {baseline ? (
                    <Button type="button" variant="ghost" onClick={() => setBaseline(null)}>
                        Clear baseline
                    </Button>
                ) : null}
            </form>
            {compare.error ? (
                <div className="px-4 pb-4">
                    <ErrorState title="Comparison failed" error={compare.error} />
                </div>
            ) : null}
            {result ? <CompareResult result={result} /> : null}
        </div>
    );
}

function CompareResult({ result }: { result: CompareResponse }) {
    if (result.status === "no_baseline") {
        return (
            <div role="status" className="border-t border-border p-4 text-sm">
                <Badge tone="warn">No baseline</Badge> <span className="text-muted">{result.reason}</span>
            </div>
        );
    }
    return (
        <div className="flex flex-col gap-4 border-t border-border p-4">
            <p className="text-sm text-muted">
                Baseline <Mono>{shortDigest(result.baseline.experiment_id, 8, 4)}</Mono> (
                {result.baseline_selection === "given" ? "chosen" : "newest compatible earlier experiment"},{" "}
                {result.baseline.samples} samples, {result.baseline.evidence_level}).
            </p>
            <ol aria-label="Comparisons" className="flex flex-col gap-3">
                {result.comparisons.map((c) => (
                    <li key={c.candidate.experiment_id} className="rounded-md border border-border p-3">
                        <p className="mb-2 text-xs text-muted">
                            Candidate <Mono>{shortDigest(c.candidate.experiment_id, 8, 4)}</Mono> ({c.candidate.samples}{" "}
                            samples, {c.candidate.evidence_level})
                        </p>
                        <ComparisonView comparison={c.comparison} />
                    </li>
                ))}
            </ol>
        </div>
    );
}
