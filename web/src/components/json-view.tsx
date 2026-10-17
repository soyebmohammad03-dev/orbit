import type { Json } from "@/lib/types";

/** Pretty-printed JSON in a scrollable, selectable block. */
export function JsonView({
    value,
    label,
    maxHeight = "28rem",
}: {
    value: Json | undefined;
    label: string;
    maxHeight?: string;
}) {
    return (
        <pre
            aria-label={label}
            tabIndex={0}
            style={{ maxHeight }}
            className="overflow-auto rounded-md border border-border bg-bg p-3 font-mono text-xs leading-relaxed text-text"
        >
            {value === undefined ? "—" : JSON.stringify(value, null, 2)}
        </pre>
    );
}
