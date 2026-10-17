import type { ReactNode } from "react";

import { HealthIndicator } from "./health-indicator";
import { Nav } from "./nav";

export function AppShell({ children }: { children: ReactNode }) {
    return (
        <div className="flex min-h-screen flex-col md:flex-row">
            <a
                href="#main"
                className="sr-only focus:not-sr-only focus:absolute focus:left-2 focus:top-2 focus:z-50 focus:rounded focus:bg-surface-3 focus:px-3 focus:py-1"
            >
                Skip to content
            </a>
            <aside className="border-b border-border bg-surface md:sticky md:top-0 md:h-screen md:w-56 md:shrink-0 md:border-b-0 md:border-r">
                <div className="flex h-full flex-col gap-6 p-4">
                    <div>
                        <p className="font-mono text-sm font-semibold tracking-[0.2em] text-text">ORBIT</p>
                        <p className="text-xs text-faint">Observatory</p>
                    </div>
                    <Nav />
                    <div className="mt-auto hidden md:block">
                        <HealthIndicator />
                    </div>
                </div>
            </aside>
            <main id="main" className="min-w-0 flex-1 px-4 py-6 md:px-8">
                <div className="mb-4 md:hidden">
                    <HealthIndicator />
                </div>
                <div className="mx-auto max-w-7xl">{children}</div>
            </main>
        </div>
    );
}
