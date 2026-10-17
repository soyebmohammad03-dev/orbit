import { expect, test } from "@playwright/test";

// The complete Observatory workflow against the real API and database.
// Every important step is checked twice: in the UI, and against the API's
// actual returned state.

const API = "http://127.0.0.1:7879/api/v1";

test("create, append, time-travel, snapshot, replay and verify", async ({ page, request }) => {
    const stream = `e2e-orders-${Date.now()}`;

    // 1–2. Health.
    await page.goto("/");
    await expect(page.getByRole("status").filter({ hasText: "API v1" }).first()).toBeVisible();
    await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();

    // 3. Create a stream.
    await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Streams" }).click();
    await page.getByLabel("Stream id").fill(stream);
    await page.getByLabel("Metadata (optional JSON object)").fill('{"suite": "e2e"}');
    await page.getByRole("button", { name: "Create stream" }).click();
    await expect(page.getByRole("table", { name: "Streams" })).toContainText(stream);
    await expect(page.getByLabel("Stream metadata")).toContainText('"suite": "e2e"');
    const created = await (await request.get(`${API}/streams/${stream}`)).json();
    expect(created.metadata).toEqual({ suite: "e2e" });

    // 4. Append events through the Event Explorer.
    await page.goto(`/events?stream=${stream}`);
    const events = [
        ["order.created", '{"status": "new", "items": {"A": 1}}'],
        ["order.paid", '{"status": "paid", "total": 30}'],
        ["order.item_removed", '{"items": {"A": null}}'],
    ];
    for (const [type, payload] of events) {
        await page.getByLabel("Event type").fill(type);
        await page.getByLabel("Payload (JSON)").fill(payload);
        await page.getByRole("button", { name: "Append" }).click();
        await expect(page.getByRole("table", { name: "Events" })).toContainText(type);
    }

    // 5. Event history: deterministic order, inspection.
    const table = page.getByRole("table", { name: "Events" });
    await expect(table.getByRole("row")).toHaveCount(4);
    await page.getByRole("button", { name: "Inspect event 2" }).click();
    await expect(page.getByLabel("Event payload")).toContainText('"total": 30');
    const page1 = await (await request.get(`${API}/streams/${stream}/events`)).json();
    expect(page1.events.map((e: { sequence: number }) => e.sequence)).toEqual([1, 2, 3]);

    // Optimistic concurrency: another writer moves the head behind the page's back.
    await page.getByLabel(/Only append if the head is still/).check();
    const sneaky = await request.post(`${API}/streams/${stream}/events`, {
        data: { event_type: "order.noted", payload: { note: "from elsewhere" } },
    });
    expect(sneaky.status()).toBe(201);
    await page.getByLabel("Event type").fill("order.shipped");
    await page.getByLabel("Payload (JSON)").fill('{"status": "shipped"}');
    await page.getByRole("button", { name: "Append" }).click();
    await expect(page.getByRole("alert").filter({ hasText: "Conflict: the stream moved" })).toContainText(
        "actual head is #4",
    );
    const head = await (await request.get(`${API}/streams/${stream}`)).json();
    expect(head.head_sequence).toBe(4);

    // 6–8. Reconstruct current and historical state, and compare them.
    const current = await (await request.get(`${API}/streams/${stream}/state`)).json();
    expect(current.state).toEqual({ status: "paid", total: 30, items: {}, note: "from elsewhere" });
    await page.goto(`/time-travel?stream=${stream}&at=1`);
    await expect(page.getByLabel("Reconstructed state").first()).toContainText('"status": "new"');
    await expect(page.getByLabel("Reconstructed state").nth(1)).toContainText('"note": "from elsewhere"');
    const diff = page.getByRole("list", { name: "State differences" });
    await expect(diff).toContainText("/status");
    await expect(diff).toContainText("/items/A");
    await expect(diff).toContainText("/total");
    const apiDiff = await (await request.get(`${API}/streams/${stream}/diff?from=1`)).json();
    expect(apiDiff.changes.map((c: { path: string }) => c.path)).toEqual(["/items/A", "/note", "/status", "/total"]);

    // 9. Create and inspect a snapshot.
    await page.goto(`/snapshots?stream=${stream}`);
    await page.getByRole("button", { name: "Create snapshot" }).click();
    const snapshots = page.getByRole("table", { name: "Snapshots" });
    await expect(snapshots).toContainText("#4");
    await expect(snapshots).toContainText("usable");
    await expect(page.getByLabel("Snapshot state")).toContainText('"total": 30');
    const listed = await (await request.get(`${API}/streams/${stream}/snapshots`)).json();
    expect(listed).toHaveLength(1);
    expect(listed[0].validation.status).toBe("usable");
    const event4 = await (await request.get(`${API}/streams/${stream}/events/4`)).json();
    expect(listed[0].event_digest).toBe(event4.digest);
    const assisted = await (await request.get(`${API}/streams/${stream}/state`)).json();
    expect(assisted.report.start.kind).toBe("snapshot");
    expect(assisted.state).toEqual(current.state);

    // 10–11. Record and execute a replay run, and read its evidence.
    await page.goto(`/replay?stream=${stream}`);
    await page.getByRole("button", { name: "Record run" }).click();
    await expect(page.getByRole("table", { name: "Replay runs" })).toContainText("(0, 4]");
    await page.getByRole("button", { name: "Execute replay" }).click();
    await expect(page.getByText("Latest execution evidence")).toBeVisible();
    await expect(page.getByRole("table", { name: "Replay trace" }).getByRole("row")).toHaveCount(5);
    await expect(page.getByRole("table", { name: "Executions" })).toContainText("Exact match");
    const runs = await (await request.get(`${API}/streams/${stream}/replays`)).json();
    expect(runs[0].last_outcome).toBe("exact_match");

    // A different projection is never substituted; the outcome says so.
    await page.getByLabel("Execute with").selectOption("counter");
    await page.getByRole("button", { name: "Execute replay" }).click();
    await expect(page.getByRole("table", { name: "Executions" })).toContainText("Unavailable");

    // 12. Integrity verification with real results.
    await page.goto(`/integrity?stream=${stream}`);
    await expect(page.getByText("Not run yet.")).toBeVisible();
    await page.getByRole("button", { name: "Verify store" }).click();
    await expect(page.getByText("store valid")).toBeVisible();
    await page.getByRole("button", { name: "Save checkpoint of current head" }).click();
    await expect(page.getByRole("table", { name: "Saved checkpoints" })).toContainText("#4");
    await page.getByRole("button", { name: /Verify stream against 1 checkpoint/ }).click();
    await expect(page.getByText("Events checked")).toBeVisible();
    const verified = await (await request.post(`${API}/verify`, { data: {} })).json();
    expect(verified.valid).toBe(true);

    // The overview now reflects the real store.
    await page.goto("/");
    await expect(page.getByRole("table", { name: "Active streams" })).toContainText(stream);
});

test("unknown streams and API errors are shown, not hidden", async ({ page }) => {
    await page.goto("/time-travel?stream=does-not-exist");
    await expect(page.getByRole("alert").filter({ hasText: "stream_not_found" })).toBeVisible();
    await page.goto("/benchmarks");
    await expect(page.getByText("No benchmark results are available.")).toBeVisible();
});
