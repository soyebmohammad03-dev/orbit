"use client";

import { useState } from "react";

import { api } from "@/lib/api";
import { compactJson, formatMicros, relativeTime } from "@/lib/format";
import type { StoreIntegrityReport } from "@/lib/types";
import { useAction, useApi } from "@/lib/use-api";
import {
    Badge,
    Button,
    Card,
    Digest,
    EmptyState,
    ErrorState,
    Loading,
    Mono,
    PageHeader,
    Stat,
    Table,
    Td,
    TextLink,
    Th,
} from "@/components/ui";

export function OverviewView() {
    const overview = useApi(() => api.overview(), "overview");
    const health = useApi(() => api.health(), "health");
    const [verification, setVerification] = useState<StoreIntegrityReport | null>(null);
    const verify = useAction(async () => setVerification(await api.verifyStore()));

    return (
        <>
            <PageHeader
                title="Overview"
                description="Live state of the local ORBIT store, read from the API."
                actions={
                    <Button
                        onClick={() => {
                            overview.reload();
                            health.reload();
                        }}
                        variant="ghost"
                    >
                        Refresh
                    </Button>
                }
            />

            {health.error ? (
                <div className="mb-6">
                    <ErrorState title="API unreachable" error={health.error} onRetry={health.reload} />
                </div>
            ) : null}

            {overview.loading && !overview.data ? <Loading label="Loading overview" /> : null}
            {overview.error && !health.error ? <ErrorState error={overview.error} onRetry={overview.reload} /> : null}

            {overview.data ? (
                <div className="flex flex-col gap-6">
                    <div className="grid grid-cols-2 gap-3 md:grid-cols-5">
                        <Stat label="Streams" value={overview.data.statistics.streams} />
                        <Stat label="Events" value={overview.data.statistics.events} />
                        <Stat label="Snapshots" value={overview.data.statistics.snapshots} />
                        <Stat
                            label="Replay runs"
                            value={overview.data.statistics.replay_runs}
                            hint={`${overview.data.statistics.replay_executions} executions`}
                        />
                        <Stat
                            label="Schema"
                            value={`v${overview.data.statistics.schema_version}`}
                            hint={health.data ? `engine ${health.data.engine_version}` : undefined}
                        />
                    </div>

                    {overview.data.statistics.streams === 0 ? (
                        <EmptyState title="This store is empty.">
                            Create a stream on the <TextLink href="/streams">Streams</TextLink> page, then append events
                            to it from the <TextLink href="/events">Event Explorer</TextLink>.
                        </EmptyState>
                    ) : null}

                    <Card
                        title="Store integrity"
                        actions={
                            <Button onClick={() => void verify.run()} busy={verify.pending}>
                                {verification ? "Verify again" : "Run verification"}
                            </Button>
                        }
                    >
                        {verify.error ? <ErrorState error={verify.error} /> : null}
                        {!verification && !verify.error ? (
                            <p className="text-sm text-muted">
                                Not run in this session. Verification reads every row, so it only runs on request.
                            </p>
                        ) : null}
                        {verification ? (
                            <div className="flex flex-wrap items-center gap-3 text-sm">
                                {verification.valid ? (
                                    <Badge tone="ok">valid</Badge>
                                ) : (
                                    <Badge tone="fail">issues found</Badge>
                                )}
                                <span className="text-muted">
                                    {verification.streams.length} streams · {verification.snapshots_checked} snapshots ·{" "}
                                    {verification.replay_runs_checked} replay runs checked in{" "}
                                    {formatMicros(verification.elapsed_micros)} (measured on server)
                                </span>
                                <TextLink href="/integrity">Details</TextLink>
                            </div>
                        ) : null}
                    </Card>

                    <div className="grid gap-6 lg:grid-cols-2">
                        <Card title="Most recently active streams" padded={false}>
                            {overview.data.active_streams.length === 0 ? (
                                <p className="p-4 text-sm text-muted">No streams yet.</p>
                            ) : (
                                <Table label="Active streams">
                                    <thead>
                                        <tr>
                                            <Th>Stream</Th>
                                            <Th className="text-right">Events</Th>
                                            <Th>Head</Th>
                                            <Th>Last event</Th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {overview.data.active_streams.map((s) => (
                                            <tr key={s.stream_id}>
                                                <Td className="whitespace-nowrap">
                                                    <TextLink href={`/streams?id=${encodeURIComponent(s.stream_id)}`}>
                                                        {s.stream_id}
                                                    </TextLink>
                                                </Td>
                                                <Td className="text-right font-mono">{s.head_sequence}</Td>
                                                <Td>
                                                    <Digest value={s.head_digest} />
                                                </Td>
                                                <Td className="text-muted">{relativeTime(s.last_recorded_at)}</Td>
                                            </tr>
                                        ))}
                                    </tbody>
                                </Table>
                            )}
                        </Card>

                        <Card title="Recent events" padded={false}>
                            {overview.data.recent_events.length === 0 ? (
                                <p className="p-4 text-sm text-muted">No events yet.</p>
                            ) : (
                                <Table label="Recent events">
                                    <thead>
                                        <tr>
                                            <Th>Stream</Th>
                                            <Th>#</Th>
                                            <Th>Type</Th>
                                            <Th>Payload</Th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {overview.data.recent_events.map((e) => (
                                            <tr key={e.event_id}>
                                                <Td className="whitespace-nowrap">
                                                    <TextLink
                                                        href={`/events?stream=${encodeURIComponent(e.stream_id)}&seq=${e.sequence}`}
                                                    >
                                                        {e.stream_id}
                                                    </TextLink>
                                                </Td>
                                                <Td className="font-mono">{e.sequence}</Td>
                                                <Td>
                                                    <Mono>{e.event_type}</Mono>
                                                </Td>
                                                <Td className="max-w-[16rem] truncate font-mono text-xs text-muted">
                                                    {compactJson(e.payload, 60)}
                                                </Td>
                                            </tr>
                                        ))}
                                    </tbody>
                                </Table>
                            )}
                            <p className="border-t border-border px-4 py-2 text-xs text-faint">
                                Ordered by recording time, which is informational. Order within a stream is always by
                                sequence.
                            </p>
                        </Card>
                    </div>
                </div>
            ) : null}
        </>
    );
}
