# Security Policy

## Supported versions

ORBIT is pre-release software. The current release is `v0.1.0`. Only the
latest commit on `main` gets fixes.

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

  | Lab requests naming files | Experiment specs reject unknown fields; artifact paths are derived only from SHA-256 digests; the lab never writes to the event store, and its registry is a separate database | `orbit-lab` `tests/registry.rs`, `orbit-api` `tests/lab.rs` |

  These mitigations reduce the risk on a developer machine. They are not a
  substitute for authentication.
- Event digests are **unkeyed SHA-256**. They detect accidental corruption and
  naive modification of stored events. They do **not** protect against
  anyone who can write the database file, because such a party can recompute
  every digest consistently. Checkpoints kept outside the database detect such
  a rewrite up to the checkpoint. The full threat model is in
  [docs/RECOVERY.md](docs/RECOVERY.md#integrity-threat-model).
- Dependency advisories: the production dependency trees have no known
  vulnerabilities (`npm audit --omit=dev` reports 0). `npm audit` reports one
  **development-only** chain:
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
