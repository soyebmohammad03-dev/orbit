# Contributing to ORBIT

Thanks for your interest. ORBIT is being built in phases (see the
[README](README.md#roadmap)); contributions should fit the current phase.

## Before you open a pull request

1. Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and, for anything
   touching persistence, [docs/STORAGE_DESIGN.md](docs/STORAGE_DESIGN.md).
2. Run the same checks as CI:

   ```bash
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   cargo doc --workspace --no-deps
   ```

3. Add tests that exercise the real SQLite implementation. Do not mock the
   storage layer to make a test pass.
4. Update the documentation and `CHANGELOG.md` when behaviour changes.

## Ground rules

- Schema changes are new migrations appended to `MIGRATIONS` in
  `crates/orbit-core/src/store/schema.rs`. Never edit a released migration.
- Changing the digest construction requires a new domain tag and a
  documented upgrade path. The golden-vector test exists to catch accidental changes.
- New write paths need fault points and `tests/fault_injection.rs` coverage.
  Tests needing crashes or real process concurrency follow the child-process
  pattern in `tests/crash_recovery.rs`. Never synchronise tests with sleeps.
- Prefer typed errors over strings, and avoid panics on any input a caller or
  a damaged database can produce.
- Keep dependencies few and justified. Record significant decisions as an ADR
  in `docs/adr/`.

By contributing you agree that your contributions are licensed under the
[MIT License](LICENSE).
