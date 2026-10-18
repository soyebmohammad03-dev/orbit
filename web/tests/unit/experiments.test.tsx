import { render, screen, within } from "@testing-library/react";
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
        ...overrides,
    };
}

describe("ExperimentSummary", () => {
    it("shows both identities, the first divergence and its structural diff", () => {
        render(<ExperimentSummary record={record()} />);
        // Identity rows plus both comparisons label the sides.
        expect(screen.getAllByText("left (expected)")).toHaveLength(3);
        expect(screen.getAllByText("right (observed)")).toHaveLength(3);
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
