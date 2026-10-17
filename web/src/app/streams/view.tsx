"use client";

import { useState } from "react";

import { useQueryParam } from "@/components/stream-picker";
import { JsonView } from "@/components/json-view";
import {
    Button,
    Card,
    Digest,
    EmptyState,
    ErrorState,
    Field,
    Input,
    KeyValue,
    Loading,
    PageHeader,
    Table,
    Td,
    TextArea,
    TextLink,
    Th,
} from "@/components/ui";
import { api } from "@/lib/api";
import { formatTimestamp, relativeTime } from "@/lib/format";
import type { JsonObject } from "@/lib/types";
import { useAction, useApi } from "@/lib/use-api";

const PAGE = 25;

export function StreamsView() {
    const [selected, setSelected] = useQueryParam("id");
    const [cursors, setCursors] = useState<(string | undefined)[]>([undefined]);
    const after = cursors[cursors.length - 1];
    const page = useApi(() => api.listStreams({ after, limit: PAGE }), `streams:${after ?? ""}`);
    const detail = useApi(() => api.getStream(selected ?? ""), selected ? `stream:${selected}` : null);

    return (
        <>
            <PageHeader
                title="Streams"
                description="Every stream in the store, ordered by id. Streams are created explicitly and never deleted."
            />
            <div className="grid grid-cols-1 gap-6 xl:grid-cols-[minmax(0,1fr)_24rem]">
                <div className="flex flex-col gap-6">
                    <Card title="All streams" padded={false}>
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
                        {page.data && page.data.streams.length === 0 && cursors.length === 1 ? (
                            <div className="p-4">
                                <EmptyState title="No streams yet.">Create the first one with the form.</EmptyState>
                            </div>
                        ) : null}
                        {page.data && page.data.streams.length > 0 ? (
                            <Table label="Streams">
                                <thead>
                                    <tr>
                                        <Th>Stream</Th>
                                        <Th className="text-right">Events</Th>
                                        <Th>Head digest</Th>
                                        <Th className="text-right">Snapshots</Th>
                                        <Th className="text-right">Replay runs</Th>
                                        <Th>Last event</Th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {page.data.streams.map((s) => (
                                        <tr
                                            key={s.stream_id}
                                            className={s.stream_id === selected ? "bg-surface-2" : undefined}
                                        >
                                            <Td>
                                                <button
                                                    className="text-left text-accent hover:underline"
                                                    onClick={() => setSelected(s.stream_id)}
                                                >
                                                    {s.stream_id}
                                                </button>
                                            </Td>
                                            <Td className="text-right font-mono">{s.head_sequence}</Td>
                                            <Td>
                                                <Digest value={s.head_digest} />
                                            </Td>
                                            <Td className="text-right font-mono">{s.snapshot_count}</Td>
                                            <Td className="text-right font-mono">{s.replay_run_count}</Td>
                                            <Td className="text-muted">{relativeTime(s.last_recorded_at)}</Td>
                                        </tr>
                                    ))}
                                </tbody>
                            </Table>
                        ) : null}
                        {page.data ? (
                            <div className="flex items-center justify-end gap-2 border-t border-border px-4 py-2">
                                <Button
                                    variant="ghost"
                                    disabled={cursors.length === 1}
                                    onClick={() => setCursors((c) => c.slice(0, -1))}
                                >
                                    Previous
                                </Button>
                                <Button
                                    variant="ghost"
                                    disabled={!page.data.next_after}
                                    onClick={() =>
                                        page.data?.next_after &&
                                        setCursors((c) => [...c, page.data?.next_after ?? undefined])
                                    }
                                >
                                    Next
                                </Button>
                            </div>
                        ) : null}
                    </Card>
                </div>

                <div className="flex flex-col gap-6">
                    <CreateStream
                        onCreated={(id) => {
                            setCursors([undefined]);
                            page.reload();
                            setSelected(id);
                        }}
                    />
                    {selected ? (
                        <Card title={`Stream ${selected}`}>
                            {detail.loading ? <Loading /> : null}
                            {detail.error ? <ErrorState error={detail.error} onRetry={detail.reload} /> : null}
                            {detail.data ? (
                                <div className="flex flex-col gap-4">
                                    <KeyValue
                                        items={[
                                            [
                                                "Events",
                                                <span key="e" className="font-mono">
                                                    {detail.data.head_sequence}
                                                </span>,
                                            ],
                                            ["Head digest", <Digest key="h" value={detail.data.head_digest} />],
                                            ["Created", formatTimestamp(detail.data.created_at)],
                                            ["Last event", formatTimestamp(detail.data.last_recorded_at)],
                                            ["Snapshots", detail.data.snapshot_count],
                                            ["Replay runs", detail.data.replay_run_count],
                                        ]}
                                    />
                                    <div>
                                        <p className="mb-1 text-xs font-medium text-muted">Metadata (immutable)</p>
                                        <JsonView
                                            value={detail.data.metadata}
                                            label="Stream metadata"
                                            maxHeight="12rem"
                                        />
                                    </div>
                                    <div className="flex flex-wrap gap-x-4 gap-y-1 text-sm">
                                        {[
                                            ["/events", "Events"],
                                            ["/time-travel", "Time travel"],
                                            ["/snapshots", "Snapshots"],
                                            ["/replay", "Replay"],
                                            ["/integrity", "Integrity"],
                                        ].map(([href, label]) => (
                                            <TextLink
                                                key={href}
                                                href={`${href}?stream=${encodeURIComponent(selected)}`}
                                            >
                                                {label}
                                            </TextLink>
                                        ))}
                                    </div>
                                </div>
                            ) : null}
                        </Card>
                    ) : null}
                </div>
            </div>
        </>
    );
}

function CreateStream({ onCreated }: { onCreated: (id: string) => void }) {
    const [id, setId] = useState("");
    const [metadata, setMetadata] = useState("");
    const [parseError, setParseError] = useState<string | null>(null);
    const create = useAction(async (streamId: string, meta?: JsonObject) => api.createStream(streamId, meta));

    async function submit(event: React.FormEvent) {
        event.preventDefault();
        setParseError(null);
        let meta: JsonObject | undefined;
        if (metadata.trim()) {
            try {
                const parsed: unknown = JSON.parse(metadata);
                if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
                    throw new Error("metadata must be a JSON object");
                meta = parsed as JsonObject;
            } catch (e) {
                setParseError(e instanceof Error ? e.message : String(e));
                return;
            }
        }
        const created = await create.run(id.trim(), meta);
        if (created) {
            setId("");
            setMetadata("");
            onCreated(created.stream_id);
        }
    }

    return (
        <Card title="Create stream">
            <form onSubmit={submit} className="flex flex-col gap-3">
                <Field
                    label="Stream id"
                    htmlFor="new-stream-id"
                    hint="1–128 characters from A–Z a–z 0–9 . _ : -, starting with a letter or digit."
                >
                    <Input
                        id="new-stream-id"
                        value={id}
                        onChange={(e) => setId(e.target.value)}
                        placeholder="orders-1"
                        required
                    />
                </Field>
                <Field label="Metadata (optional JSON object)" htmlFor="new-stream-metadata">
                    <TextArea
                        id="new-stream-metadata"
                        value={metadata}
                        onChange={(e) => setMetadata(e.target.value)}
                        placeholder='{"tenant": "acme"}'
                    />
                </Field>
                {parseError ? <ErrorState title="Invalid metadata" error={{ message: parseError }} /> : null}
                {create.error ? <ErrorState title="Could not create stream" error={create.error} /> : null}
                <div>
                    <Button type="submit" variant="primary" busy={create.pending} disabled={!id.trim()}>
                        Create stream
                    </Button>
                </div>
            </form>
        </Card>
    );
}
