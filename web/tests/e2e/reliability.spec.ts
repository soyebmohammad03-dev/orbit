import { expect, test } from "@playwright/test";

// A reliability scenario launched from the Observatory, executed by the
// separately built orbit-reliability runner, with real child processes.

const API = "http://127.0.0.1:7879/api/v1";

test("launch a reliability scenario and inspect its real result", async ({ page, request }) => {
    const catalog = await (await request.get(`${API}/lab/reliability/scenarios`)).json();
    expect(catalog.runner.enabled).toBe(true);

    await page.goto("/experiments");
    const form = page.getByRole("form", { name: "Launch a reliability scenario" });
    await form.getByLabel("Scenario").selectOption("snapshot-write-interruption");
    await form.getByLabel(/events/).fill("2");
    await form.getByRole("button", { name: "Launch scenario" }).click();

    // The lifecycle comes from the registry: created/running, then completed.
    await expect(page.getByText("Invariants preserved").first()).toBeVisible({ timeout: 120_000 });
    const cases = page.getByRole("list", { name: "Reliability cases" });
    await expect(cases.getByRole("listitem")).toHaveCount(6);
    await expect(cases).toContainText("real process · process abort");
    await expect(page.getByText(/6 cases: 6 preserved, 0 violated, 0 inconclusive/)).toBeVisible();

    // The API agrees, and the run went through the runner (the spec is the one submitted).
    const list = await (await request.get(`${API}/lab/experiments?kind=reliability&limit=1`)).json();
    const detail = await (await request.get(`${API}/lab/experiments/${list.experiments[0].id}`)).json();
    expect(detail.experiment.status).toBe("completed");
    expect(detail.experiment.spec).toEqual({
        scenario: "snapshot-write-interruption",
        scenario_version: 1,
        parameters: { events: 2 },
    });
    // Requests outside the catalog are refused before anything runs.
    const bad = await request.post(`${API}/lab/reliability/runs`, { data: { scenario: "x", command: "id" } });
    expect(bad.status()).toBe(400);

    await page.setViewportSize({ width: 375, height: 800 });
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow).toBeLessThanOrEqual(0);
});
