import { expect, test } from "@playwright/test";

// Performance measurements and evidence bundles through the Observatory,
// against the real API, event store and lab registry.

const API = "http://127.0.0.1:7879/api/v1";

test("measure, compare and inspect a performance workload", async ({ page }) => {
    await page.goto("/performance");
    await expect(page.getByRole("heading", { level: 1, name: "Performance" })).toBeVisible();
    for (const seed of ["1", "1"]) {
        await page.getByLabel("Workload").selectOption("integrity-verification");
        await page.getByLabel(/Input size/).fill("50");
        await page.getByLabel("Warm-up iterations").fill("0");
        await page.getByLabel("Measured iterations").fill("4");
        await page.getByLabel("Seed").fill(seed);
        await expect(page.getByRole("button", { name: "Measure" })).toBeEnabled({ timeout: 30_000 });
        await page.getByRole("button", { name: "Measure" }).click();
        await expect(page.getByRole("status").filter({ hasText: "measured in the background" })).toBeVisible();
    }
    const table = page.getByRole("table", { name: "Performance experiments" });
    await expect(table.getByRole("row")).toHaveCount(3);
    // The e2e API is a debug build, so both are smoke runs.
    await expect(table.getByText("Smoke run (not a measurement)")).toHaveCount(2, { timeout: 30_000 });

    const boxes = table.getByRole("checkbox");
    await boxes.first().check();
    await page.getByRole("button", { name: /Compare 1/ }).click();
    const comparisons = page.getByRole("list", { name: "Comparisons" });
    // Four samples are too few for the significance test: never a supported finding.
    await expect(comparisons).toBeVisible();
    await expect(comparisons).not.toContainText("statistically supported)");
    await expect(comparisons).toContainText(/Within threshold|Suspected/);

    await table.getByRole("link").first().click();
    await expect(page.getByRole("table", { name: "Statistics" })).toBeVisible();
    await expect(page.getByRole("img", { name: /4 measured iterations/ })).toBeVisible();

    // Fits a phone-width screen.
    await page.setViewportSize({ width: 375, height: 800 });
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow).toBeLessThanOrEqual(0);
});

test("export, validate, import and reproduce an evidence bundle", async ({ page, request }) => {
    const stream = `e2e-bundle-${Date.now()}`;
    expect((await request.post(`${API}/streams`, { data: { stream_id: stream } })).status()).toBe(201);
    for (const [type, by] of [
        ["counter.incremented", 2],
        ["counter.decremented", 5],
    ] as const) {
        await request.post(`${API}/streams/${stream}/events`, { data: { event_type: type, payload: { by } } });
    }
    const created = await request.post(`${API}/lab/experiments`, {
        data: {
            kind: "differential_replay",
            spec: { stream, left: "orbit.counter", right: "example.counter-floor-at-zero" },
        },
    });
    const id = (await created.json()).id as string;

    // The export link on the experiment downloads the bundle.
    await page.goto(`/experiments?experiment=${id}`);
    const downloading = page.waitForEvent("download");
    await page.getByRole("link", { name: "Export bundle" }).click();
    const download = await downloading;
    expect(download.suggestedFilename()).toBe(`orbit-evidence-${id}.json`);
    const file = await download.path();

    await page.goto("/evidence");
    await expect(page.getByRole("heading", { level: 1, name: "Evidence" })).toBeVisible();
    await page.getByLabel("Bundle file").setInputFiles(file);
    await page.getByRole("button", { name: "Validate" }).click();
    await expect(page.getByText("integrity verified")).toBeVisible();
    await expect(page.getByText("scientific correctness not verified")).toBeVisible();
    await expect(page.getByText("history included and verified")).toBeVisible();

    await page.getByRole("button", { name: "Import" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Imported." })).toBeVisible();
    await expect(page.getByRole("list", { name: "Bundle entries" })).toContainText("history/events.json");
    await page.getByRole("button", { name: "Reproduce" }).click();
    const attempts = page.getByRole("list", { name: "Reproduction attempts" });
    await expect(attempts).toContainText("Reproduced");
    await expect(attempts).toContainText("Compared the full result document and both traces.");

    // A tampered copy is refused and not imported.
    const bytes = await (await request.get(`${API}/lab/experiments/${id}/bundle`)).text();
    const tampered = bytes.replace('"outcome":"diverged"', '"outcome":"equivalent"');
    expect(tampered).not.toBe(bytes);
    await page.getByLabel("Bundle file").setInputFiles({
        name: "tampered.json",
        mimeType: "application/json",
        buffer: Buffer.from(tampered),
    });
    await page.getByRole("button", { name: "Import" }).click();
    await expect(page.getByText("Not imported: the bundle is invalid.")).toBeVisible();
    await expect(page.getByRole("list", { name: "Validation errors" })).toContainText("digest_mismatch");
    const list = await (await request.get(`${API}/lab/bundles`)).json();
    expect(list.bundles).toHaveLength(1);
});
