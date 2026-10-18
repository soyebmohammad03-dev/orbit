"use client";

import { Badge, Card, ErrorState, KeyValue, Loading, Mono, Table, Td, Th } from "@/components/ui";
import { api } from "@/lib/api";
import { formatMicros } from "@/lib/format";
import type { ExperimentRecord, FailureMode, ReliabilityCase, ReliabilityResult } from "@/lib/types";
import { useApi } from "@/lib/use-api";

const MODE_LABELS: Record<FailureMode, string> = {
    simulated_error: "simulated error",
    process_abort: "process abort",
    process_killed: "process killed (SIGKILL)",
    process_paused: "process paused",
    deliberate_corruption: "deliberate corruption",
    concurrent_processes: "concurrent processes",
};

const VERDICT_LABELS: Record<ReliabilityCase["verdict"], string> = {
    invariant_preserved: "preserved",
    invariant_violated: "violated",
    inconclusive: "inconclusive",
};

function verdictTone(v: ReliabilityCase["verdict"]): "ok" | "fail" | "warn" {
    return v === "invariant_preserved" ? "ok" : v === "invariant_violated" ? "fail" : "warn";
}

/** Badge that makes the simulated/real distinction impossible to miss. */
export function ModeBadge({ mode, real }: { mode: FailureMode; real: boolean }) {
    const [prefix, title] = real
        ? ["real process", "A real OS process failed or ran"]
        : mode === "deliberate_corruption"
          ? ["test-only edit", "Files created by the scenario were altered on purpose"]
          : ["simulated", "An error injected inside the running process"];
    return (
        <Badge tone={real ? "info" : "neutral"} title={title}>
            {prefix} · {MODE_LABELS[mode]}
        </Badge>
    );
}

function CaseView({ c }: { c: ReliabilityCase }) {
    return (
        <li className="rounded-md border border-border">
            <details>
                <summary className="flex cursor-pointer flex-wrap items-center gap-2 px-3 py-2 text-sm">
                    <Mono className="min-w-0 break-all">{c.id}</Mono>
                    <ModeBadge mode={c.mode} real={c.real_process} />
                    <Badge tone={verdictTone(c.verdict)}>{VERDICT_LABELS[c.verdict]}</Badge>
                </summary>
                <div className="flex flex-col gap-3 border-t border-border px-3 py-3">
                    <p className="text-sm text-muted">
                        <span className="font-medium text-text">
                            Fault {c.fault_observed ? "observed" : "not observed"}:
                        </span>{" "}
                        {c.fault_evidence}
                    </p>
                    <Table label={`Invariants of ${c.id}`}>
                        <thead>
                            <tr>
                                <Th>Invariant</Th>
                                <Th>Expected</Th>
                                <Th>Observed</Th>
                                <Th>Held</Th>
                            </tr>
                        </thead>
                        <tbody>
                            {c.invariants.map((i) => (
                                <tr key={i.name}>
                                    <Td className="font-mono text-xs">{i.name}</Td>
                                    <Td className="text-xs">{i.expected}</Td>
                                    <Td className="text-xs">{i.observed}</Td>
                                    <Td>
                                        <Badge tone={i.held ? "ok" : "fail"}>{i.held ? "yes" : "no"}</Badge>
                                    </Td>
                                </tr>
                            ))}
                        </tbody>
                    </Table>
                    {c.recovery !== null ? (
                        <p className="break-all text-xs text-muted">
                            Recovery: <Mono>{JSON.stringify(c.recovery)}</Mono>
                        </p>
                    ) : null}
                    {c.integrity !== null ? (
                        <p className="break-all text-xs text-muted">
                            Integrity: <Mono>{JSON.stringify(c.integrity)}</Mono>
                        </p>
                    ) : null}
                </div>
            </details>
        </li>
    );
}

/** A stored reliability experiment. */
export function ReliabilitySummary({ record }: { record: ExperimentRecord }) {
    const result = record.result as unknown as ReliabilityResult | null;
    return (
        <>
            <Card title="Reliability experiment">
                <KeyValue
                    items={[
                        ["Id", <Mono key="id">{record.id}</Mono>],
                        [
                            "Scenario",
                            <Mono key="s">{`${record.spec.scenario ?? "?"} v${record.spec.scenario_version ?? "?"}`}</Mono>,
                        ],
                        ["Parameters", <Mono key="p">{JSON.stringify(record.spec.parameters ?? {})}</Mono>],
                        ["Status", record.status],
                        ["Elapsed", record.elapsed_micros === null ? "—" : formatMicros(record.elapsed_micros)],
                    ]}
                />
                {record.failure ? (
                    <div role="alert" className="mt-4 rounded-md border border-fail/30 bg-fail-soft p-3 text-sm">
                        <p className="font-medium text-fail">{record.failure.kind.replaceAll("_", " ")}</p>
                        <p className="text-text">{record.failure.message}</p>
                        {record.failure.kind === "infrastructure" ? (
                            <p className="mt-1 text-xs text-muted">
                                The experiment could not be carried out. This is not a reliability result.
                            </p>
                        ) : null}
                    </div>
                ) : null}
            </Card>
            {result ? (
                <Card title="Cases">
                    <div className="flex flex-col gap-4">
                        <p className="text-sm text-muted">
                            {result.summary.cases} cases: {result.summary.invariant_preserved} preserved,{" "}
                            {result.summary.invariant_violated} violated, {result.summary.inconclusive} inconclusive;{" "}
                            {result.summary.real_process_cases} involve real OS processes.
                        </p>
                        <ol aria-label="Reliability cases" className="flex flex-col gap-2">
                            {result.cases.map((c) => (
                                <CaseView key={c.id} c={c} />
                            ))}
                        </ol>
                        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
                            <div>
                                <h4 className="mb-1 text-xs font-semibold uppercase tracking-wider text-muted">
                                    Simulates
                                </h4>
                                <ul className="list-disc pl-5 text-sm">
                                    {result.simulates.map((s) => (
                                        <li key={s}>{s}</li>
                                    ))}
                                </ul>
                            </div>
                            <div>
                                <h4 className="mb-1 text-xs font-semibold uppercase tracking-wider text-muted">
                                    Does not simulate
                                </h4>
                                <ul aria-label="Not simulated" className="list-disc pl-5 text-sm text-muted">
                                    {result.does_not_simulate.map((s) => (
                                        <li key={s}>{s}</li>
                                    ))}
                                </ul>
                            </div>
                        </div>
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

/** The supported scenarios. They are run with the CLI, never from this page. */
export function ReliabilityCatalog() {
    const catalog = useApi(() => api.reliabilityScenarios(), "lab:reliability:scenarios");
    return (
        <Card title="Reliability scenarios">
            {catalog.loading && !catalog.data ? <Loading /> : null}
            {catalog.error ? <ErrorState error={catalog.error} onRetry={catalog.reload} /> : null}
            {catalog.data ? (
                <div className="flex flex-col gap-3 text-sm">
                    <p className="text-xs text-muted">
                        These inject faults and kill processes, so the API cannot start them. Run one with:
                    </p>
                    <code className="break-all rounded bg-bg px-2 py-1 font-mono text-xs">
                        cargo run -p orbit-lab --features reliability --bin orbit-reliability -- run &lt;scenario&gt;
                    </code>
                    <ul aria-label="Reliability scenarios" className="flex flex-col gap-2">
                        {catalog.data.scenarios.map((s) => (
                            <li key={s.id}>
                                <Mono>{s.id}</Mono> <span className="text-xs text-faint">v{s.version}</span>
                                <p className="text-xs text-muted">{s.title}</p>
                            </li>
                        ))}
                    </ul>
                </div>
            ) : null}
        </Card>
    );
}
