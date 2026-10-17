import { Suspense } from "react";

import { Loading } from "@/components/ui";
import { ReplayView } from "./view";

export const metadata = { title: "Replay Lab" };

export default function Page() {
    return (
        <Suspense fallback={<Loading />}>
            <ReplayView />
        </Suspense>
    );
}
