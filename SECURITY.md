# Security Policy

## Supported versions

ORBIT is pre-release software (0.x). Only the latest commit on `main` gets
fixes.

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Use GitHub's
private vulnerability reporting ("Report a vulnerability" under the
repository's **Security** tab) once the repository is published, or contact the
maintainer privately. Include a description, reproduction steps, and the
affected commit.

## Security model and known limitations

- ORBIT is an embedded library. It has no network surface in Phase 1.
- Event digests are **unkeyed SHA-256**. They detect accidental corruption and
  naive modification of stored events. They do **not** protect against
  anyone who can write the database file, because such a party can recompute
  every digest consistently. See [docs/STORAGE_DESIGN.md](docs/STORAGE_DESIGN.md#integrity-verification).
- The database file is not encrypted. Protect it with file-system permissions.
- All SQL uses bound parameters. No caller-supplied value is interpolated
  into SQL text.
