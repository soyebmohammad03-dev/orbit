"use client";

import { usePathname, useRouter, useSearchParams } from "next/navigation";
import { useCallback } from "react";

import { api } from "@/lib/api";
import { useApi } from "@/lib/use-api";
import { Select } from "./ui";

/** Reads and writes `?stream=` (and optionally other params) on the current page. */
export function useQueryParam(name: string): [string | null, (value: string | null, reset?: string[]) => void] {
    const params = useSearchParams();
    const router = useRouter();
    const pathname = usePathname();
    const value = params.get(name);
    const set = useCallback(
        (next: string | null, reset: string[] = []) => {
            const updated = new URLSearchParams(params.toString());
            if (next === null || next === "") updated.delete(name);
            else updated.set(name, next);
            for (const r of reset) updated.delete(r);
            const qs = updated.toString();
            router.replace(qs ? `${pathname}?${qs}` : pathname, { scroll: false });
        },
        [params, router, pathname, name],
    );
    return [value, set];
}

/** A select of the store's streams (first page of up to 500). */
export function StreamPicker({
    value,
    onChange,
    id = "stream-picker",
}: {
    value: string | null;
    onChange: (id: string) => void;
    id?: string;
}) {
    const streams = useApi(() => api.listStreams({ limit: 500 }), "streams");
    const options = streams.data?.streams ?? [];
    return (
        <div className="flex items-center gap-2">
            <label htmlFor={id} className="text-xs font-medium text-muted">
                Stream
            </label>
            <Select
                id={id}
                value={value ?? ""}
                onChange={(e) => onChange(e.target.value)}
                disabled={streams.loading || options.length === 0}
                className="min-w-56"
            >
                <option value="" disabled>
                    {streams.loading ? "Loading streams…" : options.length === 0 ? "No streams yet" : "Select a stream"}
                </option>
                {options.map((s) => (
                    <option key={s.stream_id} value={s.stream_id}>
                        {s.stream_id} ({s.head_sequence} events)
                    </option>
                ))}
            </Select>
        </div>
    );
}

/** A select of the API's registered projections. */
export function ProjectionPicker({
    value,
    onChange,
    id = "projection-picker",
}: {
    value: string;
    onChange: (key: string) => void;
    id?: string;
}) {
    const caps = useApi(() => api.capabilities(), "capabilities");
    return (
        <div className="flex items-center gap-2">
            <label htmlFor={id} className="text-xs font-medium text-muted">
                Projection
            </label>
            <Select id={id} value={value} onChange={(e) => onChange(e.target.value)} disabled={!caps.data}>
                {(caps.data?.projections ?? [{ key: value, id: value, version: 1, description: "" }]).map((p) => (
                    <option key={p.key} value={p.id} title={p.description}>
                        {p.key}
                    </option>
                ))}
            </Select>
        </div>
    );
}
