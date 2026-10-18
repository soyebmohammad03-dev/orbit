/** Shortens a 64-char hex digest to `abcd1234…ef90`. */
export function shortDigest(hex: string, head = 8, tail = 4): string {
    if (hex.length <= head + tail + 1) return hex;
    return `${hex.slice(0, head)}…${hex.slice(-tail)}`;
}

/** `true` for the all-zero genesis digest. */
export function isGenesis(hex: string): boolean {
    return /^0+$/.test(hex);
}

/** Formats a duration given in microseconds. */
export function formatMicros(micros: number): string {
    if (!Number.isFinite(micros)) return "—";
    if (micros < 1000) return `${micros} µs`;
    if (micros < 1_000_000) return `${(micros / 1000).toFixed(micros < 10_000 ? 2 : 1)} ms`;
    return `${(micros / 1_000_000).toFixed(2)} s`;
}

/** Formats a duration given in nanoseconds (benchmark results). */
export function formatNanos(nanos: number): string {
    if (!Number.isFinite(nanos)) return "—";
    if (nanos < 1000) return `${nanos.toFixed(0)} ns`;
    return formatMicros(Math.round(nanos / 10) / 100);
}

/** Formats an RFC 3339 timestamp as local date and time (UTC suffix kept explicit). */
export function formatTimestamp(rfc3339: string | null | undefined): string {
    if (!rfc3339) return "—";
    const date = new Date(rfc3339);
    if (Number.isNaN(date.getTime())) return rfc3339;
    return date
        .toISOString()
        .replace("T", " ")
        .replace(/\.\d+Z$/, " UTC");
}

/** "3 min ago" style relative time against `now`. */
export function relativeTime(rfc3339: string | null | undefined, now: Date = new Date()): string {
    if (!rfc3339) return "never";
    const then = new Date(rfc3339).getTime();
    if (Number.isNaN(then)) return rfc3339;
    const seconds = Math.round((now.getTime() - then) / 1000);
    if (seconds < 5) return "just now";
    if (seconds < 60) return `${seconds}s ago`;
    const minutes = Math.round(seconds / 60);
    if (minutes < 60) return `${minutes} min ago`;
    const hours = Math.round(minutes / 60);
    if (hours < 48) return `${hours} h ago`;
    return `${Math.round(hours / 24)} d ago`;
}

/** Compact one-line JSON for tables. */
export function compactJson(value: unknown, max = 80): string {
    const text = JSON.stringify(value) ?? "undefined";
    return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

/** Human labels for replay outcome statuses. */
export const OUTCOME_LABELS: Record<string, string> = {
    exact_match: "Exact match",
    reconverged: "Reconverged",
    diverged: "Diverged",
    projection_failed: "Projection failed",
    unavailable: "Unavailable",
    invalid_input: "Invalid input",
    integrity_failure: "Integrity failure",
};

export type Tone = "neutral" | "ok" | "warn" | "fail" | "info";

/** Status tone for a replay outcome label. */
export function outcomeTone(status: string | null | undefined): Tone {
    switch (status) {
        case "exact_match":
            return "ok";
        case "reconverged":
            return "warn";
        case "diverged":
        case "projection_failed":
        case "integrity_failure":
            return "fail";
        case "unavailable":
        case "invalid_input":
            return "warn";
        default:
            return "neutral";
    }
}

/** Labels of differential-replay outcomes (orbit-lab). */
export const LAB_OUTCOME_LABELS: Record<string, string> = {
    equivalent: "Equivalent",
    reconverged: "Reconverged",
    diverged: "Diverged",
    implementation_failed: "Implementation failed",
    nondeterministic: "Nondeterministic",
    invariants_preserved: "Invariants preserved",
    invariant_violated: "Invariant violated",
    inconclusive: "Inconclusive",
};

/**
 * Tone of a differential-replay outcome. A difference between two
 * implementations is a finding, not necessarily an error, so it is shown
 * as a warning; only results that cannot be trusted are shown as failures.
 */
export function labOutcomeTone(outcome: string | null | undefined): Tone {
    switch (outcome) {
        case "equivalent":
            return "ok";
        case "reconverged":
        case "diverged":
        case "implementation_failed":
            return "warn";
        case "nondeterministic":
        case "invariant_violated":
            return "fail";
        case "invariants_preserved":
            return "ok";
        case "inconclusive":
            return "warn";
        default:
            return "neutral";
    }
}
