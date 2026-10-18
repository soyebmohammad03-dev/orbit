"use client";

import { type ReactNode, useState } from "react";

import { DiffView } from "@/components/diff-view";
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
    Select,
    Table,
    Td,
    Th,
} from "@/components/ui";
import { api } from "@/lib/api";
import { LAB_OUTCOME_LABELS, formatMicros, formatTimestamp, labOutcomeTone, shortDigest } from "@/lib/format";
import type {
    ArtifactVerificationResponse,
    ExperimentRecord,
    ImplementationDescriptor,
    LabComparison,
    LabSide,
    VerificationRecord,
} from "@/lib/types";
import { useAction, useApi } from "@/lib/use-api";

export function LabOutcomeBadge({ record }: { record: Pick<ExperimentRecord, "status" | "outcome"> }) {
    if (record.status === "failed") return <Badge tone="fail">experiment failed</Badge>;
    if (!record.outcome) return <Badge>{record.status}</Badge>;
    return <Badge tone={labOutcomeTone(record.outcome)}>{LAB_OUTCOME_LABELS[record.outcome] ?? record.outcome}</Badge>;
}

function ReproducibilityBadge({ value }: { value: string | null }) {
    if (!value) return null;
    const tone = value === "reproducible" ? "ok" : value === "unverified" ? "warn" : "fail";
    return <Badge tone={tone}>{value.replace("_", " ")}</Badge>;
}

export function ExperimentsView() {
    const [experimentId, setExperiment] = useQueryParam("experiment");
    const [refresh, setRefresh] = useState(0);
    const list = useApi(() => api.listLabExperiments({ limit: 100 }), `lab:list:${refresh}`);

    return (
        <>
            <PageHeader
                title="Experiments"
                description="Differential replay: two projection implementations fold the same verified, ordered history, and their canonical states are compared after every event. Results, identities and artifacts are kept in the lab registry, a database separate from the event store."
            />
            <div className="grid grid-cols-1 gap-6 xl:grid-cols-[minmax(0,1fr)_24rem]">
                <div className="flex min-w-0 flex-col gap-6">
                    <Card title="Recorded experiments" padded={false}>
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
                        {list.data && list.data.experiments.length === 0 ? (
                            <div className="p-4">
                                <EmptyState title="No experiments yet.">
                                    Compare two implementations over a stream with the form.
                                </EmptyState>
                            </div>
                        ) : null}
                        {list.data && list.data.experiments.length > 0 ? (
                            <Table label="Experiments">
                                <thead>
                                    <tr>
                                        <Th>Experiment</Th>
                                        <Th>Stream</Th>
                                        <Th>Implementations</Th>
                                        <Th>Outcome</Th>
                                        <Th>Reproducibility</Th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {list.data.experiments.map((e) => (
                                        <tr key={e.id} className={e.id === experimentId ? "bg-surface-2" : undefined}>
                                            <Td>
                                                <button
                                                    className="font-mono text-xs text-accent hover:underline"
                                                    onClick={() => setExperiment(e.id)}
                                                    aria-label={`Show experiment ${e.id}`}
                                                >
                                                    {shortDigest(e.id, 8, 4)}
                                                </button>
                                            </Td>
                                            <Td className="font-mono text-xs">{e.stream}</Td>
                                            <Td className="font-mono text-xs">
                                                {e.left} ↔ {e.right}
                                            </Td>
                                            <Td>
                                                <LabOutcomeBadge record={e} />
                                            </Td>
                                            <Td>
                                                <ReproducibilityBadge value={e.reproducibility} />
                                            </Td>
                                        </tr>
                                    ))}
                                </tbody>
                            </Table>
                        ) : null}
                    </Card>
                    {experimentId ? (
                        <ExperimentPanel id={experimentId} onChanged={() => setRefresh((n) => n + 1)} />
                    ) : null}
                </div>
                <NewExperiment
                    onCreated={(id) => {
                        setRefresh((n) => n + 1);
                        setExperiment(id);
                    }}
                />
            </div>
        </>
    );
}

function implementationLabel(i: ImplementationDescriptor): string {
    return `${i.id} (${i.projection.id}@${i.projection.version})`;
}

function NewExperiment({ onCreated }: { onCreated: (id: string) => void }) {
    const implementations = useApi(() => api.labImplementations(), "lab:implementations");
    const [stream, setStream] = useState<string | null>(null);
    const [left, setLeft] = useState("orbit.counter");
    const [right, setRight] = useState("example.counter-floor-at-zero");
    const [reference, setReference] = useState<"" | LabSide>("");
    const [to, setTo] = useState("");
    const create = useAction(() =>
        api.createDifferential({
            stream: stream ?? "",
            left,
            right,
            reference: reference || null,
            to: to ? Number(to) : null,
        }),
    );
    const options = implementations.data?.implementations ?? [];
    return (
        <Card title="New differential replay">
            {implementations.error ? (
                <ErrorState error={implementations.error} onRetry={implementations.reload} />
            ) : null}
            <form
                className="flex flex-col gap-3"
                onSubmit={async (e) => {
                    e.preventDefault();
                    const record = await create.run();
                    if (record) onCreated(record.id);
                }}
            >
                <StreamPicker value={stream} onChange={setStream} id="lab-stream" />
                {(["left", "right"] as const).map((side) => (
                    <Field
                        key={side}
                        label={side === "left" ? "Left implementation" : "Right implementation"}
                        htmlFor={`lab-${side}`}
                    >
                        <Select
                            id={`lab-${side}`}
                            value={side === "left" ? left : right}
                            onChange={(e) => (side === "left" ? setLeft : setRight)(e.target.value)}
                            disabled={options.length === 0}
                        >
                            {options.map((i) => (
                                <option key={i.id} value={i.id} title={i.implementation_type}>
                                    {implementationLabel(i)}
                                </option>
                            ))}
                        </Select>
                    </Field>
                ))}
                <Field
                    label="Reference"
                    htmlFor="lab-reference"
                    hint="Without a reference, results describe differences between peers."
                >
                    <Select
                        id="lab-reference"
                        value={reference}
                        onChange={(e) => setReference(e.target.value as "" | LabSide)}
                    >
                        <option value="">None</option>
                        <option value="left">Left is expected</option>
                        <option value="right">Right is expected</option>
                    </Select>
                </Field>
                <Field label="Last event (default: head)" htmlFor="lab-to">
                    <Input id="lab-to" type="number" min={0} value={to} onChange={(e) => setTo(e.target.value)} />
                </Field>
                {create.error ? <ErrorState title="Experiment not created" error={create.error} /> : null}
                <div>
                    <Button type="submit" variant="primary" busy={create.pending} disabled={!stream}>
                        Run differential replay
                    </Button>
                </div>
            </form>
        </Card>
    );
}

function ExperimentPanel({ id, onChanged }: { id: string; onChanged: () => void }) {
    const [refresh, setRefresh] = useState(0);
    const detail = useApi(() => api.getLabExperiment(id), `lab:${id}:${refresh}`);
    const [verified, setVerified] = useState<ArtifactVerificationResponse | null>(null);
    const verify = useAction(() => api.verifyLabExperiment(id));
    const reproduce = useAction(() => api.reproduceLabExperiment(id));
    const reload = () => {
        setRefresh((n) => n + 1);
        onChanged();
    };
    if (detail.loading && !detail.data) return <Loading />;
    if (detail.error) return <ErrorState error={detail.error} onRetry={detail.reload} />;
    if (!detail.data) return null;
    const { experiment, verifications } = detail.data;
    const finished = experiment.status === "completed" || experiment.status === "failed";
    return (
        <>
            <ExperimentSummary record={experiment} />
            <Card
                title="Verification"
                actions={
                    <>
                        <Button
                            busy={verify.pending}
                            disabled={!finished}
                            onClick={async () => {
                                const r = await verify.run();
                                if (r) {
                                    setVerified(r);
                                    reload();
                                }
                            }}
                        >
                            Verify artifacts
                        </Button>
                        <Button
                            busy={reproduce.pending}
                            disabled={experiment.status !== "completed"}
                            onClick={async () => {
                                if (await reproduce.run()) reload();
                            }}
                        >
                            Reproduce
                        </Button>
                    </>
                }
            >
                {verify.error ? <ErrorState title="Verification failed" error={verify.error} /> : null}
                {reproduce.error ? <ErrorState title="Reproduction failed" error={reproduce.error} /> : null}
                {verified ? (
                    <div className="mb-3 flex flex-wrap items-center gap-2 text-sm">
                        <Badge tone={verified.report.status === "intact" ? "ok" : "fail"}>
                            {verified.report.status}
                        </Badge>
                        {verified.report.artifacts.map((a) => (
                            <Badge key={a.role} tone={a.state === "intact" ? "ok" : "fail"} title={a.digest}>
                                {a.role}: {a.state}
                            </Badge>
                        ))}
                    </div>
                ) : null}
                <VerificationList items={verifications} />
            </Card>
        </>
    );
}

function VerificationList({ items }: { items: VerificationRecord[] }) {
    if (items.length === 0) return <p className="text-sm text-muted">Not verified yet.</p>;
    return (
        <ul aria-label="Verification history" className="flex flex-col gap-1 text-sm">
            {items.map((v) => (
                <li key={v.id} className="flex flex-wrap items-center gap-2">
                    <Badge tone={["intact", "reproduced"].includes(v.status) ? "ok" : "fail"}>{v.status}</Badge>
                    <span className="text-muted">{v.kind}</span>
                    <span className="text-xs text-faint">{formatTimestamp(v.verified_at)}</span>
                </li>
            ))}
        </ul>
    );
}

function sideName(side: LabSide, reference: LabSide | null): string {
    if (!reference) return side;
    return side === reference ? `${side} (expected)` : `${side} (observed)`;
}

function ComparisonView({ comparison, reference }: { comparison: LabComparison; reference: LabSide | null }) {
    return (
        <div className="flex flex-col gap-3">
            <KeyValue
                items={[
                    ["Boundary", <Mono key="b">{comparison.boundary}</Mono>],
                    [
                        "Event",
                        comparison.event ? (
                            <Mono key="e">
                                #{comparison.event.sequence} {comparison.event.event_type}
                            </Mono>
                        ) : (
                            "initial state"
                        ),
                    ],
                    [sideName("left", reference), <Digest key="l" value={comparison.left.digest} />],
                    [sideName("right", reference), <Digest key="r" value={comparison.right.digest} />],
                ]}
            />
            <DiffView changes={comparison.changes} />
            {comparison.changes_total > comparison.changes.length ? (
                <p className="text-xs text-muted">
                    Showing {comparison.changes.length} of {comparison.changes_total} changes.
                </p>
            ) : null}
        </div>
    );
}

/** The stored experiment: identities, input, outcome and diagnosis. */
export function ExperimentSummary({ record }: { record: ExperimentRecord }) {
    const result = record.result;
    const reference = result?.reference ?? null;
    return (
        <>
            <Card title="Experiment" actions={<LabOutcomeBadge record={record} />}>
                <KeyValue
                    items={[
                        ["Id", <Mono key="id">{record.id}</Mono>],
                        ["Status", record.status],
                        [
                            "Input",
                            record.input ? (
                                <Mono key="in">
                                    {record.input.stream}, boundaries {record.input.from}–{record.input.to}
                                </Mono>
                            ) : (
                                <Mono key="in">{record.spec.stream}</Mono>
                            ),
                        ],
                        ...record.implementations.map(
                            (i) =>
                                [
                                    sideName(i.side, reference),
                                    <span key={i.side} className="flex flex-wrap items-center gap-2">
                                        <Mono>{implementationLabel(i.descriptor)}</Mono>
                                        <Digest value={i.descriptor.fingerprint} />
                                    </span>,
                                ] as [string, ReactNode],
                        ),
                        [
                            "Reproducibility",
                            <span key="rep" className="flex flex-col gap-1">
                                <ReproducibilityBadge value={record.reproducibility} />
                                {result?.reproducibility.reasons.map((r) => (
                                    <span key={r} className="text-xs text-muted">
                                        {r}
                                    </span>
                                ))}
                            </span>,
                        ],
                        [
                            "Build",
                            <Mono key="build">
                                {record.build.orbit_version}
                                {record.build.git_commit ? ` @ ${record.build.git_commit.slice(0, 12)}` : ""}
                            </Mono>,
                        ],
                        ["Elapsed", record.elapsed_micros === null ? "—" : formatMicros(record.elapsed_micros)],
                    ]}
                />
                {record.failure ? (
                    <div role="alert" className="mt-4 rounded-md border border-fail/30 bg-fail-soft p-3 text-sm">
                        <p className="font-medium text-fail">{record.failure.kind.replaceAll("_", " ")}</p>
                        <p className="text-text">{record.failure.message}</p>
                    </div>
                ) : null}
            </Card>
            {result ? (
                <Card title="Diagnosis">
                    <div className="flex flex-col gap-4">
                        <p className="text-sm text-muted">
                            {result.events_processed} events fed to both implementations · {result.boundaries_compared}{" "}
                            boundaries compared · {result.boundaries_differing} differing
                            {result.divergent_segments_total > 0
                                ? ` in ${result.divergent_segments_total} segment${result.divergent_segments_total === 1 ? "" : "s"}`
                                : ""}
                        </p>
                        {result.failures.map((f) => (
                            <div
                                key={f.side}
                                role="alert"
                                className="rounded-md border border-warn/30 bg-warn-soft p-3 text-sm"
                            >
                                <p className="font-medium text-warn">
                                    {f.side} ({f.implementation}) failed at boundary {f.boundary}: {f.failure.kind}
                                </p>
                                <p className="text-text">{f.failure.error?.message ?? f.failure.message ?? ""}</p>
                            </div>
                        ))}
                        {result.first_divergence ? (
                            <div>
                                <h3 className="mb-2 text-sm font-medium text-text">
                                    First divergence (after {result.first_divergence.boundaries_matched_before} matching
                                    boundaries)
                                </h3>
                                <ComparisonView comparison={result.first_divergence} reference={reference} />
                            </div>
                        ) : (
                            <p className="text-sm text-ok">The states were equal at every compared boundary.</p>
                        )}
                        {result.final_comparison && result.first_divergence ? (
                            <div>
                                <h3 className="mb-2 text-sm font-medium text-text">Final states</h3>
                                <ComparisonView comparison={result.final_comparison} reference={reference} />
                            </div>
                        ) : null}
                        {record.artifacts.length > 0 ? (
                            <div>
                                <h3 className="mb-2 text-sm font-medium text-text">Artifacts</h3>
                                <ul className="flex flex-col gap-1 text-xs">
                                    {record.artifacts.map((a) => (
                                        <li key={a.role} className="flex flex-wrap items-center gap-2">
                                            <Mono>{a.role}</Mono>
                                            <span className="text-faint">{a.size} bytes</span>
                                            <Digest value={a.digest} />
                                        </li>
                                    ))}
                                </ul>
                            </div>
                        ) : null}
                    </div>
                </Card>
            ) : null}
        </>
    );
}
