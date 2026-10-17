"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

export const NAV_ITEMS = [
    { href: "/", label: "Overview" },
    { href: "/streams", label: "Streams" },
    { href: "/events", label: "Events" },
    { href: "/time-travel", label: "Time Travel" },
    { href: "/snapshots", label: "Snapshots" },
    { href: "/replay", label: "Replay Lab" },
    { href: "/integrity", label: "Integrity" },
    { href: "/benchmarks", label: "Benchmarks" },
] as const;

export function isActive(pathname: string, href: string): boolean {
    return href === "/" ? pathname === "/" : pathname === href || pathname.startsWith(`${href}/`);
}

export function Nav() {
    const pathname = usePathname() ?? "/";
    return (
        <nav aria-label="Main" className="flex flex-row flex-wrap gap-0.5 md:flex-col">
            {NAV_ITEMS.map((item) => {
                const active = isActive(pathname, item.href);
                return (
                    <Link
                        key={item.href}
                        href={item.href}
                        aria-current={active ? "page" : undefined}
                        className={
                            active
                                ? "rounded-md bg-surface-3 px-3 py-1.5 text-sm font-medium text-text"
                                : "rounded-md px-3 py-1.5 text-sm text-muted hover:bg-surface-2 hover:text-text"
                        }
                    >
                        {item.label}
                    </Link>
                );
            })}
        </nav>
    );
}
