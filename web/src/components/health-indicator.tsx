"use client";

import { useEffect, useState } from "react";

import { api, ApiError } from "@/lib/api";
import type { HealthResponse } from "@/lib/types";

type HealthState = { kind: "checking" } | { kind: "ok"; health: HealthResponse } | { kind: "down"; message: string };

/** Polls `/health` every 10 s; shows exactly what the API last reported. */
export function HealthIndicator() {
    const [state, setState] = useState<HealthState>({ kind: "checking" });
    useEffect(() => {
        let alive = true;
        const check = () =>
            api.health().then(
                (health) => alive && setState({ kind: "ok", health }),
                (e: unknown) =>
                    alive && setState({ kind: "down", message: e instanceof ApiError ? e.message : String(e) }),
            );
        void check();
        const timer = setInterval(check, 10_000);
        return () => {
            alive = false;
            clearInterval(timer);
        };
    }, []);

    const dot = state.kind === "ok" ? "bg-ok" : state.kind === "down" ? "bg-fail" : "bg-faint";
    const text =
        state.kind === "ok"
            ? `API ${state.health.api_version} · engine ${state.health.engine_version} · schema v${state.health.database.schema_version}`
            : state.kind === "down"
              ? "API unreachable"
              : "Checking API…";
    return (
        <div
            role="status"
            aria-live="polite"
            title={state.kind === "down" ? state.message : undefined}
            className="flex items-center gap-2 text-xs text-muted"
        >
            <span aria-hidden className={`inline-block size-2 rounded-full ${dot}`} />
            <span>{text}</span>
        </div>
    );
}
