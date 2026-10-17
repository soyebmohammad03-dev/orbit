"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import { ApiError } from "./api";

export interface ApiState<T> {
    data: T | undefined;
    error: ApiError | undefined;
    loading: boolean;
    reload: () => void;
}

function toApiError(e: unknown): ApiError {
    if (e instanceof ApiError) return e;
    return new ApiError(0, {
        code: "client_error",
        message: e instanceof Error ? e.message : String(e),
        class: null,
        details: null,
    });
}

/**
 * Runs `load` when `key` changes (and on `reload()`), tracking loading and
 * error state. Pass `key = null` to skip loading (e.g. no stream selected).
 * Results are tagged with the request they belong to, so a superseded
 * request can never overwrite a newer one, and data from a previous `key`
 * is never shown for a new one.
 */
export function useApi<T>(load: () => Promise<T>, key: string | null): ApiState<T> {
    const [nonce, setNonce] = useState(0);
    const [result, setResult] = useState<{ key: string | null; nonce: number; data?: T; error?: ApiError }>({
        key: null,
        nonce: -1,
    });
    const loadRef = useRef(load);
    useEffect(() => {
        loadRef.current = load;
    });

    useEffect(() => {
        if (key === null) return;
        let current = true;
        loadRef.current().then(
            (data) => {
                if (current) setResult({ key, nonce, data });
            },
            (e: unknown) => {
                if (current) setResult({ key, nonce, error: toApiError(e) });
            },
        );
        return () => {
            current = false;
        };
    }, [key, nonce]);

    const reload = useCallback(() => setNonce((n) => n + 1), []);
    const sameKey = key !== null && result.key === key;
    const settled = sameKey && result.nonce === nonce;
    return {
        // Keep showing the previous data for the same key while a reload is in flight.
        data: sameKey ? result.data : undefined,
        error: settled ? result.error : undefined,
        loading: key !== null && !settled,
        reload,
    };
}

/** Wraps a mutation with pending and error state. */
export function useAction<A extends unknown[], R>(action: (...args: A) => Promise<R>) {
    const [pending, setPending] = useState(false);
    const [error, setError] = useState<ApiError | undefined>(undefined);
    const run = useCallback(
        async (...args: A): Promise<R | undefined> => {
            setPending(true);
            setError(undefined);
            try {
                return await action(...args);
            } catch (e) {
                setError(toApiError(e));
                return undefined;
            } finally {
                setPending(false);
            }
        },
        [action],
    );
    return { run, pending, error, clearError: () => setError(undefined) };
}
