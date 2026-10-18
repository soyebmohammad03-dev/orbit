import { Suspense } from "react";

import { Loading } from "@/components/ui";
import { PerformanceView } from "./view";

export const metadata = { title: "Performance" };

export default function Page() {
    return (
        <Suspense fallback={<Loading />}>
            <PerformanceView />
        </Suspense>
    );
}
