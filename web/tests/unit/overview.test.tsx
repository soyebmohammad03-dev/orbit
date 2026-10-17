import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "@/lib/api";
import type { HealthResponse, OverviewResponse } from "@/lib/types";

const mocks = vi.hoisted(() => ({
    overview: vi.fn<() => Promise<OverviewResponse>>(),
    health: vi.fn<() => Promise<HealthResponse>>(),
    verifyStore: vi.fn(),
}));

vi.mock("@/lib/api", async (importOriginal) => {
    const actual = await importOriginal<typeof import("@/lib/api")>();
    return { ...actual, api: { ...actual.api, ...mocks } };
});

const { OverviewView } = await import("@/app/overview-view");

const health: HealthResponse = {
    status: "ok",
    api_version: "v1",
    engine_version: "0.1.0",
    database: { reachable: true, schema_version: 3 },
};

const empty: OverviewResponse = {
    statistics: { streams: 0, events: 0, snapshots: 0, replay_runs: 0, replay_executions: 0, schema_version: 3 },
    recent_events: [],
    active_streams: [],
};

describe("Overview page", () => {
    beforeEach(() => {
        mocks.overview.mockReset();
        mocks.health.mockReset();
        mocks.verifyStore.mockReset();
        mocks.health.mockResolvedValue(health);
    });

    it("shows loading, then real counts and the empty-store guidance", async () => {
        let resolve!: (v: OverviewResponse) => void;
        mocks.overview.mockReturnValue(new Promise((r) => (resolve = r)));
        render(<OverviewView />);
        expect(screen.getByRole("status")).toHaveTextContent("Loading overview");
        resolve(empty);
        expect(await screen.findByText("This store is empty.")).toBeInTheDocument();
        expect(screen.getByText(/Not run in this session/)).toBeInTheDocument();
    });

    it("renders active streams and recent events from the API", async () => {
        mocks.overview.mockResolvedValue({
            statistics: {
                streams: 1,
                events: 2,
                snapshots: 1,
                replay_runs: 0,
                replay_executions: 0,
                schema_version: 3,
            },
            active_streams: [
                {
                    stream_id: "orders-1",
                    created_at: "2026-01-01T00:00:00Z",
                    head_sequence: 2,
                    head_digest: "ab".repeat(32),
                    last_recorded_at: "2026-01-01T00:00:01Z",
                    snapshot_count: 1,
                    replay_run_count: 0,
                },
            ],
            recent_events: [
                {
                    event_id: "e1",
                    stream_id: "orders-1",
                    sequence: 2,
                    event_type: "order.paid",
                    schema_version: 1,
                    recorded_at: "2026-01-01T00:00:01Z",
                    payload: { status: "paid" },
                    metadata: {},
                    prev_digest: "0".repeat(64),
                    digest: "cd".repeat(32),
                },
            ],
        });
        render(<OverviewView />);
        expect(await screen.findByRole("table", { name: "Active streams" })).toHaveTextContent("orders-1");
        expect(screen.getByRole("table", { name: "Recent events" })).toHaveTextContent("order.paid");
        expect(screen.queryByText("This store is empty.")).not.toBeInTheDocument();
    });

    it("shows API errors with a working retry", async () => {
        mocks.health.mockResolvedValue(health);
        mocks.overview.mockRejectedValueOnce(
            new ApiError(500, { code: "database_error", message: "disk I/O error", class: "database", details: null }),
        );
        mocks.overview.mockResolvedValueOnce(empty);
        render(<OverviewView />);
        const alert = await screen.findByRole("alert");
        expect(alert).toHaveTextContent("disk I/O error");
        await userEvent.click(screen.getByRole("button", { name: "Retry" }));
        expect(await screen.findByText("This store is empty.")).toBeInTheDocument();
        expect(mocks.overview).toHaveBeenCalledTimes(2);
    });

    it("only reports integrity results after verification actually ran", async () => {
        mocks.overview.mockResolvedValue(empty);
        mocks.verifyStore.mockResolvedValue({
            valid: false,
            schema_version: 3,
            sqlite_integrity: ["ok"],
            foreign_key_violations: 0,
            missing_safeguards: ["events_no_update"],
            streams: [],
            snapshots_checked: 0,
            snapshot_issues: [],
            replay_runs_checked: 0,
            replay_run_issues: [],
            elapsed_micros: 900,
        });
        render(<OverviewView />);
        await screen.findByText(/Not run in this session/);
        expect(screen.queryByText("valid")).not.toBeInTheDocument();
        await userEvent.click(screen.getByRole("button", { name: "Run verification" }));
        await waitFor(() => expect(screen.getByText("issues found")).toBeInTheDocument());
        expect(mocks.verifyStore).toHaveBeenCalledOnce();
    });
});
