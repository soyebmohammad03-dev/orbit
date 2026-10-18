# ADR 0009: A separate `orbit-lab` crate with its own registry database

- Status: accepted
- Date: 2026-10-18

## Context

Sprint 7 adds an experiment registry, implementation identities and
differential replay ([LAB.md](../LAB.md); plan in
[ATLAS_INTEGRATION.md](../ATLAS_INTEGRATION.md)). The engine, `orbit-core`,
is the system under test, and its correctness guarantees and schema (v3)
must not change. `orbit-core`'s `ProjectionKey` cannot tell apart two
implementations that declare the same name and version, and `ReplayLab`
deliberately refuses to compare different keys.

## Decision

1. **A new crate, `crates/orbit-lab`** (a library plus an `orbit-lab` binary).
   It depends on `orbit-core` through its public API only, and `orbit-api`
   depends on it. `orbit-core` gains nothing and changes nothing.
2. **Its own SQLite database, `lab.db`**, with application id `ORLB`, its own
   migrations, triggers that enforce the lifecycle
   (`created → running → completed | failed`) and immutability, and a
   `CHECK` constraint tying each status to its columns. The lab only ever
   *reads* event stores.
3. **Content-addressed artifact files** under `artifacts/sha256/`, with paths
   derived only from digests, verified on every read.
4. **`ImplementationDescriptor`** with a unique `id` and a fingerprint over
   the identity fields (id, name, declared key, adapter, Rust type name,
   configuration digest, determinism declaration). The build commit is
   recorded beside it, not inside it.
5. **Differential replay folds both implementations in lockstep** with
   `Projection::apply` over pages from `read_after`. It reuses
   `verify_stream`, `state_digest`, `to_canonical_string` and `diff::diff`.
   It does not use `Reconstructor` or `ReplayLab` internals.
6. **Interrupted runs are detected with OS file locks**
   (`std::fs::File::try_lock`), not with timeouts or heartbeats.

## Consequences

- The core's dependency set, schema and API stay as they were. The lab can
  evolve, adding experiment kinds and evidence bundles, without migrations
  of the event store.
- An experiment's result can be inspected and re-executed later, but
  reproduction needs the original event store. History is not copied into
  the registry.
- The fingerprint distinguishes registrations, not code bodies. Behavioural
  differences are established only by executing (documented in LAB.md).
- One more crate to build and test. CI already runs `cargo test --workspace`,
  so it is covered without workflow changes.
- `orbit-api` does not enable `orbit-core/fault-injection` through the lab:
  the lab uses default features.
