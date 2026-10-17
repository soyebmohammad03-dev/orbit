import { describe, expect, it, vi } from "vitest";

import { ApiError, createClient } from "@/lib/api";

function response(status: number, body: unknown, headers: Record<string, string> = {}) {
    return new Response(body === undefined ? "" : JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json", ...headers },
    });
}

describe("api client", () => {
    it("builds URLs, sends JSON bodies and parses responses", async () => {
        const fetch = vi.fn(async () => response(201, { sequence: 3 }));
        const api = createClient({ baseUrl: "http://api.test/api/v1/", fetch });
        const event = await api.appendEvent("orders/1", { event_type: "a", payload: { x: 1 }, expected_sequence: 2 });
        expect(event).toEqual({ sequence: 3 });
        const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
        expect(url).toBe("http://api.test/api/v1/streams/orders%2F1/events");
        expect(init.method).toBe("POST");
        expect(JSON.parse(init.body as string)).toEqual({ event_type: "a", payload: { x: 1 }, expected_sequence: 2 });
        expect((init.headers as Record<string, string>)["content-type"]).toBe("application/json");
    });

    it("omits empty query parameters", async () => {
        const fetch = vi.fn<typeof globalThis.fetch>(async () =>
            response(200, { events: [], next_after: null, head_sequence: 0 }),
        );
        const api = createClient({ baseUrl: "http://api.test", fetch });
        await api.listEvents("s", { after: undefined, limit: 25 });
        await api.getDiff("s", { from: 0 });
        expect(fetch.mock.calls[0]?.[0]).toBe("http://api.test/streams/s/events?limit=25");
        expect(fetch.mock.calls[1]?.[0]).toBe("http://api.test/streams/s/diff?from=0");
    });

    it("turns the structured error envelope into an ApiError", async () => {
        const body = {
            error: {
                code: "sequence_conflict",
                message: "conflict",
                class: "conflict",
                details: { expected_sequence: 0, actual_sequence: 2 },
            },
        };
        const api = createClient({ baseUrl: "http://api.test", fetch: async () => response(409, body) });
        const error = await api.appendEvent("s", { event_type: "a", payload: 1 }).catch((e: unknown) => e);
        expect(error).toBeInstanceOf(ApiError);
        const apiError = error as ApiError;
        expect(apiError.status).toBe(409);
        expect(apiError.code).toBe("sequence_conflict");
        expect(apiError.errorClass).toBe("conflict");
        expect(apiError.details).toEqual({ expected_sequence: 0, actual_sequence: 2 });
        expect(apiError.isNetworkError).toBe(false);
    });

    it("reports non-JSON HTTP errors without inventing a code", async () => {
        const api = createClient({
            baseUrl: "http://api.test",
            fetch: async () => new Response("bad gateway", { status: 502 }),
        });
        const error = (await api.health().catch((e: unknown) => e)) as ApiError;
        expect(error.status).toBe(502);
        expect(error.code).toBe("http_error");
        expect(error.message).toContain("bad gateway");
    });

    it("reports an unreachable API as a network error", async () => {
        const api = createClient({
            baseUrl: "http://api.test",
            fetch: async () => {
                throw new TypeError("Failed to fetch");
            },
        });
        const error = (await api.health().catch((e: unknown) => e)) as ApiError;
        expect(error.isNetworkError).toBe(true);
        expect(error.code).toBe("network_error");
        expect(error.message).toContain("http://api.test");
    });

    it("exposes the server-measured request time", async () => {
        const api = createClient({
            baseUrl: "http://api.test",
            fetch: async () => response(200, { state: {} }, { "x-orbit-elapsed-micros": "1234" }),
        });
        const result = await api.getState("s", { at: 2 });
        expect(result.serverMicros).toBe(1234);
    });
});
