import { EmptyState, PageHeader, TextLink } from "@/components/ui";

export default function NotFound() {
    return (
        <>
            <PageHeader title="Page not found" />
            <EmptyState title="There is no page at this address.">
                <TextLink href="/">Back to the overview</TextLink>
            </EmptyState>
        </>
    );
}
