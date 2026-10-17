import type { Metadata } from "next";
import type { ReactNode } from "react";

import { AppShell } from "@/components/app-shell";
import "./globals.css";

export const metadata: Metadata = {
    title: { default: "ORBIT Observatory", template: "%s · ORBIT Observatory" },
    description: "Event-store observability and time-travel debugging for ORBIT.",
};

export default function RootLayout({ children }: { children: ReactNode }) {
    return (
        <html lang="en" className="h-full antialiased">
            <body className="min-h-full">
                <AppShell>{children}</AppShell>
            </body>
        </html>
    );
}
