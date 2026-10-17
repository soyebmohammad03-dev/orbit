import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { defineConfig, devices } from "@playwright/test";

// One fresh database directory per test run, shared with worker processes
// through the environment. Removed by global-teardown.ts.
process.env.ORBIT_E2E_DIR ??= mkdtempSync(join(tmpdir(), "orbit-e2e-"));
const dataDir = process.env.ORBIT_E2E_DIR;

const API_PORT = 7879;
const WEB_PORT = 3100;
const API_URL = `http://127.0.0.1:${API_PORT}/api/v1`;
const WEB_URL = `http://127.0.0.1:${WEB_PORT}`;

export default defineConfig({
    testDir: "tests/e2e",
    timeout: 60_000,
    fullyParallel: false,
    workers: 1,
    retries: 0,
    reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
    globalTeardown: "./tests/e2e/global-teardown.ts",
    use: {
        baseURL: WEB_URL,
        trace: "retain-on-failure",
    },
    projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
    webServer: [
        {
            // The real API over a real, empty SQLite file.
            command: "cargo run --quiet -p orbit-api",
            cwd: "..",
            url: `${API_URL}/health`,
            timeout: 300_000,
            reuseExistingServer: false,
            env: {
                ORBIT_DB_PATH: join(dataDir, "orbit.db"),
                ORBIT_API_ADDR: `127.0.0.1:${API_PORT}`,
                ORBIT_API_CORS_ORIGINS: WEB_URL,
                ORBIT_BENCHMARK_DIR: "",
                ORBIT_DURABILITY: "normal",
            },
        },
        {
            // A production build of the Observatory pointed at that API.
            command: `npm run build && npx next start --port ${WEB_PORT} --hostname 127.0.0.1`,
            url: WEB_URL,
            timeout: 300_000,
            reuseExistingServer: false,
            env: { NEXT_PUBLIC_ORBIT_API_URL: API_URL, NEXT_TELEMETRY_DISABLED: "1" },
        },
    ],
});

export { API_URL };
