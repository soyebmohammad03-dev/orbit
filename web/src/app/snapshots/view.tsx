"use client";

import { useState } from "react";

import { JsonView } from "@/components/json-view";
import { ProjectionPicker, StreamPicker, useQueryParam } from "@/components/stream-picker";
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
    PageHeader,
    Table,
    Td,
    Th,
} from "@/components/ui";
import { api } from "@/lib/api";
import { formatTimestamp } from "@/lib/format";
import type { CreateSnapshotResponse } from "@/lib/types";
import { useAction, useApi } from "@/lib/use-api";

export function SnapshotsView() {
    const [stream, setStream] = useQueryParam("stream");
    const [selectedId, setSelected] = useQueryParam("snapshot");
    const [refresh, setRefresh] = useState(0);
    const list = useApi(() => api.listSnapshots(stream ?? ""), stream ? `snapshots:${stream}:${refresh}` : null);
    const detail = useApi(() => api.getStream(stream ?? ""), stream ? `stream:${stream}:${refresh}` : null);
    const selected = list.data?.find((s) => s.snapshot_id === selectedId);

    return (
        <>
            <PageHeader
                title="Snapshots"
                description="Stored projection states bound to the digest of the event they were built from. Each is validated when listed: digest, history binding, projection key and state decoding."
                actions={<StreamPicker value={stream} onChange={(id) => setStream(id, ["snapshot"])} />}
            />
            {!stream ? (
                <EmptyState title="Select a stream to inspect its snapshots." />
            ) : (
                <div className="grid gap-6 xl:grid-cols-[minmax(0,1fr)_24rem]">
                    <div className="flex flex-col gap-6">
                        <Card title="Snapshots" padded={false}>
                            {list.loading && !list.data ? (
                                <div className="px-4">
                                    <Loading />
                                </div>
                            ) : null}
                            {list.error ? (
                                <div className="p-4">
                                    <ErrorState error={list.error} onRetry={list.reload} />
                                </div>
                            ) : null}
                            {list.data && list.data.length === 0 ? (
                                <div className="p-4">
                                    <EmptyState title="No snapshots for this stream yet.">
                                        Create one with the form; reconstructions will then start from it.
                                    </EmptyState>
                                </div>
                            ) : null}
                            {list.data && list.data.length > 0 ? (
                                <Table label="Snapshots">
                                    <thead>
                                        <tr>
                                            <Th>Sequence</Th>
                                            <Th>Projection</Th>
                                            <Th>Validation</Th>
                                            <Th>Event digest</Th>
                                            <Th>State digest</Th>
                                            <Th>Created</Th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {list.data.map((s) => (
                                            <tr
                                                key={s.snapshot_id}
                                                className={s.snapshot_id === selectedId ? "bg-surface-2" : undefined}
                                            >
                                                <Td>
                                                    <button
                                                        className="font-mono text-accent hover:underline"
                                                        onClick={() => setSelected(s.snapshot_id)}
                                                    >
                                                        #{s.sequence}
                                                    </button>
                                                </Td>
                                                <Td className="font-mono text-xs">{s.projection}</Td>
                                                <Td>
                                                    {s.validation.status === "usable" ? (
                                                        <Badge
                                                            tone="ok"
                                                            title={`checked by ${s.validation.checked_by}`}
                                                        >
                                                            usable
                                                        </Badge>
                                                    ) : (
                                                        <Badge tone="fail" title={JSON.stringify(s.validation.problem)}>
                                                            unusable: {s.validation.problem?.kind}
                                                        </Badge>
                                                    )}
                                                </Td>
                                                <Td>
                                                    <Digest value={s.event_digest} />
                                                </Td>
                                                <Td>
                                                    <Digest value={s.state_digest} />
                                                </Td>
                                                <Td className="whitespace-nowrap text-xs text-muted">
                                                    {formatTimestamp(s.created_at)}
                                                </Td>
                                            </tr>
                                        ))}
                                    </tbody>
                                </Table>
                            ) : null}
                        </Card>
                        {selected ? (
                            <Card title={`Snapshot #${selected.sequence}`}>
                                <div className="flex flex-col gap-4">
                                    <KeyValue
                                        items={[
                                            [
                                                "Snapshot id",
                                                <span key="i" className="font-mono text-xs">
                                                    {selected.snapshot_id}
                                                </span>,
                                            ],
                                            ["Projection", selected.projection],
                                            ["Covers events", `#1 – #${selected.sequence}`],
                                            [
                                                "Bound to event digest",
                                                <Digest key="e" value={selected.event_digest} full />,
                                            ],
                                            ["State digest", <Digest key="s" value={selected.state_digest} full />],
                                            ["Validated by", selected.validation.checked_by],
                                        ]}
                                    />
                                    {selected.validation.problem ? (
                                        <ErrorState
                                            title="Snapshot is unusable"
                                            error={{
                                                message: JSON.stringify(selected.validation.problem),
                                                code: selected.validation.problem.kind,
                                            }}
                                        />
                                    ) : null}
                                    <JsonView value={selected.state} label="Snapshot state" />
                                </div>
                            </Card>
                        ) : null}
                    </div>
                    <CreateSnapshot
                        stream={stream}
                        head={detail.data?.head_sequence}
                        onCreated={(r) => {
                            setRefresh((n) => n + 1);
                            setSelected(r.snapshot.snapshot_id);
                        }}
                    />
                </div>
            )}
        </>
    );
}

function CreateSnapshot({
    stream,
    head,
    onCreated,
}: {
    stream: string;
    head: number | undefined;
    onCreated: (r: CreateSnapshotResponse) => void;
}) {
    const [projection, setProjection] = useState("merge-patch");
    const [at, setAt] = useState("");
    const [result, setResult] = useState<CreateSnapshotResponse | null>(null);
    const create = useAction(() => api.createSnapshot(stream, { projection, at: at ? Number(at) : undefined }));
    return (
        <Card title="Create snapshot">
            <form
                className="flex flex-col gap-3"
                onSubmit={async (e) => {
                    e.preventDefault();
                    const r = await create.run();
                    if (r) {
                        setResult(r);
                        onCreated(r);
                    }
                }}
            >
                <ProjectionPicker value={projection} onChange={setProjection} id="snapshot-projection" />
                <Field label={`Sequence (default: head${head !== undefined ? ` #${head}` : ""})`} htmlFor="snapshot-at">
                    <Input
                        id="snapshot-at"
                        type="number"
                        min={1}
                        max={head}
                        value={at}
                        onChange={(e) => setAt(e.target.value)}
                        placeholder={head !== undefined ? String(head) : ""}
                    />
                </Field>
                {create.error ? <ErrorState title="Snapshot not created" error={create.error} /> : null}
                {result ? (
                    <p className="text-sm text-muted">
                        {result.created ? "Created" : "An identical snapshot already existed"} at #
                        {result.snapshot.sequence} ({result.report.events_applied} events applied to build it).
                    </p>
                ) : null}
                <div>
                    <Button type="submit" variant="primary" busy={create.pending} disabled={!head}>
                        Create snapshot
                    </Button>
                </div>
            </form>
        </Card>
    );
}
