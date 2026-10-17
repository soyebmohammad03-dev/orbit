# ADR 0006: Feature-gated fault hooks and child-process crash tests

- Status: accepted
- Date: 2026-10-18

## Context

Phase 3 requires deterministic failure testing at transaction boundaries,
without production-only global flags and without timing-based tests.

## Decision

- Write paths call `self.fault(FaultPoint::X)?` at named boundaries. With the
  `fault-injection` cargo feature, this consults a per-`EventStore` hook. Without
  the feature (the default), it is an inlined `Ok(())`. The test suite enables
  the feature with a self dev-dependency
  (`orbit-core = { path = ".", features = ["fault-injection"] }`).
- Process crashes are produced in child processes, by re-invoking the test
  binary with an environment variable. The child either `abort()`s from inside
  the hook or is killed with `SIGKILL`. The parent then reopens and verifies.

## Consequences

- No hook exists in production builds. Because the hook is per instance,
  parallel tests cannot interfere.
- An injected error follows the exact code path of a real error at that point:
  `?` drops the transaction, which rolls back.
- Crash tests cover process death, including with a live WAL. They do not
  cover OS crashes or power loss, and the documentation says so.
