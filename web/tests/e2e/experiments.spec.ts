import { expect, test } from "@playwright/test";

// Differential replay through the Observatory, against the real API, event
// store and lab registry.

const API = "http://127.0.0.1:7879/api/v1";

test("run, inspect, verify and reproduce a differential replay", async ({ page, request }) => {
    const stream = `e2e-counter-${Date.now()}`;
    expect((await request.post(`${API}/streams`, { data: { stream_id: stream } })).status()).toBe(201);
    for (const [type, by] of [
        ["counter.incremented", 2],
        ["counter.decremented", 5],
        ["counter.incremented", 1],
    ] as const) {
        const r = await request.post(`${API}/streams/${stream}/events`, {
            data: { event_type: type, payload: { by } },
        });
        expect(r.status()).toBe(201);
    }

    await page.goto("/experiments");
    await expect(page.getByRole("heading", { level: 1, name: "Experiments" })).toBeVisible();
    await page.getByLabel("Stream").selectOption(stream);
    await page.getByLabel("Left implementation").selectOption("orbit.counter");
    await page.getByLabel("Right implementation").selectOption("example.counter-floor-at-zero");
    await page.getByLabel("Reference").selectOption("left");
    await page.getByRole("button", { name: "Run differential replay" }).click();

    await expect(page.getByRole("table", { name: "Experiments" })).toContainText(stream);
    await expect(page.getByText(/First divergence \(after 2 matching/)).toBeVisible();
    const diff = page.getByRole("list", { name: "State differences" }).first();
    await expect(diff).toContainText("/value");
    await expect(diff).toContainText("-3");

    await page.getByRole("button", { name: "Verify artifacts" }).click();
    await expect(page.getByRole("list", { name: "Verification history" })).toContainText("intact");
    await page.getByRole("button", { name: "Reproduce" }).click();
    await expect(page.getByRole("list", { name: "Verification history" })).toContainText("reproduced");

    // The API agrees, and the event store was only read.
    const page1 = await (await request.get(`${API}/lab/experiments?limit=1`)).json();
    const id = page1.experiments[0].id;
    const detail = await (await request.get(`${API}/lab/experiments/${id}`)).json();
    expect(detail.experiment.outcome).toBe("diverged");
    expect(detail.experiment.result.first_divergence.boundary).toBe(2);
    expect(detail.verifications.map((v: { status: string }) => v.status)).toEqual(["intact", "reproduced"]);
    const s = await (await request.get(`${API}/streams/${stream}`)).json();
    expect(s.head_sequence).toBe(3);

    // The page, with a result open, fits a phone-width screen.
    await page.setViewportSize({ width: 375, height: 800 });
    await expect(page.getByText(/First divergence/)).toBeVisible();
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow).toBeLessThanOrEqual(0);
});
