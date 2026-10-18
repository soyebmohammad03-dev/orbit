import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ReliabilityCatalogResponse } from "@/lib/types";

const calls = vi.hoisted(() => ({
    catalog: null as ReliabilityCatalogResponse | null,
    launched: [] as unknown[],
}));

vi.mock("@/lib/api", async (original) => {
    const actual = await original<typeof import("@/lib/api")>();
    return {
        ...actual,
        api: {
            ...actual.api,
            reliabilityScenarios: async () => calls.catalog,
            launchReliability: async (spec: unknown) => {
                calls.launched.push(spec);
                return { id: "0192f3a4-0000-7000-8000-00000000000c", status: "created" };
            },
        },
    };
});

import { ReliabilityCatalog } from "@/app/experiments/reliability";

const scenario = {
    id: "snapshot-write-interruption",
    version: 1,
    title: "Interrupted snapshot writes",
    summary: "Saving a snapshot is interrupted.",
    simulates: [],
    does_not_simulate: [],
    invariants: [],
    parameters: [{ name: "events", description: "events committed before the fault", default: 10, min: 1, max: 500 }],
};

beforeEach(() => {
    calls.launched = [];
});

describe("reliability catalog", () => {
    it("explains that launching is disabled and offers no launch control", async () => {
        calls.catalog = {
            scenarios: [scenario],
            runner: { enabled: false, reason: "disabled" },
            execution: "cli",
        };
        render(<ReliabilityCatalog />);
        expect(await screen.findByRole("note")).toHaveTextContent("Launching is disabled on this server");
        expect(screen.queryByRole("button", { name: /launch/i })).not.toBeInTheDocument();
    });

    it("launches a catalog scenario with bounded parameters only", async () => {
        calls.catalog = {
            scenarios: [scenario],
            runner: { enabled: true, timeout_secs: 900, busy: false },
            execution: "cli",
        };
        const launched = vi.fn();
        render(<ReliabilityCatalog onLaunched={launched} />);
        const input = await screen.findByLabelText(/events/);
        await userEvent.type(input, "9999");
        expect(screen.getByRole("alert")).toHaveTextContent("outside its allowed range");
        expect(screen.getByRole("button", { name: "Launch scenario" })).toBeDisabled();
        await userEvent.clear(input);
        await userEvent.type(input, "3");
        await userEvent.click(screen.getByRole("button", { name: "Launch scenario" }));
        expect(calls.launched).toEqual([{ scenario: "snapshot-write-interruption", parameters: { events: 3 } }]);
        expect(launched).toHaveBeenCalledWith("0192f3a4-0000-7000-8000-00000000000c");
        expect(screen.getByText(/stopped after 900 s/)).toBeInTheDocument();
    });

    it("blocks a second launch while one is running", async () => {
        calls.catalog = {
            scenarios: [scenario],
            runner: { enabled: true, timeout_secs: 900, busy: true },
            execution: "cli",
        };
        render(<ReliabilityCatalog />);
        expect(await screen.findByRole("button", { name: "A scenario is running" })).toBeDisabled();
    });
});
