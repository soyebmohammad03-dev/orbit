import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { ReliabilitySummary } from "@/app/experiments/reliability";
import type { ExperimentRecord, ReliabilityCase, ReliabilityResult } from "@/lib/types";

const d = (c: string) => c.repeat(64);

function reliabilityCase(over: Partial<ReliabilityCase>): ReliabilityCase {
    return {
        id: "append_after_insert/process_abort",
        fault_point: "AppendAfterInsert",
        mode: "process_abort",
        real_process: true,
        fault_observed: true,
        fault_evidence: "child terminated by SIGABRT (6)",
        invariants: [{ name: "nothing_committed", expected: "head stays at 5", observed: "head is 5", held: true }],
        recovery: { reopened: true },
        integrity: { valid: true },
        verdict: "invariant_preserved",
        ...over,
    };
}

function record(result: ReliabilityResult | null, over: Partial<ExperimentRecord> = {}): ExperimentRecord {
    return {
        id: "0192f3a4-0000-7000-8000-000000000009",
        kind: "reliability",
        spec_version: 1,
        spec: {
            stream: undefined as unknown as string,
            left: undefined as unknown as string,
            right: undefined as unknown as string,
            scenario: "event-append-interruption",
            scenario_version: 1,
            parameters: { events: 5 },
        },
        spec_digest: d("s"),
        implementations: [],
        build: { orbit_version: "0.1.0", git_commit: null, executable_digest: null },
        status: result ? "completed" : "failed",
        created_at: "2026-10-18T10:00:00.000000Z",
        started_at: "2026-10-18T10:00:00.000000Z",
        finished_at: "2026-10-18T10:00:01.000000Z",
        elapsed_micros: 1_000_000,
        input: null,
        outcome: result?.verdict ?? null,
        result: result as never,
        result_digest: result ? d("r") : null,
        reproducibility: result ? "unverified" : null,
        failure: null,
        artifacts: [],
        identity: {
            sides: [],
            build: { orbit_version: "0.1.0", git_commit: null },
            completeness: "declared",
            gaps: [],
        },
        ...over,
    };
}

function result(cases: ReliabilityCase[]): ReliabilityResult {
    const count = (v: ReliabilityCase["verdict"]) => cases.filter((c) => c.verdict === v).length;
    return {
        result_version: 1,
        scenario: { id: "event-append-interruption", version: 1, title: "Interrupted event appends" },
        parameters: { events: 5 },
        history: { events: 5, requests_digest: d("h") },
        cases,
        summary: {
            cases: cases.length,
            invariant_preserved: count("invariant_preserved"),
            invariant_violated: count("invariant_violated"),
            inconclusive: count("inconclusive"),
            real_process_cases: cases.filter((c) => c.real_process).length,
        },
        verdict: count("invariant_violated") > 0 ? "invariant_violated" : "invariants_preserved",
        simulates: ["a process abort at a transaction stage"],
        does_not_simulate: ["power loss or an operating-system crash"],
        limitations: ["passing cases do not prove correctness for all failures"],
    };
}

describe("ReliabilitySummary", () => {
    it("labels simulated and real failures and shows expected versus observed invariants", async () => {
        render(
            <ReliabilitySummary
                record={record(
                    result([
                        reliabilityCase({}),
                        reliabilityCase({
                            id: "append_after_insert/simulated_error",
                            mode: "simulated_error",
                            real_process: false,
                            fault_evidence: "append returned InjectedFault",
                        }),
                    ]),
                )}
            />,
        );
        const cases = screen.getByRole("list", { name: "Reliability cases" });
        expect(cases).toHaveTextContent("real process · process abort");
        expect(cases).toHaveTextContent("simulated · simulated error");
        expect(screen.getByText(/1 involve real OS processes/)).toBeInTheDocument();
        await userEvent.click(within(cases).getByText("append_after_insert/process_abort"));
        const table = screen.getByRole("table", { name: "Invariants of append_after_insert/process_abort" });
        expect(table).toHaveTextContent("head stays at 5");
        expect(table).toHaveTextContent("head is 5");
        expect(screen.getByRole("list", { name: "Not simulated" })).toHaveTextContent("power loss");
    });

    it("shows violated and inconclusive cases as such", () => {
        render(
            <ReliabilitySummary
                record={record(
                    result([
                        reliabilityCase({
                            verdict: "invariant_violated",
                            invariants: [
                                { name: "nothing_committed", expected: "head 5", observed: "head 6", held: false },
                            ],
                        }),
                        reliabilityCase({ id: "x/process_abort", fault_observed: false, verdict: "inconclusive" }),
                    ]),
                )}
            />,
        );
        const cases = screen.getByRole("list", { name: "Reliability cases" });
        expect(within(cases).getByText("violated")).toBeInTheDocument();
        expect(within(cases).getByText("inconclusive")).toBeInTheDocument();
        expect(screen.getByText(/1 violated, 1 inconclusive/)).toBeInTheDocument();
    });

    it("never presents an infrastructure failure as a result", () => {
        render(
            <ReliabilitySummary
                record={record(null, {
                    failure: { kind: "infrastructure", message: "cannot start child process", details: null },
                })}
            />,
        );
        const alert = screen.getByRole("alert");
        expect(alert).toHaveTextContent("infrastructure");
        expect(alert).toHaveTextContent("This is not a reliability result.");
        expect(screen.queryByRole("list", { name: "Reliability cases" })).not.toBeInTheDocument();
    });
});
