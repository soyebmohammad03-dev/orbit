import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { DiffView } from "@/components/diff-view";
import { isActive } from "@/components/nav";
import { EmptyState, ErrorState } from "@/components/ui";

describe("DiffView", () => {
    it("renders one row per change with its operation and values", () => {
        render(
            <DiffView
                changes={[
                    { path: "/items/A", op: "removed", value: 1 },
                    { path: "/status", op: "modified", from: "new", to: "paid" },
                    { path: "/total", op: "added", value: 30 },
                ]}
            />,
        );
        const rows = screen.getAllByRole("listitem");
        expect(rows).toHaveLength(3);
        expect(rows[0]).toHaveTextContent("removed");
        expect(rows[0]).toHaveTextContent("/items/A");
        expect(rows[1]).toHaveTextContent('"new"');
        expect(rows[1]).toHaveTextContent('"paid"');
        expect(rows[2]).toHaveTextContent("added");
    });

    it("says so when states are identical", () => {
        render(<DiffView changes={[]} />);
        expect(screen.getByText(/identical/)).toBeInTheDocument();
    });

    it("labels the root path", () => {
        render(<DiffView changes={[{ path: "", op: "modified", from: 1, to: 2 }]} />);
        expect(screen.getByText("(root)")).toBeInTheDocument();
    });
});

describe("ErrorState and EmptyState", () => {
    it("shows the error code, status and message, and retries", async () => {
        const retry = vi.fn();
        render(
            <ErrorState
                error={{ message: "stream x does not exist", code: "stream_not_found", status: 404 }}
                onRetry={retry}
            />,
        );
        const alert = screen.getByRole("alert");
        expect(alert).toHaveTextContent("stream_not_found");
        expect(alert).toHaveTextContent("HTTP 404");
        expect(alert).toHaveTextContent("stream x does not exist");
        await userEvent.click(screen.getByRole("button", { name: "Retry" }));
        expect(retry).toHaveBeenCalledOnce();
    });

    it("renders an empty state with its explanation", () => {
        render(<EmptyState title="No snapshots">Create one.</EmptyState>);
        expect(screen.getByText("No snapshots")).toBeInTheDocument();
        expect(screen.getByText("Create one.")).toBeInTheDocument();
    });
});

describe("navigation", () => {
    it("marks the active section", () => {
        expect(isActive("/", "/")).toBe(true);
        expect(isActive("/streams", "/")).toBe(false);
        expect(isActive("/replay", "/replay")).toBe(true);
        expect(isActive("/replayx", "/replay")).toBe(false);
    });
});
