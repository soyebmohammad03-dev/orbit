import { Suspense } from "react";

import { Loading } from "@/components/ui";
import { EvidenceView } from "./view";

export const metadata = { title: "Evidence" };

export default function Page() {
    return (
        <Suspense fallback={<Loading />}>
            <EvidenceView />
        </Suspense>
    );
}
