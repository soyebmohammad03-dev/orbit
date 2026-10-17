import Link from "next/link";
import type { ComponentProps, ReactNode } from "react";

import type { Tone } from "@/lib/format";
import { shortDigest } from "@/lib/format";

function cx(...classes: (string | false | null | undefined)[]): string {
    return classes.filter(Boolean).join(" ");
}

export function PageHeader({
    title,
    description,
    actions,
}: {
    title: string;
    description?: ReactNode;
    actions?: ReactNode;
}) {
    return (
        <header className="mb-6 flex flex-wrap items-end justify-between gap-4 border-b border-border pb-4">
            <div>
                <h1 className="text-lg font-semibold tracking-tight text-text">{title}</h1>
                {description ? <p className="mt-1 max-w-3xl text-sm text-muted">{description}</p> : null}
            </div>
            {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
        </header>
    );
}

export function Card({
    title,
    actions,
    children,
    className,
    padded = true,
}: {
    title?: ReactNode;
    actions?: ReactNode;
    children: ReactNode;
    className?: string;
    padded?: boolean;
}) {
    return (
        <section className={cx("min-w-0 rounded-lg border border-border bg-surface", className)}>
            {title || actions ? (
                <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-4 py-2.5">
                    {title ? (
                        <h2 className="text-xs font-semibold uppercase tracking-wider text-muted">{title}</h2>
                    ) : (
                        <span />
                    )}
                    {actions}
                </div>
            ) : null}
            <div className={padded ? "p-4" : undefined}>{children}</div>
        </section>
    );
}

const toneClasses: Record<Tone, string> = {
    neutral: "bg-surface-3 text-muted border-border-strong",
    ok: "bg-ok-soft text-ok border-ok/30",
    warn: "bg-warn-soft text-warn border-warn/30",
    fail: "bg-fail-soft text-fail border-fail/30",
    info: "bg-accent-soft text-accent border-accent/30",
};

export function Badge({ tone = "neutral", children, title }: { tone?: Tone; children: ReactNode; title?: string }) {
    return (
        <span
            title={title}
            className={cx(
                "inline-flex items-center gap-1 rounded border px-1.5 py-0.5 text-[11px] font-medium leading-none",
                toneClasses[tone],
            )}
        >
            {children}
        </span>
    );
}

type ButtonProps = ComponentProps<"button"> & { variant?: "primary" | "secondary" | "ghost"; busy?: boolean };

export function Button({ variant = "secondary", busy, disabled, className, children, ...rest }: ButtonProps) {
    const styles = {
        primary: "bg-accent text-bg hover:bg-accent/90 border-transparent font-semibold",
        secondary: "bg-surface-3 text-text hover:bg-surface-2 border-border-strong",
        ghost: "bg-transparent text-muted hover:text-text border-transparent",
    }[variant];
    return (
        <button
            {...rest}
            disabled={disabled || busy}
            aria-busy={busy || undefined}
            className={cx(
                "inline-flex h-8 items-center gap-2 rounded-md border px-3 text-sm transition-colors disabled:cursor-not-allowed disabled:opacity-50",
                styles,
                className,
            )}
        >
            {busy ? <Spinner /> : null}
            {children}
        </button>
    );
}

export function Spinner() {
    return (
        <span
            aria-hidden
            className="inline-block size-3 animate-spin rounded-full border-2 border-current border-r-transparent"
        />
    );
}

export function Loading({ label = "Loading" }: { label?: string }) {
    return (
        <div role="status" aria-live="polite" className="flex items-center gap-2 py-6 text-sm text-muted">
            <Spinner />
            <span>{label}…</span>
        </div>
    );
}

export function EmptyState({ title, children, action }: { title: string; children?: ReactNode; action?: ReactNode }) {
    return (
        <div className="flex flex-col items-start gap-2 rounded-md border border-dashed border-border-strong px-4 py-6">
            <p className="text-sm font-medium text-text">{title}</p>
            {children ? <div className="max-w-2xl text-sm text-muted">{children}</div> : null}
            {action}
        </div>
    );
}

export function ErrorState({
    error,
    onRetry,
    title = "Request failed",
}: {
    error: { message: string; code?: string; status?: number };
    onRetry?: () => void;
    title?: string;
}) {
    return (
        <div role="alert" className="rounded-md border border-fail/40 bg-fail-soft px-4 py-3">
            <div className="flex flex-wrap items-center justify-between gap-3">
                <div className="min-w-0">
                    <p className="text-sm font-semibold text-fail">
                        {title}
                        {error.code ? (
                            <span className="ml-2 font-mono text-xs font-normal text-fail/80">{error.code}</span>
                        ) : null}
                        {error.status ? (
                            <span className="ml-2 font-mono text-xs font-normal text-fail/80">HTTP {error.status}</span>
                        ) : null}
                    </p>
                    <p className="mt-1 break-words text-sm text-text/90">{error.message}</p>
                </div>
                {onRetry ? (
                    <Button onClick={onRetry} variant="secondary">
                        Retry
                    </Button>
                ) : null}
            </div>
        </div>
    );
}

export function Digest({ value, full = false }: { value: string; full?: boolean }) {
    return (
        <code title={value} className="whitespace-nowrap font-mono text-xs text-muted">
            {full ? value : shortDigest(value)}
        </code>
    );
}

export function Mono({ children, className }: { children: ReactNode; className?: string }) {
    return <code className={cx("font-mono text-xs", className)}>{children}</code>;
}

export function KeyValue({ items }: { items: [ReactNode, ReactNode][] }) {
    return (
        <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1.5 text-sm">
            {items.map(([k, v], i) => (
                <div key={i} className="contents">
                    <dt className="text-muted">{k}</dt>
                    <dd className="min-w-0 break-words text-text">{v}</dd>
                </div>
            ))}
        </dl>
    );
}

export function Stat({ label, value, hint }: { label: string; value: ReactNode; hint?: ReactNode }) {
    return (
        <div className="rounded-lg border border-border bg-surface px-4 py-3">
            <p className="text-xs uppercase tracking-wider text-muted">{label}</p>
            <p className="mt-1 font-mono text-xl text-text">{value}</p>
            {hint ? <p className="mt-0.5 text-xs text-faint">{hint}</p> : null}
        </div>
    );
}

export function Field({
    label,
    hint,
    children,
    htmlFor,
}: {
    label: string;
    hint?: ReactNode;
    children: ReactNode;
    htmlFor: string;
}) {
    return (
        <div className="flex flex-col gap-1">
            <label htmlFor={htmlFor} className="text-xs font-medium text-muted">
                {label}
            </label>
            {children}
            {hint ? <p className="text-xs text-faint">{hint}</p> : null}
        </div>
    );
}

const inputClass =
    "h-8 rounded-md border border-border-strong bg-surface-2 px-2.5 text-sm text-text placeholder:text-faint focus:border-accent focus:outline-none";

export function Input(props: ComponentProps<"input">) {
    return <input {...props} className={cx(inputClass, props.className)} />;
}

export function Select(props: ComponentProps<"select">) {
    return <select {...props} className={cx(inputClass, "pr-8", props.className)} />;
}

export function TextArea(props: ComponentProps<"textarea">) {
    return (
        <textarea
            {...props}
            className={cx(
                "min-h-24 rounded-md border border-border-strong bg-surface-2 px-2.5 py-2 font-mono text-xs text-text placeholder:text-faint focus:border-accent focus:outline-none",
                props.className,
            )}
        />
    );
}

export function Table({ children, label }: { children: ReactNode; label: string }) {
    return (
        <div className="overflow-x-auto">
            <table aria-label={label} className="w-full border-collapse text-left text-sm">
                {children}
            </table>
        </div>
    );
}

export function Th({ children, className }: { children?: ReactNode; className?: string }) {
    return (
        <th
            scope="col"
            className={cx(
                "border-b border-border px-3 py-2 text-xs font-medium uppercase tracking-wider text-muted",
                className,
            )}
        >
            {children}
        </th>
    );
}

export function Td({ children, className }: { children?: ReactNode; className?: string }) {
    return <td className={cx("border-b border-border/60 px-3 py-2 align-top", className)}>{children}</td>;
}

export function TextLink({ href, children }: { href: string; children: ReactNode }) {
    return (
        <Link href={href} className="text-accent hover:underline">
            {children}
        </Link>
    );
}
