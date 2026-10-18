"use client";

import { useState } from "react";

import {
    Badge,
    Button,
    Card,
    ErrorState,
    Field,
    Input,
    KeyValue,
    Loading,
    Mono,
    Select,
    Table,
    Td,
    Th,
} from "@/components/ui";
import { api } from "@/lib/api";
import { formatMicros } from "@/lib/format";
import type { ExperimentRecord, FailureMode, ReliabilityCase, ReliabilityResult, ScenarioInfo } from "@/lib/types";
import { useAction, useApi } from "@/lib/use-api";

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
                {record.status === "created" || record.status === "running" ? (
                    <p role="status" className="mt-3 text-sm text-muted">
                        {record.status === "created"
                            ? "Recorded; waiting for the runner process to start it."
                            : "Running in the runner process. This page refreshes until it finishes."}
                    </p>
                ) : null}
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

/**
 * The supported scenarios. When the server has a separately configured
 * runner (`ORBIT_RELIABILITY_RUNNER`), a scenario from this catalog can be
 * launched; the API itself never injects faults.
 */
export function ReliabilityCatalog({ onLaunched }: { onLaunched?: (id: string) => void }) {
    const catalog = useApi(() => api.reliabilityScenarios(), "lab:reliability:scenarios");
    return (
        <Card title="Reliability scenarios">
            {catalog.loading && !catalog.data ? <Loading /> : null}
            {catalog.error ? <ErrorState error={catalog.error} onRetry={catalog.reload} /> : null}
            {catalog.data ? (
                <div className="flex flex-col gap-3 text-sm">
                    <ul aria-label="Reliability scenarios" className="flex flex-col gap-2">
                        {catalog.data.scenarios.map((s) => (
                            <li key={s.id}>
                                <Mono>{s.id}</Mono> <span className="text-xs text-faint">v{s.version}</span>
                                <p className="text-xs text-muted">{s.title}</p>
                            </li>
                        ))}
                    </ul>
                    {catalog.data.runner.enabled ? (
                        <LaunchForm
                            scenarios={catalog.data.scenarios}
                            timeout={catalog.data.runner.timeout_secs}
                            busy={catalog.data.runner.busy}
                            onLaunched={(id) => {
                                catalog.reload();
                                onLaunched?.(id);
                            }}
                        />
                    ) : (
                        <div role="note" className="flex flex-col gap-2 text-xs text-muted">
                            <p>
                                Launching is disabled on this server. Scenarios inject faults and kill processes, so
                                only the separately built <Mono>orbit-reliability</Mono> runner can execute them. To
                                launch from here, start the API with <Mono>ORBIT_RELIABILITY_RUNNER</Mono> set to its
                                absolute path, or run one directly:
                            </p>
                            <code className="break-all rounded bg-bg px-2 py-1 font-mono">
                                cargo run -p orbit-lab --features reliability --bin orbit-reliability -- run
                                &lt;scenario&gt;
                            </code>
                        </div>
                    )}
                </div>
            ) : null}
        </Card>
    );
}

function LaunchForm({
    scenarios,
    timeout,
    busy,
    onLaunched,
}: {
    scenarios: ScenarioInfo[];
    timeout: number;
    busy: boolean;
    onLaunched: (id: string) => void;
}) {
    const [scenarioId, setScenario] = useState(scenarios[0]?.id ?? "");
    const scenario = scenarios.find((s) => s.id === scenarioId) ?? scenarios[0];
    const [values, setValues] = useState<Record<string, string>>({});
    const launch = useAction((spec: { scenario: string; parameters: Record<string, number> }) =>
        api.launchReliability(spec),
    );
    if (!scenario) return null;
    const invalid = scenario.parameters.some((p) => {
        const v = values[`${scenario.id}:${p.name}`];
        return v !== undefined && v !== "" && (!Number.isInteger(Number(v)) || Number(v) < p.min || Number(v) > p.max);
    });
    return (
        <form
            aria-label="Launch a reliability scenario"
            className="flex flex-col gap-3 border-t border-border pt-3"
            onSubmit={async (e) => {
                e.preventDefault();
                const parameters: Record<string, number> = {};
                for (const p of scenario.parameters) {
                    const v = values[`${scenario.id}:${p.name}`];
                    if (v !== undefined && v !== "") parameters[p.name] = Number(v);
                }
                const record = await launch.run({ scenario: scenario.id, parameters });
                if (record) onLaunched(record.id);
            }}
        >
            <Field label="Scenario" htmlFor="rel-scenario" hint={scenario.summary}>
                <Select id="rel-scenario" value={scenario.id} onChange={(e) => setScenario(e.target.value)}>
                    {scenarios.map((s) => (
                        <option key={s.id} value={s.id}>
                            {s.title}
                        </option>
                    ))}
                </Select>
            </Field>
            {scenario.parameters.map((p) => (
                <Field
                    key={p.name}
                    label={`${p.name} (${p.description})`}
                    htmlFor={`rel-${p.name}`}
                    hint={`${p.min}–${p.max}, default ${p.default}`}
                >
                    <Input
                        id={`rel-${p.name}`}
                        type="number"
                        min={p.min}
                        max={p.max}
                        value={values[`${scenario.id}:${p.name}`] ?? ""}
                        onChange={(e) => setValues((v) => ({ ...v, [`${scenario.id}:${p.name}`]: e.target.value }))}
                    />
                </Field>
            ))}
            <p className="text-xs text-muted">
                Runs in the separate runner process, one at a time, stopped after {timeout} s. It works only on files it
                creates under the lab directory.
            </p>
            {invalid ? (
                <p role="alert" className="text-xs text-fail">
                    A value is outside its allowed range.
                </p>
            ) : null}
            {launch.error ? <ErrorState title="Scenario not launched" error={launch.error} /> : null}
            <div>
                <Button type="submit" variant="primary" busy={launch.pending} disabled={invalid || busy}>
                    {busy ? "A scenario is running" : "Launch scenario"}
                </Button>
            </div>
        </form>
    );
}
