# Security Policy

## Supported versions

The current release is `v0.2.0` (0.x: no API stability guarantee). Only
the latest commit on `main` gets fixes.

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Use GitHub's
private vulnerability reporting ("Report a vulnerability" under the
[repository's](https://github.com/soyebmohammad03-dev/orbit) **Security** tab),
or contact the maintainer privately. Include a description, reproduction steps, and the
affected commit.

## Security model and known limitations

- The `orbit-core` library has no network surface.
- **`orbit-api` is for trusted local use only.** It has **no authentication or
  authorization**, so anyone who can reach it can read every event and append
  to every stream. Do not deploy it on the internet or on a shared network.
  Putting it behind a proxy does not change this.

  | Threat | Mitigation in `orbit-api` | Test |
  |--------|---------------------------|------|
  | Access from other machines | Binds to `127.0.0.1` by default. Non-loopback addresses are refused unless `ORBIT_API_ALLOW_REMOTE=1`, and logged as a warning when allowed. | `config.rs` unit tests |
  | DNS rebinding (a malicious web page resolving its domain to 127.0.0.1) | `Host` allowlist: only `localhost`, `127.0.0.1`, `[::1]` and explicitly configured names | `tests/hardening.rs` |
  | Cross-site requests from other origins | CORS for configured origins only, GET/POST only. JSON-only `POST` bodies force a preflight, so cross-site form posts are rejected before any write. | `tests/hardening.rs`, `tests/api.rs` |
  | Oversized or unbounded requests | Body limit, page-size and interval caps, replay cap, per-request deadline, SQLite busy timeout | `tests/api.rs`, `tests/hardening.rs` |
  | Information disclosure | No SQL or file paths from clients. Errors contain no local paths. Benchmark files are read only from the configured directory, never a client-supplied path, and symlinks are not followed. | `tests/hardening.rs`, `benchmarks.rs` tests |
  | Fault injection reachable from the API | Fault hooks exist only with `orbit-core/fault-injection` and `orbit-lab/reliability`, which `orbit-api` never enables; scenarios execute only in the separately built `orbit-reliability` tool; the API rejects `kind: reliability` and has no route that injects faults; nothing reads environment variables to enable a hook | `orbit-api` `tests/lab.rs`; CI checks the API's feature graph and searches its binary for child-process code |
  | Launching the reliability runner from a browser | Disabled unless an operator sets `ORBIT_RELIABILITY_RUNNER` to an absolute path; the executable path is configuration, never request data; requests are catalog specs only (unknown fields, scenarios and out-of-range parameters refused, nothing recorded); the runner gets fixed arguments (`--lab <dir> run-id <id>`), no shell; one run at a time; a deadline kills the runner's process group; a client cannot name, signal or kill any process or file | `orbit-lab` `tests/launcher.rs`, `orbit-api` `tests/reliability_runs.rs`, `web/tests/e2e/reliability.spec.ts` |
  | Lab requests naming files | Experiment specs reject unknown fields; artifact paths are derived only from SHA-256 digests; the lab never writes to the event store, and its registry is a separate database | `orbit-lab` `tests/registry.rs`, `orbit-api` `tests/lab.rs` |
  | Malicious evidence bundles (oversized, deeply nested, truncated, path traversal, unexpected fields, forged digests) | Raw-byte upload with its own 32 MiB limit; nesting depth checked before parsing; exact fields and canonical form; strict entry-name pattern; entry count and size limits; strict base64; every digest and the event chain recomputed before anything is trusted; entries stored only by content digest, never by name; nothing in a bundle is ever executed; invalid bundles are never imported; imports never touch local experiments | `orbit-lab` `src/bundle.rs` unit tests, `tests/bundles.rs` (fixtures, single-byte property test), `orbit-api` `tests/evidence.rs` |
  | Substituted or forged bundles | Integrity alone cannot prevent this: a consistent forgery validates under a **new** bundle digest. Optional Ed25519 signatures (`ed25519-dalek`, strict verification) over the bundle digest; invalid or malformed signatures make a bundle invalid; a signature counts only for a key id **and** public key in an explicitly configured trust store (`--trusted-keys`, `ORBIT_TRUSTED_KEYS`), otherwise `signed_untrusted`; keys are never trusted automatically; key files are written owner-only and never overwritten, and the secret is never printed or embedded | `tests/bundles.rs` (`a_consistent_forgery_…`, `signatures::…`), `src/signing.rs` tests, `tests/cli.rs`, `orbit-api` `tests/evidence.rs` |
  | Resource exhaustion through measurements | Every performance spec field is bounded and unknown fields are rejected; one background measurement per server (409 otherwise); work directories are inside the lab directory and removed afterwards | `orbit-lab` `tests/performance.rs`, `orbit-api` `tests/evidence.rs` |

  These mitigations reduce the risk on a developer machine. They are not a
  substitute for authentication.
- Event digests are **unkeyed SHA-256**. They detect accidental corruption and
  naive modification of stored events. They do **not** protect against
  anyone who can write the database file, because such a party can recompute
  every digest consistently. Checkpoints kept outside the database detect such
  a rewrite up to the checkpoint. The full threat model is in
  [docs/RECOVERY.md](docs/RECOVERY.md#integrity-threat-model).
- Dependency advisories, checked for v0.2.0 on 2026-10-18 and on every CI
  run: `cargo audit` (RustSec advisory database, 1,338 advisories, 247
  crates in `Cargo.lock`) reports **0 vulnerabilities**; `npm audit
  --omit=dev` reports **0** for the web's production dependencies. `npm
  audit` reports one **development-only** chain:
  `eslint-config-next → @next/eslint-plugin-next → fast-glob → micromatch → braces`,
  GHSA-vfj7-8cjw-p6xm. It is a denial-of-service issue triggered by
  pathological glob patterns given to the linter. Every `braces` version is
  affected, so no patched release exists, and npm's suggested "fix" downgrades
  to `eslint-config-next` 14, which is incompatible with Next.js 16. It runs
  only when linting this repository's own files, and is accepted until
  upstream ships a fix.
- The `fault-injection` feature is for testing only. It lets code installed
  by the embedding application make writes fail or abort. Do not enable it in
  production builds.
- The database file is not encrypted. Protect it with file-system permissions.
- All SQL uses bound parameters. No caller-supplied value is interpolated
  into SQL text.
