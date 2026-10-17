import { Suspense } from "react";

import { Loading } from "@/components/ui";
import { TimeTravelView } from "./view";

export const metadata = { title: "Time Travel" };

export default function Page() {
    return (
        <Suspense fallback={<Loading />}>
            <TimeTravelView />
        </Suspense>
    );
}
