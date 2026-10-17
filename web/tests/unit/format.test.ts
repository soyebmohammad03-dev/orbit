import { describe, expect, it } from "vitest";

import {
    compactJson,
    formatMicros,
    formatNanos,
    isGenesis,
    outcomeTone,
    relativeTime,
    shortDigest,
} from "@/lib/format";

describe("format", () => {
    it("shortens digests", () => {
        const d = "a".repeat(60) + "beef";
        expect(shortDigest(d)).toBe("aaaaaaaa…beef");
        expect(shortDigest("abc")).toBe("abc");
        expect(isGenesis("0".repeat(64))).toBe(true);
        expect(isGenesis(d)).toBe(false);
    });

    it("formats durations", () => {
        expect(formatMicros(512)).toBe("512 µs");
        expect(formatMicros(2_500)).toBe("2.50 ms");
        expect(formatMicros(45_000)).toBe("45.0 ms");
        expect(formatMicros(3_200_000)).toBe("3.20 s");
        expect(formatNanos(850)).toBe("850 ns");
        expect(formatNanos(6_920)).toBe("6.92 µs");
        expect(formatNanos(11_400_000)).toBe("11.4 ms");
        expect(formatNanos(Number.NaN)).toBe("—");
    });

    it("formats relative time", () => {
        const now = new Date("2026-01-01T12:00:00Z");
        expect(relativeTime(null, now)).toBe("never");
        expect(relativeTime("2026-01-01T11:59:58Z", now)).toBe("just now");
        expect(relativeTime("2026-01-01T11:55:00Z", now)).toBe("5 min ago");
        expect(relativeTime("2025-12-28T12:00:00Z", now)).toBe("4 d ago");
    });

    it("maps replay outcomes to tones", () => {
        expect(outcomeTone("exact_match")).toBe("ok");
        expect(outcomeTone("diverged")).toBe("fail");
        expect(outcomeTone("integrity_failure")).toBe("fail");
        expect(outcomeTone("reconverged")).toBe("warn");
        expect(outcomeTone(null)).toBe("neutral");
    });

    it("truncates JSON", () => {
        expect(compactJson({ a: 1 })).toBe('{"a":1}');
        expect(compactJson("x".repeat(100), 10)).toHaveLength(10);
    });
});
