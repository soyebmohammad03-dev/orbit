import type { Change, Json } from "@/lib/types";
import { Badge } from "./ui";

function show(value: Json): string {
    return JSON.stringify(value);
}

/** Renders the engine's structural diff: one row per changed JSON Pointer path. */
export function DiffView({ changes }: { changes: Change[] }) {
    if (changes.length === 0) {
        return <p className="text-sm text-muted">No differences: the two states are identical.</p>;
    }
    return (
        <ul aria-label="State differences" className="divide-y divide-border rounded-md border border-border bg-bg">
            {changes.map((change) => (
                <li
                    key={`${change.op}:${change.path}`}
                    className="flex flex-wrap items-start gap-3 px-3 py-2 font-mono text-xs"
                >
                    {change.op === "added" ? (
                        <Badge tone="ok">added</Badge>
                    ) : change.op === "removed" ? (
                        <Badge tone="fail">removed</Badge>
                    ) : (
                        <Badge tone="warn">modified</Badge>
                    )}
                    <span className="text-text">{change.path === "" ? "(root)" : change.path}</span>
                    <span className="min-w-0 flex-1 break-all text-muted">
                        {change.op === "modified" ? (
                            <>
                                <span className="text-fail/90 line-through decoration-fail/40">
                                    {show(change.from)}
                                </span>
                                <span className="mx-2 text-faint">→</span>
                                <span className="text-ok">{show(change.to)}</span>
                            </>
                        ) : change.op === "added" ? (
                            <span className="text-ok">{show(change.value)}</span>
                        ) : (
                            <span className="text-fail/90">{show(change.value)}</span>
                        )}
                    </span>
                </li>
            ))}
        </ul>
    );
}
