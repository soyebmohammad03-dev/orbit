// Captures the README screenshots from a running Observatory and API, and
// checks every captured page for console errors, failed requests,
// horizontal overflow and unnamed controls. Nothing is mocked: start the
// API with data first, for example with
//   scripts/validate-workflow.sh /tmp/orbit-demo --keep
// and the Observatory with `npm run build && npm run start`.
//
// Usage: node scripts/screenshots.mjs [webBase] [apiBase]
import { chromium } from "@playwright/test";

const web = process.argv[2] ?? "http://localhost:3000";
const api = process.argv[3] ?? "http://127.0.0.1:7878/api/v1";
const out = new URL("../../docs/assets/screenshots/", import.meta.url).pathname;
const get = async (path) => (await fetch(api + path)).json();

const experiments = (await get("/lab/experiments?limit=100")).experiments;
const diverged = experiments.find((e) => e.outcome === "diverged");
const reliability = experiments.find((e) => e.kind === "reliability" && e.status === "completed");
const bundle = (await get("/lab/bundles")).bundles[0];
if (!diverged || !reliability || !bundle) throw new Error("run scripts/validate-workflow.sh --keep first");

const browser = await chromium.launch();
const problems = [];

async function capture(name, path, { width = 1440, height = 900, prepare, full = false } = {}) {
    const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: 1 });
    page.on("console", (m) => m.type() === "error" && problems.push(`${name}: console: ${m.text()}`));
    page.on("requestfailed", (r) => problems.push(`${name}: request failed: ${r.url()}`));
    page.on("response", (r) => r.status() >= 500 && problems.push(`${name}: HTTP ${r.status()} ${r.url()}`));
    await page.goto(web + path);
    await page.waitForLoadState("networkidle");
    if (prepare) await prepare(page);
    await page.waitForLoadState("networkidle");
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    if (overflow > 0) problems.push(`${name}: ${overflow}px horizontal overflow`);
    const unnamed = await page.evaluate(
        () =>
            [...document.querySelectorAll("button, a, input, select, textarea")].filter((el) => {
                const label =
                    el.getAttribute("aria-label") ||
                    el.textContent?.trim() ||
                    (el.id && document.querySelector(`label[for="${el.id}"]`)?.textContent);
                return !label && el.getAttribute("type") !== "hidden";
            }).length,
    );
    if (unnamed > 0) problems.push(`${name}: ${unnamed} controls without an accessible name`);
    await page.screenshot({ path: `${out}${name}.png`, fullPage: full });
    console.log(`captured ${name} (${width}x${height})`);
    await page.close();
}

await capture("overview", "/");
await capture("time-travel", "/time-travel?stream=order-1042&at=3");
await capture("integrity", "/integrity?stream=order-1042", {
    prepare: async (p) => {
        await p.getByRole("button", { name: "Verify store" }).click();
        await p.getByText("store valid").waitFor();
    },
});
await capture("differential-replay", `/experiments?experiment=${diverged.id}`, {
    height: 1400,
    prepare: (p) =>
        p
            .getByText(/First divergence/)
            .first()
            .waitFor(),
});
await capture("reliability", `/experiments?experiment=${reliability.id}`, {
    height: 1200,
    prepare: (p) => p.getByRole("list", { name: "Reliability cases" }).waitFor(),
});
await capture("performance", "/performance", {
    height: 1100,
    prepare: async (p) => {
        const table = p.getByRole("table", { name: "Performance experiments" });
        await table.getByRole("checkbox").first().check();
        await p.getByRole("button", { name: /^Compare 1/ }).click();
        await p.getByRole("list", { name: "Comparisons" }).waitFor();
    },
});
await capture("evidence", `/evidence?bundle=${bundle.bundle_digest}`, {
    height: 1100,
    prepare: (p) => p.getByRole("list", { name: "Bundle entries" }).waitFor(),
});
await capture("mobile-experiments", `/experiments?experiment=${diverged.id}`, {
    width: 390,
    height: 844,
    prepare: (p) =>
        p
            .getByText(/First divergence/)
            .first()
            .waitFor(),
});
await browser.close();

if (problems.length > 0) {
    console.error(problems.join("\n"));
    process.exit(1);
}
console.log("no console errors, failed requests, overflow or unnamed controls");
