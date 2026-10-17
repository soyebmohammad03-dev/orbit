import { Suspense } from "react";

import { Loading } from "@/components/ui";
import { StreamsView } from "./view";

export const metadata = { title: "Streams" };

export default function Page() {
    return (
        <Suspense fallback={<Loading />}>
            <StreamsView />
        </Suspense>
    );
}
