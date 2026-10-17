"use client";

import { useState } from "react";

import { JsonView } from "@/components/json-view";
import { StreamPicker, useQueryParam } from "@/components/stream-picker";
import {
    Badge,
    Button,
    Card,
    Digest,
    EmptyState,
    ErrorState,
    Field,
    Input,
    KeyValue,
    Loading,
    Mono,
    PageHeader,
    Table,
    Td,
    TextArea,
    TextLink,
    Th,
} from "@/components/ui";
import { api, ApiError } from "@/lib/api";
import { compactJson, formatTimestamp, isGenesis } from "@/lib/format";
import type { AppendEventRequest, Json, JsonObject } from "@/lib/types";
import { useAction, useApi } from "@/lib/use-api";

const PAGE = 25;

export function EventsView() {
    const [stream, setStream] = useQueryParam("stream");
    const [seqParam, setSeq] = useQueryParam("seq");
    const [cursors, setCursors] = useState<number[]>([0]);
    const [refresh, setRefresh] = useState(0);
    const after = cursors[cursors.length - 1] ?? 0;
    const page = useApi(
        () => api.listEvents(stream ?? "", { after, limit: PAGE }),
        stream ? `events:${stream}:${after}:${refresh}` : null,
    );
    const selectedSeq = seqParam ? Number(seqParam) : null;
    const selected = useApi(
        () => api.getEvent(stream ?? "", selectedSeq ?? 0),
        stream && selectedSeq ? `event:${stream}:${selectedSeq}` : null,
    );

    return (
        <>
            <PageHeader
                title="Event Explorer"
                description="Events of one stream in sequence order: the only authoritative order. Append new events with optional optimistic concurrency."
                actions={
                    <StreamPicker
                        value={stream}
                        onChange={(id) => {
                            setCursors([0]);
                            setStream(id, ["seq"]);
                        }}
                    />
                }
            />
            {!stream ? (
                <EmptyState title="Select a stream to browse its events.">
                    No streams yet? Create one on the <TextLink href="/streams">Streams</TextLink> page.
                </EmptyState>
            ) : (
                <div className="grid gap-6 xl:grid-cols-[minmax(0,1fr)_26rem]">
                    <Card title={page.data ? `Events · head #${page.data.head_sequence}` : "Events"} padded={false}>
                        {page.loading && !page.data ? (
                            <div className="px-4">
                                <Loading />
                            </div>
                        ) : null}
                        {page.error ? (
                            <div className="p-4">
                                <ErrorState error={page.error} onRetry={page.reload} />
                            </div>
                        ) : null}
                        {page.data && page.data.events.length === 0 ? (
                            <div className="p-4">
                                <EmptyState title="This stream has no events yet.">
                                    Append one with the form.
                                </EmptyState>
                            </div>
                        ) : null}
                        {page.data && page.data.events.length > 0 ? (
                            <Table label="Events">
                                <thead>
                                    <tr>
                                        <Th>#</Th>
                                        <Th>Type</Th>
                                        <Th>v</Th>
                                        <Th>Payload</Th>
                                        <Th>Recorded</Th>
                                        <Th>Digest</Th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {page.data.events.map((e) => (
                                        <tr
                                            key={e.event_id}
                                            className={e.sequence === selectedSeq ? "bg-surface-2" : undefined}
                                        >
                                            <Td>
                                                <button
                                                    className="font-mono text-accent hover:underline"
                                                    onClick={() => setSeq(String(e.sequence))}
                                                    aria-label={`Inspect event ${e.sequence}`}
                                                >
                                                    {e.sequence}
                                                </button>
                                            </Td>
                                            <Td>
                                                <Mono>{e.event_type}</Mono>
                                            </Td>
                                            <Td className="font-mono text-muted">{e.schema_version}</Td>
                                            <Td className="max-w-[18rem] truncate font-mono text-xs text-muted">
                                                {compactJson(e.payload, 70)}
                                            </Td>
                                            <Td className="whitespace-nowrap text-xs text-muted">
                                                {formatTimestamp(e.recorded_at)}
                                            </Td>
                                            <Td>
                                                <Digest value={e.digest} />
                                            </Td>
                                        </tr>
                                    ))}
                                </tbody>
                            </Table>
                        ) : null}
                        {page.data ? (
                            <div className="flex items-center justify-between gap-2 border-t border-border px-4 py-2 text-xs text-muted">
                                <span>
                                    Showing #{page.data.events[0]?.sequence ?? "–"}–#
                                    {page.data.events[page.data.events.length - 1]?.sequence ?? "–"} of{" "}
                                    {page.data.head_sequence}
                                </span>
                                <div className="flex gap-2">
                                    <Button
                                        variant="ghost"
                                        disabled={cursors.length === 1}
                                        onClick={() => setCursors((c) => c.slice(0, -1))}
                                    >
                                        Previous
                                    </Button>
                                    <Button
                                        variant="ghost"
                                        disabled={page.data.next_after === null}
                                        onClick={() =>
                                            page.data?.next_after != null &&
                                            setCursors((c) => [...c, page.data?.next_after ?? 0])
                                        }
                                    >
                                        Next
                                    </Button>
                                </div>
                            </div>
                        ) : null}
                    </Card>

                    <div className="flex flex-col gap-6">
                        <AppendEvent
                            stream={stream}
                            head={page.data?.head_sequence}
                            onAppended={(seq) => {
                                setRefresh((r) => r + 1);
                                setSeq(String(seq));
                            }}
                        />
                        {selectedSeq ? (
                            <Card title={`Event #${selectedSeq}`}>
                                {selected.loading ? <Loading /> : null}
                                {selected.error ? (
                                    <ErrorState error={selected.error} onRetry={selected.reload} />
                                ) : null}
                                {selected.data ? (
                                    <div className="flex flex-col gap-4">
                                        <KeyValue
                                            items={[
                                                ["Event id", <Mono key="id">{selected.data.event_id}</Mono>],
                                                ["Type", <Mono key="t">{selected.data.event_type}</Mono>],
                                                ["Schema version", selected.data.schema_version],
                                                ["Recorded", formatTimestamp(selected.data.recorded_at)],
                                                [
                                                    "Previous digest",
                                                    isGenesis(selected.data.prev_digest) ? (
                                                        <Badge key="g">genesis</Badge>
                                                    ) : (
                                                        <Digest key="p" value={selected.data.prev_digest} />
                                                    ),
                                                ],
                                                ["Digest", <Digest key="d" value={selected.data.digest} />],
                                            ]}
                                        />
                                        <div>
                                            <p className="mb-1 text-xs font-medium text-muted">Payload</p>
                                            <JsonView
                                                value={selected.data.payload}
                                                label="Event payload"
                                                maxHeight="16rem"
                                            />
                                        </div>
                                        <div>
                                            <p className="mb-1 text-xs font-medium text-muted">Metadata</p>
                                            <JsonView
                                                value={selected.data.metadata}
                                                label="Event metadata"
                                                maxHeight="10rem"
                                            />
                                        </div>
                                    </div>
                                ) : null}
                            </Card>
                        ) : null}
                    </div>
                </div>
            )}
        </>
    );
}

function parseJson(text: string, what: string, requireObject: boolean): Json {
    let parsed: unknown;
    try {
        parsed = JSON.parse(text);
    } catch (e) {
        throw new Error(`${what} is not valid JSON: ${e instanceof Error ? e.message : String(e)}`);
    }
    if (requireObject && (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))) {
        throw new Error(`${what} must be a JSON object`);
    }
    return parsed as Json;
}

function AppendEvent({
    stream,
    head,
    onAppended,
}: {
    stream: string;
    head: number | undefined;
    onAppended: (sequence: number) => void;
}) {
    const [eventType, setEventType] = useState("");
    const [payload, setPayload] = useState("{}");
    const [metadata, setMetadata] = useState("");
    const [guard, setGuard] = useState(false);
    const [localError, setLocalError] = useState<string | null>(null);
    const append = useAction((body: AppendEventRequest) => api.appendEvent(stream, body));

    async function submit(event: React.FormEvent) {
        event.preventDefault();
        setLocalError(null);
        let body: AppendEventRequest;
        try {
            body = { event_type: eventType.trim(), payload: parseJson(payload, "Payload", false) };
            if (metadata.trim()) body.metadata = parseJson(metadata, "Metadata", true) as JsonObject;
            if (guard && head !== undefined) body.expected_sequence = head;
        } catch (e) {
            setLocalError(e instanceof Error ? e.message : String(e));
            return;
        }
        const created = await append.run(body);
        if (created) onAppended(created.sequence);
    }

    const conflict =
        append.error instanceof ApiError && append.error.code === "sequence_conflict" ? append.error : null;

    return (
        <Card title="Append event">
            <form onSubmit={submit} className="flex flex-col gap-3">
                <Field label="Event type" htmlFor="event-type">
                    <Input
                        id="event-type"
                        value={eventType}
                        onChange={(e) => setEventType(e.target.value)}
                        placeholder="order.created"
                        required
                    />
                </Field>
                <Field
                    label="Payload (JSON)"
                    htmlFor="event-payload"
                    hint="With the default merge-patch projection, an object payload is merged into the state; null removes a field."
                >
                    <TextArea id="event-payload" value={payload} onChange={(e) => setPayload(e.target.value)} />
                </Field>
                <Field label="Metadata (optional JSON object)" htmlFor="event-metadata">
                    <TextArea
                        id="event-metadata"
                        value={metadata}
                        onChange={(e) => setMetadata(e.target.value)}
                        className="min-h-12"
                        placeholder='{"actor": "alice"}'
                    />
                </Field>
                <label className="flex items-center gap-2 text-sm text-muted">
                    <input
                        type="checkbox"
                        checked={guard}
                        onChange={(e) => setGuard(e.target.checked)}
                        className="accent-[var(--accent)]"
                    />
                    Only append if the head is still #{head ?? "?"} (optimistic concurrency)
                </label>
                {localError ? <ErrorState title="Invalid input" error={{ message: localError }} /> : null}
                {conflict ? (
                    <ErrorState
                        title="Conflict: the stream moved"
                        error={{
                            message: `Expected head #${String((conflict.details as JsonObject | null)?.expected_sequence)}, actual head is #${String((conflict.details as JsonObject | null)?.actual_sequence)}. Refresh and retry.`,
                            code: conflict.code,
                            status: conflict.status,
                        }}
                    />
                ) : append.error ? (
                    <ErrorState title="Append rejected" error={append.error} />
                ) : null}
                <div>
                    <Button type="submit" variant="primary" busy={append.pending} disabled={!eventType.trim()}>
                        Append
                    </Button>
                </div>
            </form>
        </Card>
    );
}
