import { Suspense } from "react";

import { Loading } from "@/components/ui";
import { OverviewView } from "./overview-view";

export default function Page() {
    return (
        <Suspense fallback={<Loading />}>
            <OverviewView />
        </Suspense>
    );
}
