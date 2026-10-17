# ORBIT Observatory (web)

The web interface of ORBIT: a Next.js (App Router), strict TypeScript and
Tailwind CSS application that talks to the `orbit-api` server. It has no
backend of its own and no fixture data. Every page renders what the API
returns.

```bash
npm ci
cp .env.example .env.local    # optional; defaults to http://127.0.0.1:7878/api/v1
npm run dev                   # http://localhost:3000 (start orbit-api first)
```

| Script                            | What it does                                                                                                                                   |
| --------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `npm run dev` / `build` / `start` | Next.js development server, production build, production server                                                                                |
| `npm run lint`                    | ESLint (Next.js core-web-vitals and TypeScript rules)                                                                                          |
| `npm run typecheck`               | `tsc --noEmit` with `strict`                                                                                                                   |
| `npm run format:check`            | Prettier                                                                                                                                       |
| `npm test`                        | Vitest and Testing Library unit and component tests (`tests/unit`)                                                                             |
| `npm run test:e2e`                | Playwright end-to-end workflow against a real `orbit-api` and SQLite database (`tests/e2e`). Run `npx playwright install chromium` once first. |

Layout:

- `src/lib/api.ts`: the typed API client (`createClient`). It turns structured
  error envelopes into `ApiError` and reads the server-measured
  `X-Orbit-Elapsed-Micros`.
- `src/lib/types.ts`: response types, mirroring `crates/orbit-api/src/dto.rs`
  and the engine's serialized reports.
- `src/lib/use-api.ts`: `useApi` (load with loading, error and retry states) and
  `useAction` (mutations).
- `src/components`: the shell, navigation, health indicator, pickers, and the
  diff and JSON views.
- `src/app/*`: one route per page. Each `page.tsx` renders a client `view.tsx`
  inside `<Suspense>`.

See the repository's [docs/OBSERVATORY.md](../docs/OBSERVATORY.md) for the
architecture and walkthrough.
