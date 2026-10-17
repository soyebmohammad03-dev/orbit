import { Suspense } from "react";

import { Loading } from "@/components/ui";
import { BenchmarksView } from "./view";

export const metadata = { title: "Benchmarks" };

export default function Page() {
    return (
        <Suspense fallback={<Loading />}>
            <BenchmarksView />
        </Suspense>
    );
}
