"use client";

import Link from "next/link";
import { useEffect, useState } from "react";

import { useQueryParam } from "@/components/stream-picker";
import {
    Badge,
    Button,
    Card,
    Digest,
    EmptyState,
    ErrorState,
    Field,
    KeyValue,
    Loading,
    Mono,
    PageHeader,
    Table,
    Td,
    Th,
} from "@/components/ui";
import { api } from "@/lib/api";
import {
    LAB_OUTCOME_LABELS,
    REPRODUCTION_LABELS,
    formatTimestamp,
    labOutcomeTone,
    reproductionTone,
    shortDigest,
} from "@/lib/format";
import type { BundleVerification, ImportOutcome, ImportedBundleSummary, ValidationReport } from "@/lib/types";
import { useAction, useApi } from "@/lib/use-api";

const PAGE = 50;

export function EvidenceView() {
    const [digest, setDigest] = useQueryParam("bundle");
    const [extra, setExtra] = useState<ImportedBundleSummary[]>([]);
    const [cursor, setCursor] = useState<number | null>(null);
    const list = useApi(() => api.listBundles({ limit: PAGE }), "bundles:list");
    const more = useAction((before: number) => api.listBundles({ limit: PAGE, before }));
    const rows = [...(list.data?.bundles ?? []), ...extra];
    const next = extra.length > 0 ? cursor : (list.data?.next_before ?? null);
    const reload = () => {
        setExtra([]);
        setCursor(null);
        list.reload();
    };
    return (
        <>
            <PageHeader
                title="Evidence"
                description="Portable evidence bundles: a finished experiment with its record, artifacts, verification history and, for differential replays, its input events, under one integrity manifest. Validation checks consistency; reproduction re-executes where this platform can."
            />
            <div className="grid grid-cols-1 gap-6 xl:grid-cols-[minmax(0,1fr)_24rem]">
                <div className="flex min-w-0 flex-col gap-6">
                    <Card title="Imported bundles" padded={false}>
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
                        {list.data && rows.length === 0 ? (
                            <div className="p-4">
                                <EmptyState title="No imported bundles.">
                                    Export one from an experiment, then validate and import it with the form.
                                </EmptyState>
                            </div>
                        ) : null}
                        {rows.length > 0 ? (
                            <Table label="Imported bundles">
                                <thead>
                                    <tr>
                                        <Th>Bundle</Th>
                                        <Th>Kind</Th>
                                        <Th>Outcome in bundle</Th>
                                        <Th>Imported</Th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {rows.map((b) => (
                                        <tr
                                            key={b.bundle_digest}
                                            className={b.bundle_digest === digest ? "bg-surface-2" : undefined}
                                        >
                                            <Td>
                                                <button
                                                    className="font-mono text-xs text-accent hover:underline"
                                                    onClick={() => setDigest(b.bundle_digest)}
                                                    aria-label={`Show bundle ${b.bundle_digest}`}
                                                >
                                                    {shortDigest(b.bundle_digest, 8, 4)}
                                                </button>
                                            </Td>
                                            <Td className="font-mono text-xs">{b.kind}</Td>
                                            <Td>
                                                {b.status === "failed" ? (
                                                    <Badge tone="fail">experiment failed</Badge>
                                                ) : (
                                                    <Badge tone={labOutcomeTone(b.outcome)}>
                                                        {LAB_OUTCOME_LABELS[b.outcome ?? ""] ?? b.outcome ?? b.status}
                                                    </Badge>
                                                )}
                                            </Td>
                                            <Td className="text-xs">{formatTimestamp(b.imported_at)}</Td>
                                        </tr>
                                    ))}
                                </tbody>
                            </Table>
                        ) : null}
                        {next !== null ? (
                            <div className="p-4">
                                <Button
                                    busy={more.pending}
                                    onClick={async () => {
                                        const page = await more.run(next);
                                        if (page) {
                                            setExtra((e) => [...e, ...page.bundles]);
                                            setCursor(page.next_before);
                                        }
                                    }}
                                >
                                    Load more
                                </Button>
                            </div>
                        ) : null}
                    </Card>
                    {digest ? <BundlePanel digest={digest} /> : null}
                </div>
                <div className="flex min-w-0 flex-col gap-6">
                    <UploadBundle
                        onImported={(o) => {
                            reload();
                            setDigest(o.bundle.bundle_digest);
                        }}
                    />
                    <Card title="Exporting">
                        <p className="text-sm text-muted">
                            Open a finished experiment on the{" "}
                            <Link className="text-accent hover:underline" href="/experiments">
                                Experiments
                            </Link>{" "}
                            page and use <strong>Export bundle</strong>. A bundle is not signed: to detect a substituted
                            bundle, compare its digest with one obtained from a trusted source.
                        </p>
                    </Card>
                </div>
            </div>
        </>
    );
}

function UploadBundle({ onImported }: { onImported: (o: ImportOutcome) => void }) {
    const [file, setFile] = useState<File | null>(null);
    const [report, setReport] = useState<ValidationReport | null>(null);
    const [imported, setImported] = useState<ImportOutcome | null>(null);
    const validate = useAction((f: File) => api.validateBundle(f));
    const doImport = useAction((f: File) => api.importBundle(f));
    const rejected =
        doImport.error?.code === "invalid_bundle" ? (doImport.error.details as unknown as ValidationReport) : null;
    return (
        <Card title="Validate or import a bundle">
            <form className="flex flex-col gap-3" onSubmit={(e) => e.preventDefault()}>
                <Field
                    label="Bundle file"
                    htmlFor="bundle-file"
                    hint="The .json file exported by ORBIT, at most 32 MiB."
                >
                    <input
                        id="bundle-file"
                        type="file"
                        accept="application/json,.json"
                        className="text-sm"
                        onChange={(e) => {
                            setFile(e.target.files?.[0] ?? null);
                            setReport(null);
                            setImported(null);
                        }}
                    />
                </Field>
                <div className="flex flex-wrap gap-2">
                    <Button
                        type="button"
                        busy={validate.pending}
                        disabled={!file}
                        onClick={async () => {
                            if (!file) return;
                            const r = await validate.run(file);
                            if (r) setReport(r);
                        }}
                    >
                        Validate
                    </Button>
                    <Button
                        type="button"
                        variant="primary"
                        busy={doImport.pending}
                        disabled={!file}
                        onClick={async () => {
                            if (!file) return;
                            const o = await doImport.run(file);
                            if (o) {
                                setImported(o);
                                setReport(o.bundle.validation);
                                onImported(o);
                            }
                        }}
                    >
                        Import
                    </Button>
                </div>
            </form>
            {validate.error ? <ErrorState title="Validation failed" error={validate.error} /> : null}
            {doImport.error && !rejected ? <ErrorState title="Import failed" error={doImport.error} /> : null}
            {rejected ? (
                <div className="mt-3">
                    <p role="alert" className="mb-2 text-sm font-medium text-fail">
                        Not imported: the bundle is invalid.
                    </p>
                    <ValidationReportView report={rejected} />
                </div>
            ) : null}
            {imported ? (
                <p role="status" className="mt-3 text-sm text-muted">
                    {imported.newly_imported ? "Imported." : "Already imported; nothing changed."}{" "}
                    {imported.local_experiment.exists
                        ? "This lab has an experiment with the same id; it was not changed."
                        : "No local experiment was created."}
                </p>
            ) : null}
            {report && !rejected ? (
                <div className="mt-3">
                    <ValidationReportView report={report} />
                </div>
            ) : null}
        </Card>
    );
}

/** A validation report. Integrity and scientific correctness are shown separately. */
export function ValidationReportView({ report }: { report: ValidationReport }) {
    return (
        <div className="flex flex-col gap-3 text-sm">
            <span className="flex flex-wrap items-center gap-2">
                <Badge tone={report.valid ? "ok" : "fail"}>{report.valid ? "valid" : "invalid"}</Badge>
                <Badge tone={report.integrity === "verified" ? "ok" : "fail"}>integrity {report.integrity}</Badge>
                <Badge tone="neutral" title="Validation checks consistency, not whether the result is right">
                    scientific correctness not verified
                </Badge>
                <Badge
                    tone={
                        report.history === "included_and_verified"
                            ? "ok"
                            : report.history === "not_included"
                              ? "warn"
                              : "fail"
                    }
                >
                    history {report.history.replaceAll("_", " ")}
                </Badge>
            </span>
            {report.bundle_digest ? (
                <p className="text-xs text-muted">
                    Bundle digest <Digest value={report.bundle_digest} />
                </p>
            ) : null}
            {report.errors.length > 0 ? (
                <ul aria-label="Validation errors" className="flex flex-col gap-1">
                    {report.errors.map((e, i) => (
                        <li key={i} className="break-words text-xs">
                            <Mono className="text-fail">{e.code}</Mono> {e.message}
                        </li>
                    ))}
                </ul>
            ) : null}
            {report.valid ? (
                <p className="text-xs text-muted">
                    {report.independently_replayable
                        ? "Replayable from the bundle: "
                        : "Not replayable from the bundle alone: "}
                    {report.replayability}
                </p>
            ) : null}
            {report.passed.length > 0 ? (
                <p className="text-xs text-faint">Checks passed: {report.passed.join(", ")}</p>
            ) : null}
            <ul aria-label="What validation shows" className="list-disc pl-5 text-xs text-muted">
                {report.limitations.map((l) => (
                    <li key={l}>{l}</li>
                ))}
            </ul>
        </div>
    );
}

function BundlePanel({ digest }: { digest: string }) {
    const detail = useApi(() => api.getBundle(digest), `bundle:${digest}`);
    const reproduce = useAction(() => api.reproduceBundle(digest));
    const count = detail.data?.verifications.length ?? 0;
    const { reload } = detail;
    // A performance bundle is measured in the background: poll until a new
    // attempt appears (the count of attempts when it was started is `awaited`).
    const [awaited, setAwaited] = useState<number | null>(null);
    const measuring = awaited !== null && count <= awaited;
    useEffect(() => {
        if (!measuring) return;
        const timer = setInterval(reload, 1000);
        return () => clearInterval(timer);
    }, [measuring, reload]);
    if (detail.loading && !detail.data) return <Loading />;
    if (detail.error) return <ErrorState error={detail.error} onRetry={detail.reload} />;
    if (!detail.data) return null;
    const { bundle, verifications } = detail.data;
    const doc = bundle.document.document;
    const e = doc.experiment;
    return (
        <>
            <Card title="Bundle">
                <KeyValue
                    items={[
                        ["Digest", <Digest key="d" value={bundle.bundle_digest} full />],
                        ["Experiment", <Mono key="e">{`${e.id} (${e.kind})`}</Mono>],
                        [
                            "Outcome in bundle",
                            e.status === "failed" ? (
                                <Badge key="o" tone="fail">
                                    experiment failed
                                </Badge>
                            ) : (
                                <Badge key="o" tone={labOutcomeTone(e.outcome)}>
                                    {LAB_OUTCOME_LABELS[e.outcome ?? ""] ?? e.outcome}
                                </Badge>
                            ),
                        ],
                        [
                            "Exported by",
                            <Mono key="x">
                                {bundle.document.manifest.exporter.orbit_version}
                                {bundle.document.manifest.exporter.git_commit
                                    ? ` @ ${bundle.document.manifest.exporter.git_commit.slice(0, 12)}`
                                    : ""}
                            </Mono>,
                        ],
                        [
                            "Input history",
                            doc.history.included
                                ? `included: ${doc.history.events} events, digest chain verified`
                                : `not included (${doc.history.reason ?? "no reason given"})`,
                        ],
                        ["Imported", formatTimestamp(bundle.imported_at)],
                    ]}
                />
                <h3 className="mb-1 mt-4 text-sm font-medium text-text">Entries</h3>
                <ul aria-label="Bundle entries" className="flex flex-col gap-1 text-xs">
                    {bundle.document.manifest.entries.map((en) => (
                        <li key={en.name} className="flex flex-wrap items-center gap-2">
                            <Mono>{en.name}</Mono>
                            <span className="text-faint">{en.size} bytes</span>
                            <Digest value={en.sha256} />
                        </li>
                    ))}
                </ul>
            </Card>
            <Card
                title="Reproduction"
                actions={
                    <Button
                        busy={reproduce.pending || measuring}
                        onClick={async () => {
                            const r = await reproduce.run();
                            if (!r) return;
                            if ("verified_at" in r) {
                                detail.reload();
                            } else {
                                setAwaited(count);
                            }
                        }}
                    >
                        Reproduce
                    </Button>
                }
            >
                {reproduce.error ? <ErrorState title="Reproduction failed" error={reproduce.error} /> : null}
                {measuring ? (
                    <p role="status" className="mb-2 text-sm text-muted">
                        Measuring the bundled workload again on this server…
                    </p>
                ) : null}
                <BundleAttempts items={verifications} />
            </Card>
        </>
    );
}

function BundleAttempts({ items }: { items: BundleVerification[] }) {
    if (items.length === 0) return <p className="text-sm text-muted">No reproduction attempt yet.</p>;
    return (
        <ul aria-label="Reproduction attempts" className="flex flex-col gap-2 text-sm">
            {items.map((v) => {
                const d = (v.detail ?? {}) as Record<string, unknown>;
                const differences = Array.isArray(d.differences) ? (d.differences as string[]) : [];
                return (
                    <li key={v.id} className="flex flex-col gap-1">
                        <span className="flex flex-wrap items-center gap-2">
                            <Badge tone={reproductionTone(v.status)}>{REPRODUCTION_LABELS[v.status] ?? v.status}</Badge>
                            <span className="text-xs text-faint">{formatTimestamp(v.verified_at)}</span>
                        </span>
                        <span className="pl-1 text-xs text-muted">
                            {typeof d.reason === "string" ? d.reason : null}
                            {d.compared === "full_result" ? "Compared the full result document and both traces." : null}
                            {differences.length > 0 ? ` Differs in: ${differences.join(", ")}.` : null}
                            {typeof d.attempt_experiment_id === "string" ? (
                                <>
                                    {" "}
                                    New measurement:{" "}
                                    <Link
                                        className="text-accent hover:underline"
                                        href={`/experiments?experiment=${d.attempt_experiment_id}`}
                                    >
                                        {shortDigest(d.attempt_experiment_id, 8, 4)}
                                    </Link>
                                </>
                            ) : null}
                        </span>
                    </li>
                );
            })}
        </ul>
    );
}
