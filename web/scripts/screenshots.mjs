// Captures README screenshots from a running Observatory (default
// http://localhost:3000) backed by a running orbit-api with data, e.g. after
// scripts/seed-demo.sh. Usage: node scripts/screenshots.mjs [baseUrl]
import { chromium } from "@playwright/test";

const base = process.argv[2] ?? "http://localhost:3000";
const out = new URL("../../docs/screenshots/", import.meta.url).pathname;

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 });

async function shot(path, name, prepare) {
    await page.goto(base + path);
    await page.waitForLoadState("networkidle");
    if (prepare) await prepare();
    await page.waitForTimeout(300);
    await page.screenshot({ path: `${out}${name}.png`, fullPage: false });
    console.log(`captured ${name}`);
}

await shot("/", "overview");
await shot("/time-travel?stream=order-1042&at=2", "time-travel");
const runs = await (await fetch("http://127.0.0.1:7878/api/v1/streams/counter-1/replays")).json();
await shot(`/replay?stream=counter-1&run=${runs[0].run_id}`, "replay-lab");
await shot("/integrity?stream=warehouse-north", "integrity", async () => {
    await page.getByRole("button", { name: "Verify store" }).click();
    await page.getByText("store valid").waitFor();
});
await browser.close();
