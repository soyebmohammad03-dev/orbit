import { Suspense } from "react";

import { Loading } from "@/components/ui";
import { ExperimentsView } from "./view";

export const metadata = { title: "Experiments" };

export default function Page() {
    return (
        <Suspense fallback={<Loading />}>
            <ExperimentsView />
        </Suspense>
    );
}
