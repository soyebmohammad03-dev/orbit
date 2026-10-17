import { Suspense } from "react";

import { Loading } from "@/components/ui";
import { EventsView } from "./view";

export const metadata = { title: "Events" };

export default function Page() {
    return (
        <Suspense fallback={<Loading />}>
            <EventsView />
        </Suspense>
    );
}
