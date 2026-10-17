# Security Policy

## Supported versions

ORBIT is pre-release software (0.x). Only the latest commit on `main` gets
fixes.

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Use GitHub's
private vulnerability reporting ("Report a vulnerability" under the
[repository's](https://github.com/soyebmohammad03-dev/orbit) **Security** tab),
or contact the maintainer privately. Include a description, reproduction steps, and the
affected commit.

## Security model and known limitations

- The `orbit-core` library has no network surface. The `orbit-api` server
  (Phase 4) is a **local development tool without authentication**. It binds to
  `127.0.0.1` by default and refuses non-loopback addresses unless
  `ORBIT_API_ALLOW_REMOTE=1` is set. Do not expose it to untrusted networks.
  CORS allows only the configured local origins. The API accepts no SQL and no
  file paths from clients, and never returns the database path.
- Event digests are **unkeyed SHA-256**. They detect accidental corruption and
  naive modification of stored events. They do **not** protect against
  anyone who can write the database file, because such a party can recompute
  every digest consistently. Checkpoints kept outside the database detect such
  a rewrite up to the checkpoint. The full threat model is in
  [docs/RECOVERY.md](docs/RECOVERY.md#integrity-threat-model).
- The `fault-injection` feature is for testing only. It lets code installed
  by the embedding application make writes fail or abort. Do not enable it in
  production builds.
- The database file is not encrypted. Protect it with file-system permissions.
- All SQL uses bound parameters. No caller-supplied value is interpolated
  into SQL text.
