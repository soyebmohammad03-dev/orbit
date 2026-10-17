import { Suspense } from "react";

import { Loading } from "@/components/ui";
import { SnapshotsView } from "./view";

export const metadata = { title: "Snapshots" };

export default function Page() {
    return (
        <Suspense fallback={<Loading />}>
            <SnapshotsView />
        </Suspense>
    );
}
