"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

export const NAV_GROUPS: { label: string; items: { href: string; label: string }[] }[] = [
    {
        label: "Event store",
        items: [
            { href: "/", label: "Overview" },
            { href: "/streams", label: "Streams" },
            { href: "/events", label: "Events" },
            { href: "/time-travel", label: "Time Travel" },
            { href: "/snapshots", label: "Snapshots" },
            { href: "/integrity", label: "Integrity" },
        ],
    },
    {
        label: "Lab",
        items: [
            { href: "/replay", label: "Replay Lab" },
            { href: "/experiments", label: "Experiments" },
            { href: "/performance", label: "Performance" },
            { href: "/evidence", label: "Evidence" },
            { href: "/benchmarks", label: "Benchmarks" },
        ],
    },
];

export const NAV_ITEMS = NAV_GROUPS.flatMap((g) => g.items);

export function isActive(pathname: string, href: string): boolean {
    return href === "/" ? pathname === "/" : pathname === href || pathname.startsWith(`${href}/`);
}

export function Nav() {
    const pathname = usePathname() ?? "/";
    return (
        <nav aria-label="Main" className="flex flex-row flex-wrap gap-x-4 gap-y-2 md:flex-col">
            {NAV_GROUPS.map((group) => (
                <div key={group.label} className="flex flex-col gap-0.5">
                    <span className="px-3 text-[11px] font-semibold uppercase tracking-wider text-faint">
                        {group.label}
                    </span>
                    <ul className="flex flex-row flex-wrap gap-0.5 md:flex-col">
                        {group.items.map((item) => {
                            const active = isActive(pathname, item.href);
                            return (
                                <li key={item.href}>
                                    <Link
                                        href={item.href}
                                        aria-current={active ? "page" : undefined}
                                        className={
                                            active
                                                ? "block rounded-md bg-surface-3 px-3 py-1.5 text-sm font-medium text-text"
                                                : "block rounded-md px-3 py-1.5 text-sm text-muted hover:bg-surface-2 hover:text-text"
                                        }
                                    >
                                        {item.label}
                                    </Link>
                                </li>
                            );
                        })}
                    </ul>
                </div>
            ))}
        </nav>
    );
}
