import { render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ValidationReportView } from "@/app/evidence/view";
import { ComparisonView, PerformanceSummary } from "@/components/performance";
import { createClient } from "@/lib/api";
import { formatPercent, reproductionTone, verdictTone } from "@/lib/format";
import type { Comparison, ExperimentRecord, PerformanceResult, ValidationReport } from "@/lib/types";

const d = (c: string) => c.repeat(64);
const build = { orbit_version: "0.1.0", git_commit: null, executable_digest: null };

function perfResult(over: Partial<PerformanceResult> = {}): PerformanceResult {
    return {
        result_version: 1,
        workload: {
            id: "integrity-verification",
            version: 1,
            title: "Integrity verification",
            iteration: "verify the digest chain of the whole stream",
            input_unit: "events",
        },
        configuration: { workload: "integrity-verification", input_size: 100, measured_iterations: 3 },
        configuration_digest: d("c"),
        environment: { os: "linux", arch: "x86_64", available_parallelism: 4, build_profile: "debug", build },
        evidence_level: "smoke",
        evidence_notes: ["unoptimized (debug) build", "3 measured iterations; a measurement needs at least 10"],
        unit: "nanoseconds per iteration",
        warmup_samples_ns: [],
        samples_ns: [1000, 2000, 3000],
        summary: {
            count: 3,
            mean: 2000,
            median: 2000,
            std_dev: 1000,
            mad: 1000,
            coefficient_of_variation: 0.5,
            min: 1000,
            max: 3000,
            percentiles: [],
            mean_ci: null,
            median_ci: null,
            notes: ["no interval of the mean below 10 samples"],
        },
        throughput_per_second_at_median: 50_000_000,
        limitations: ["one run in one process on one machine"],
        ...over,
    };
}

function record(result: PerformanceResult | null, over: Partial<ExperimentRecord> = {}): ExperimentRecord {
    return {
        id: "0192f3a4-0000-7000-8000-00000000000a",
        kind: "performance",
        spec_version: 1,
        spec: {
            workload: "integrity-verification",
            workload_version: 1,
            input_size: 100,
            warmup_iterations: 0,
            measured_iterations: 3,
            seed: 1,
        } as unknown as ExperimentRecord["spec"],
        spec_digest: d("s"),
        implementations: [],
        build,
        status: result ? "completed" : "running",
        created_at: "2026-10-18T10:00:00Z",
        started_at: "2026-10-18T10:00:00Z",
        finished_at: result ? "2026-10-18T10:00:01Z" : null,
        elapsed_micros: result ? 1000 : null,
        input: null,
        outcome: result?.evidence_level ?? null,
        result: result as unknown as ExperimentRecord["result"],
        result_digest: result ? d("r") : null,
        reproducibility: result ? "unverified" : null,
        failure: null,
        artifacts: [],
        identity: {
            completeness: "declared",
            declared: [],
            configuration: null,
            build,
        } as unknown as ExperimentRecord["identity"],
        ...over,
    };
}

describe("performance results", () => {
    it("labels smoke runs and shows only the statistics that were computed", () => {
        render(<PerformanceSummary record={record(perfResult())} />);
        expect(screen.getByText("Smoke run")).toBeInTheDocument();
        expect(screen.getByRole("note")).toHaveTextContent("It shows that the workload runs, not how fast it is.");
        const table = screen.getByRole("table", { name: "Statistics" });
        expect(within(table).getByText("Samples").nextSibling).toHaveTextContent("3");
        // No interval was computed: shown as missing, never invented.
        expect(within(table).getByText("95 % interval of the mean (Student t)").nextSibling).toHaveTextContent("—");
        expect(screen.getByRole("img", { name: /3 measured iterations/ })).toBeInTheDocument();
        expect(screen.getByRole("list", { name: "Not computed" })).toHaveTextContent("no interval of the mean");
    });

    it("says when a measurement was taken inside the API process, and shows its overhead", () => {
        const base = perfResult();
        render(
            <PerformanceSummary
                record={record({
                    ...base,
                    environment: { ...base.environment, process: "orbit-api" },
                    overhead: { prepare_ns: 2_000_000, iterations_wall_ns: 9_000, untimed_ns: 3_000 },
                })}
            />,
        );
        expect(screen.getByText(/Inside the API server, which also serves requests/)).toBeInTheDocument();
        expect(screen.getByText(/untimed setup and checks 3 µs of 9 µs/)).toBeInTheDocument();
    });

    it("says a pending measurement runs in the background and cannot be cancelled", () => {
        render(<PerformanceSummary record={record(null)} />);
        expect(screen.getByRole("status")).toHaveTextContent("cannot be cancelled");
        expect(screen.queryByRole("table")).not.toBeInTheDocument();
    });
});

function comparison(over: Partial<Comparison>): Comparison {
    return {
        verdict: "suspected_regression",
        compatibility: { incompatible: [], warnings: [] },
        baseline_median_ns: 1000,
        candidate_median_ns: 1200,
        absolute_change_ns: 200,
        relative_change: 0.2,
        median_intervals: null,
        median_intervals_overlap: true,
        test: { u: 74, z: 0.09, p_value: 0.925, method: "mann_whitney_u_normal_approximation" },
        policy: { threshold_percent: 10, alpha: 0.01 },
        explanation: "the median changed by +20.00 %, but the test is not significant",
        ...over,
    };
}

describe("comparisons", () => {
    it("never presents a suspected change as established", () => {
        render(<ComparisonView comparison={comparison({})} />);
        expect(screen.getByText("Suspected regression (not statistically supported)")).toBeInTheDocument();
        expect(screen.getByText(/\+20\.0 %/)).toBeInTheDocument();
        expect(screen.getByText(/p = 9\.25e-1/)).toBeInTheDocument();
    });

    it("shows why incompatible results were not compared", () => {
        render(
            <ComparisonView
                comparison={comparison({
                    verdict: "incompatible",
                    relative_change: null,
                    test: null,
                    compatibility: { incompatible: ["seed: Some(1) vs Some(2)"], warnings: [] },
                    explanation: "not compared: seed: Some(1) vs Some(2)",
                })}
            />,
        );
        expect(screen.getByText("Incompatible: not compared")).toBeInTheDocument();
        expect(screen.getByRole("list", { name: "Incompatible" })).toHaveTextContent("seed");
        expect(screen.queryByText(/median .* →/)).not.toBeInTheDocument();
    });

    it("maps verdicts and statuses to tones", () => {
        expect(verdictTone("regression")).toBe("fail");
        expect(verdictTone("suspected_regression")).toBe("warn");
        expect(verdictTone("within_threshold")).toBe("ok");
        expect(reproductionTone("reproduced")).toBe("ok");
        expect(reproductionTone("artifact_missing")).toBe("fail");
        expect(reproductionTone("unsupported")).toBe("warn");
        expect(formatPercent(0.1234)).toBe("+12.3 %");
        expect(formatPercent(-0.05)).toBe("-5.0 %");
        expect(formatPercent(null)).toBe("—");
    });
});

function report(over: Partial<ValidationReport>): ValidationReport {
    return {
        valid: true,
        bundle_digest: d("b"),
        format_version: 1,
        experiment_id: "0192f3a4-0000-7000-8000-00000000000b",
        kind: "differential_replay",
        status: "completed",
        outcome: "diverged",
        errors: [],
        passed: ["structure", "entries", "history_chain"],
        integrity: "verified",
        scientific_correctness: "not_verified",
        history: "included_and_verified",
        independently_replayable: true,
        replayability: "the verified input history is included",
        authenticity: {
            status: "unsigned",
            trusted_keys: [],
            untrusted_keys: [],
            trust_store_configured: false,
            note: "unsigned: who produced this bundle is not established",
        },
        limitations: ["a valid bundle is not signed"],
        ...over,
    };
}

describe("bundle validation reports", () => {
    it("separates integrity from scientific correctness", () => {
        render(<ValidationReportView report={report({})} />);
        expect(screen.getByText("integrity verified")).toBeInTheDocument();
        expect(screen.getByText("scientific correctness not verified")).toBeInTheDocument();
        expect(screen.getByText(/Replayable from the bundle/)).toBeInTheDocument();
        expect(screen.getByText("unsigned: origin not established")).toBeInTheDocument();
    });

    it("never presents an untrusted signature as trusted", () => {
        render(
            <ValidationReportView
                report={report({
                    authenticity: {
                        status: "signed_untrusted",
                        trusted_keys: [],
                        untrusted_keys: ["someone"],
                        trust_store_configured: true,
                        note: "the signatures are valid, but no signing key is trusted here",
                    },
                })}
            />,
        );
        expect(screen.getByText("signed, by a key not trusted here")).toBeInTheDocument();
        expect(screen.getByText(/Key ids: someone/)).toBeInTheDocument();
        expect(screen.queryByText("signed by a trusted key")).not.toBeInTheDocument();
    });

    it("lists every problem with its code", () => {
        render(
            <ValidationReportView
                report={report({
                    valid: false,
                    integrity: "failed",
                    history: "invalid",
                    errors: [
                        { code: "unsafe_entry_name", message: 'entry name "../x" is not allowed' },
                        { code: "digest_mismatch", message: "artifacts/left_trace: content does not match" },
                    ],
                })}
            />,
        );
        expect(screen.getByText("invalid")).toBeInTheDocument();
        const errors = screen.getByRole("list", { name: "Validation errors" });
        expect(within(errors).getAllByRole("listitem")).toHaveLength(2);
        expect(errors).toHaveTextContent("unsafe_entry_name");
        expect(screen.queryByText(/Replayable/)).not.toBeInTheDocument();
    });
});

describe("api client: bundles and performance", () => {
    it("posts bundle files unchanged and builds export URLs", async () => {
        const fetch = vi.fn<typeof globalThis.fetch>(
            async () => new Response(JSON.stringify(report({})), { status: 200 }),
        );
        const api = createClient({ baseUrl: "http://api.test/api/v1", fetch });
        const raw = '{"format":"orbit-evidence-bundle" }';
        await api.validateBundle(raw);
        const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
        expect(url).toBe("http://api.test/api/v1/lab/bundles/validate");
        expect(init.body).toBe(raw);
        expect(api.bundleUrl("a/b")).toBe("http://api.test/api/v1/lab/experiments/a%2Fb/bundle");
        expect(api.bundleUrl("x", false)).toBe("http://api.test/api/v1/lab/experiments/x/bundle?history=false");
    });

    it("sends performance specs with their kind", async () => {
        const fetch = vi.fn<typeof globalThis.fetch>(async () => new Response("{}", { status: 202 }));
        const api = createClient({ baseUrl: "http://api.test", fetch });
        await api.createPerformance({ workload: "event-append", input_size: 10 });
        await api.listLabExperiments({ kind: "performance", limit: 5 });
        const [, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
        expect(JSON.parse(init.body as string)).toEqual({
            kind: "performance",
            spec: { workload: "event-append", input_size: 10 },
        });
        expect(fetch.mock.calls[1][0]).toBe("http://api.test/lab/experiments?kind=performance&limit=5");
    });
});
