import { rmSync } from "node:fs";

export default function globalTeardown() {
    const dir = process.env.ORBIT_E2E_DIR;
    if (dir && dir.includes("orbit-e2e-")) rmSync(dir, { recursive: true, force: true });
}
