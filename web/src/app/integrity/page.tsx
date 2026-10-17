import { Suspense } from "react";

import { Loading } from "@/components/ui";
import { IntegrityView } from "./view";

export const metadata = { title: "Integrity" };

export default function Page() {
    return (
        <Suspense fallback={<Loading />}>
            <IntegrityView />
        </Suspense>
    );
}
