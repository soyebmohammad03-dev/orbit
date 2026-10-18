import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ExperimentSummary, LabOutcomeBadge } from "@/app/experiments/view";
import { createClient } from "@/lib/api";
import { labOutcomeTone } from "@/lib/format";
import type { ExperimentRecord } from "@/lib/types";

const digest = (c: string) => c.repeat(64);

function descriptor(id: string, type: string, fingerprint: string) {
    return {
        descriptor_version: 1,
        id,
        name: "Counter",
        projection: { id: "counter", version: 1 },
        adapter: "orbit-core/projection",
        implementation_type: type,
        config: {},
        config_digest: digest("c"),
        determinism: "pure" as const,
        fingerprint,
    };
}

function record(overrides: Partial<ExperimentRecord> = {}): ExperimentRecord {
    const input = {
        stream: "c",
        from: 0,
        to: 3,
        from_event_digest: digest("0"),
        to_event_digest: digest("3"),
        history_fingerprint: digest("h"),
    };
    return {
        id: "0192f3a4-0000-7000-8000-000000000001",
        kind: "differential_replay",
        spec_version: 1,
        spec: { stream: "c", left: "orbit.counter", right: "example.counter-floor-at-zero" },
        spec_digest: digest("s"),
        implementations: [
            { side: "left", descriptor: descriptor("orbit.counter", "orbit_core::Counter", digest("a")) },
            { side: "right", descriptor: descriptor("example.counter-floor-at-zero", "orbit_lab::Floor", digest("b")) },
        ],
        build: { orbit_version: "0.1.0", git_commit: null },
        status: "completed",
        created_at: "2026-10-18T10:00:00.000000Z",
        started_at: "2026-10-18T10:00:00.000000Z",
        finished_at: "2026-10-18T10:00:00.001000Z",
        elapsed_micros: 950,
        input,
        outcome: "diverged",
        result: {
            outcome: "diverged",
            reference: "left",
            left: { implementation: "orbit.counter", fingerprint: digest("a") },
            right: { implementation: "example.counter-floor-at-zero", fingerprint: digest("b") },
            input,
            events_processed: 3,
            events_fed_digest: digest("e"),
            boundaries_compared: 4,
            boundaries_differing: 2,
            first_divergence: {
                boundary: 2,
                event: { sequence: 2, event_id: "e2", event_type: "counter.decremented", schema_version: 1 },
                left: { digest: digest("1"), canonical_bytes: 12 },
                right: { digest: digest("2"), canonical_bytes: 11 },
                equal: false,
                changes: [{ path: "/value", op: "modified", from: -3, to: 0 }],
                changes_total: 1,
                boundaries_matched_before: 2,
            },
            divergent_segments: [{ first: 2, last: 3 }],
            divergent_segments_total: 1,
            final_comparison: {
                boundary: 3,
                event: null,
                left: { digest: digest("4"), canonical_bytes: 12 },
                right: { digest: digest("5"), canonical_bytes: 11 },
                equal: false,
                changes: [{ path: "/value", op: "modified", from: -2, to: 1 }],
                changes_total: 1,
            },
            failures: [],
            determinism: { checked: true, consistent: true, inconsistency: null, declared_uncontrolled: [] },
            reproducibility: { status: "reproducible", reasons: ["input pinned by event digests"] },
        },
        result_digest: digest("r"),
        reproducibility: "reproducible",
        failure: null,
        artifacts: [{ role: "left_trace", digest: digest("t"), media_type: "application/octet-stream", size: 128 }],
        identity: {
            sides: [
                {
                    side: "left",
                    implementation: "orbit.counter",
                    projection: { id: "counter", version: 1 },
                    fingerprint: digest("a"),
                    config_digest: digest("c"),
                },
                {
                    side: "right",
                    implementation: "example.counter-floor-at-zero",
                    projection: { id: "counter", version: 1 },
                    fingerprint: digest("b"),
                    config_digest: digest("c"),
                },
            ],
            build: { orbit_version: "0.1.0", git_commit: null, executable_digest: null },
            completeness: "declared",
            gaps: [
                "fingerprints cover registration metadata and configuration, not code; they do not establish semantic equivalence",
            ],
        },
        ...overrides,
    };
}

describe("ExperimentSummary", () => {
    it("shows both identities, the first divergence and its structural diff", () => {
        render(<ExperimentSummary record={record()} />);
        // The identity row, the first divergence, the final comparison and the
        // (collapsed) entry of interval 1 each label the sides.
        expect(screen.getAllByText("left (expected)")).toHaveLength(4);
        expect(screen.getAllByText("right (observed)")).toHaveLength(4);
        expect(screen.getByText("orbit.counter (counter@1)")).toBeInTheDocument();
        expect(screen.getByText("example.counter-floor-at-zero (counter@1)")).toBeInTheDocument();
        expect(screen.getByText(/First divergence \(after 2 matching/)).toBeInTheDocument();
        const diffs = screen.getAllByRole("list", { name: "State differences" });
        expect(within(diffs[0]!).getByText("/value")).toBeInTheDocument();
        expect(diffs[0]).toHaveTextContent("-3");
        expect(screen.getByText(/3 events fed to both implementations/)).toBeInTheDocument();
        expect(screen.getByText("reproducible")).toBeInTheDocument();
        expect(screen.getByText("left_trace")).toBeInTheDocument();
    });

    it("does not call either side wrong without a reference", () => {
        const r = record();
        render(<ExperimentSummary record={{ ...r, result: { ...r.result!, reference: null } }} />);
        expect(screen.queryByText(/expected/)).not.toBeInTheDocument();
        expect(screen.getAllByText("left").length).toBeGreaterThan(0);
    });

    it("shows a structured failure for a failed experiment", () => {
        render(
            <ExperimentSummary
                record={record({
                    status: "failed",
                    outcome: null,
                    result: null,
                    reproducibility: null,
                    input: null,
                    artifacts: [],
                    failure: { kind: "history_unavailable", message: "stream c does not exist", details: null },
                })}
            />,
        );
        const alert = screen.getByRole("alert");
        expect(alert).toHaveTextContent("history unavailable");
        expect(alert).toHaveTextContent("stream c does not exist");
        expect(screen.getByText("experiment failed")).toBeInTheDocument();
        expect(screen.queryByText("Diagnosis")).not.toBeInTheDocument();
    });
});

describe("lab outcomes", () => {
    it("treats differences as findings and nondeterminism as untrustworthy", () => {
        expect(labOutcomeTone("equivalent")).toBe("ok");
        expect(labOutcomeTone("diverged")).toBe("warn");
        expect(labOutcomeTone("nondeterministic")).toBe("fail");
        render(<LabOutcomeBadge record={{ status: "completed", outcome: "reconverged" }} />);
        expect(screen.getByText("Reconverged")).toBeInTheDocument();
    });

    it("the client posts a differential replay request", async () => {
        const fetch = vi.fn<typeof globalThis.fetch>(
            async () => new Response(JSON.stringify({ id: "x" }), { status: 201 }),
        );
        const api = createClient({ baseUrl: "http://api.test", fetch });
        await api.createDifferential({ stream: "c", left: "a", right: "b" });
        const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
        expect(url).toBe("http://api.test/lab/experiments");
        expect(JSON.parse(init.body as string)).toEqual({
            kind: "differential_replay",
            spec: { stream: "c", left: "a", right: "b" },
        });
        await api.listLabExperiments({ limit: 5, before: "y" });
        expect(fetch.mock.calls[1]?.[0]).toBe("http://api.test/lab/experiments?limit=5&before=y");
    });
});

function sprint8(): ExperimentRecord {
    const base = record();
    const result = base.result!;
    const fd = result.first_divergence!;
    return {
        ...base,
        identity: {
            ...base.identity,
            completeness: "executable",
            build: { ...base.build, executable_digest: digest("x") },
        },
        result: {
            ...result,
            result_version: 2,
            reference: null,
            first_divergence: { ...fd, change_summary: { added: 0, removed: 0, modified: 1, type_changed: 0 } },
            timeline: {
                first_boundary: 0,
                last_boundary: 3,
                intervals: [
                    {
                        index: 1,
                        first: 2,
                        last: 2,
                        boundaries: 1,
                        end: "converged",
                        converged_at: 3,
                        convergence_event: {
                            sequence: 3,
                            event_id: "e3",
                            event_type: "counter.reset",
                            schema_version: 1,
                        },
                        entry: { ...fd, change_summary: { added: 0, removed: 0, modified: 1, type_changed: 0 } },
                        exit: null,
                    },
                ],
                intervals_total: 1,
                intervals_truncated: false,
                convergences: 1,
                final_equal: true,
            },
            diagnosis: {
                evidence: ["The states were equal at 2 compared boundaries and first differ at boundary 2."],
                not_established: ["Why the states differ."],
            },
        },
    };
}

describe("Sprint 8 diagnostics", () => {
    it("shows the timeline, intervals with convergence, and evidence apart from open questions", async () => {
        render(<ExperimentSummary record={sprint8()} />);
        expect(
            screen.getByRole("img", { name: /1 divergence interval between boundaries 0 and 3; 1 convergence/ }),
        ).toBeInTheDocument();
        const intervals = screen.getByRole("list", { name: "Divergence intervals" });
        expect(intervals).toHaveTextContent("Interval 1");
        expect(intervals).toHaveTextContent("boundaries 2–2");
        expect(intervals).toHaveTextContent("converged at 3");
        expect(screen.getByRole("list", { name: "Observed evidence" })).toHaveTextContent("first differ at boundary 2");
        expect(screen.getByRole("list", { name: "Not established" })).toHaveTextContent("Why the states differ");
        expect(screen.getByText("executable digest")).toBeInTheDocument();
        // Peers: no expected/observed labels; diffs read left → right.
        expect(screen.queryByText(/expected/)).not.toBeInTheDocument();
        expect(screen.getAllByText(/Each change reads left → right/).length).toBeGreaterThan(0);
        // Expanding an interval reveals its diff and convergence event.
        await userEvent.click(within(intervals).getByText("Interval 1"));
        expect(within(intervals).getByText("#3 counter.reset")).toBeInTheDocument();
        // Event navigation links point at the Events page.
        const link = screen.getAllByRole("link", { name: "open event" })[0]!;
        expect(link).toHaveAttribute("href", "/events?stream=c&seq=2");
    });

    it("shows Sprint 7 results with their segments, marked as legacy", () => {
        const r = record();
        render(
            <ExperimentSummary
                record={{
                    ...r,
                    result: { ...r.result!, divergent_segments: [{ first: 2, last: 3 }], divergent_segments_total: 1 },
                }}
            />,
        );
        expect(screen.getByText(/Recorded before Sprint 8/)).toBeInTheDocument();
        expect(screen.getByRole("list", { name: "Divergence intervals" })).toHaveTextContent("end not recorded");
        expect(screen.getByText("declared metadata only")).toBeInTheDocument();
    });

    it("loads the trace around the first divergence from the API", async () => {
        const fetch = vi.fn<typeof globalThis.fetch>(
            async () =>
                new Response(
                    JSON.stringify({
                        experiment_id: "x",
                        first_boundary: 0,
                        last_boundary: 3,
                        boundaries_total: 4,
                        entries: [
                            { boundary: 1, left: digest("1"), right: digest("1"), equal: true },
                            { boundary: 2, left: digest("1"), right: digest("2"), equal: false },
                        ],
                        next_from: null,
                    }),
                    { status: 200 },
                ),
        );
        const original = globalThis.fetch;
        globalThis.fetch = fetch;
        try {
            render(<ExperimentSummary record={sprint8()} />);
            await userEvent.click(screen.getByRole("button", { name: "Show trace around it" }));
            const table = await screen.findByRole("table", { name: "Trace" });
            expect(within(table).getByText("differ")).toBeInTheDocument();
            expect(within(table).getByText("equal")).toBeInTheDocument();
            expect(String(fetch.mock.calls[0]?.[0])).toContain("/lab/experiments/");
            expect(String(fetch.mock.calls[0]?.[0])).toContain("/trace?from=0&limit=11");
        } finally {
            globalThis.fetch = original;
        }
    });
});
