# Reliability scenarios

The lab can run a small, fixed catalog of **reliability scenarios**. Each one
makes ORBIT fail in a controlled way, then checks what must still hold. The
results are stored as experiments (kind `reliability`) in the lab registry
([LAB.md](LAB.md)) and shown in the Observatory under **Experiments**.

The point is correctness under the failures listed here, not coverage of
every failure. A passing scenario shows that these invariants held for these
failures on the machine that ran it. It does not prove correctness for all
failures, and **nothing here simulates power loss, an operating-system crash
or a storage-device fault.**

## Running them

```bash
cargo run -p orbit-lab --features reliability --bin orbit-reliability -- scenarios
cargo run -p orbit-lab --features reliability --bin orbit-reliability -- run event-append-interruption
cargo run -p orbit-lab --features reliability --bin orbit-reliability -- run registry-concurrency \
    --param processes=8 --param experiments_per_process=25
cargo run -p orbit-lab --features reliability --bin orbit-reliability -- reproduce <experiment id>
```

`--lab DIR` (default `$ORBIT_LAB_DIR` or `data/lab`) selects the lab. Output
is the stored experiment as JSON. Every scenario creates its own files in
`<lab>/reliability-work/<experiment id>/` and removes them afterwards. It
never opens an existing event store; that includes the deliberate
corruption cases, which edit only the scenario's own copies.

The scenarios also run in the test suite (`crates/orbit-lab/tests/reliability.rs`),
on Linux and macOS in CI.

## Catalog

Only these scenarios exist. Parameters are bounded integers, and anything
else is rejected.

| Scenario | Parameters | Cases |
|----------|------------|-------|
| `event-append-interruption` v1 | `events` 1–500 (default 10) | 12 |
| `snapshot-write-interruption` v1 | `events` 1–500 (default 10) | 6 |
| `experiment-interruption` v2 | `events` 6–500 (default 8) | 8 |
| `registry-concurrency` v1 | `processes` 2–8 (default 4), `experiments_per_process` 1–25 (default 5) | 3 |

### `event-append-interruption`

The five append fault points of `orbit-core` (`AppendBeforeBegin`,
`AppendAfterBegin`, `AppendAfterSequenceAllocated`, `AppendAfterInsert`,
`AppendBeforeCommit`) are each exercised twice:

- **simulated error:** in-process, the store's fault hook returns an error
  at the point, once;
- **process abort:** a child process installs a hook that calls
  `std::process::abort()` at the point. There are no destructors and no
  rollback; the operating system tears the process down.

Two more cases complete the scenario:

- **`process_killed`:** a child appends continuously and prints each
  sequence only after `append` has returned, so after `COMMIT`. The parent
  reads five acknowledgements, then kills it with `SIGKILL`.
- **`deliberate_corruption`:** the scenario edits an event's payload in its
  own copy of a store (dropping the append-only trigger to do so).

**Invariants** after reopening the store, which runs SQLite's recovery:
- An interrupted append commits nothing: the head and every earlier
  event's digest are unchanged.
- Sequences are dense, and `verify_store` reports no issue.
- The next append gets the next sequence.
- After a kill, every acknowledged event is present.
- Corruption is reported by `verify_store`, and a differential replay over
  it fails with `integrity_failure`.

### `snapshot-write-interruption`

The three snapshot fault points (`SnapshotBeforeBegin`,
`SnapshotAfterValidation`, `SnapshotBeforeCommit`) are each exercised by
simulated error and by process abort.

**Invariants:**
- No snapshot is stored.
- The store verifies.
- Snapshot-assisted reconstruction equals full replay.
- A later snapshot saves and validates as usable.

### `experiment-interruption`

The lab's own fault points ([`orbit_lab::fault`](../crates/orbit-lab/src/fault.rs)):
- `run_after_start`
- `run_after_execute`
- `artifact_after_partial_write`: half of an artifact's bytes written, not
  flushed or renamed;
- `run_before_complete`: all artifacts written, the result not recorded.

A completed **baseline** experiment exists first. Then:

- **Process abort at each point:** a child process runs a differential
  replay and aborts at the point. Each interrupted run uses a different
  range, so its artifacts have new content and the write really happens:
  identical content would be deduplicated and skipped. The parent then
  opens the lab as a new process would.
- **Simulated error** at `run_before_complete`, in-process.
- **Process paused:** a child stops at `run_after_start` and waits on its
  stdin. While it is alive, another lab instance opens and recovers the
  registry. Then the child is released.
- **Creator killed before start** (added in version 2): a child creates an
  experiment and waits. While it is alive, another lab instance opens and
  recovers the registry. Then the child is killed with `SIGKILL`, and the
  registry is opened again.
- **Deliberate corruption:** a byte of the baseline's `left_trace` file is
  flipped.

**Invariants:**
- An interrupted experiment is `failed`/`interrupted`: never `completed`,
  no result, no artifact links. Its lock file is gone.
- A live experiment is not marked interrupted, and it completes after it
  resumes.
- The baseline stays `completed` with the same result digest; its
  artifacts verify, and it reproduces.
- Recovery removes the temporary files of interrupted writes. Every file
  stored under an artifact name has exactly that content, so a half-written
  file is never accepted.
- A corrupted artifact is reported `damaged`. Reading it fails with
  `ArtifactCorrupted`, and the trace endpoint refuses it. Reproducing its
  experiment reports `artifact_missing`, never `reproduced`.
- A created experiment is not abandoned while its creator lives. Once the
  creator is killed, it is `failed`/`abandoned` exactly once, with no
  result, and its creation lock file is removed.

Artifact files written before an interruption but never linked are counted
(`unlinked_artifact_files`) and left in place. They are complete,
content-addressed files that no record references.

### `registry-concurrency`

Child processes, each with its own SQLite connection, are released together
by a barrier: each prints `ready`, and the parent writes one line to every
stdin once all are ready. Three cases:

- **create and run:** every process creates and runs
  `experiments_per_process` differential replays on the same registry;
- **start race:** every process calls `start` on the same `created`
  experiment;
- **recovery race:** the start winner exited without running it, so every
  process calls `recover_interrupted` at once.

**Invariants:**
- No error, and no experiment lost or duplicated: the ids reported by the
  processes equal the registry's ids.
- All experiments complete, every one's artifacts verify, and identical
  inputs give one result digest.
- One `artifacts` row per distinct content.
- Exactly one process starts the contested experiment; the others get
  `InvalidTransition`.
- Exactly one process recovers it, and it ends `failed`/`interrupted`.

## Fault-injection boundaries

| Hooks | Where | Enabled by |
|-------|-------|------------|
| `orbit-core` `FaultPoint` hooks (`EventStore::set_fault_hook`) | event-store write paths | the `fault-injection` feature of `orbit-core` |
| `orbit-lab` `LabFaultPoint` hooks (`Lab::set_fault_hook`) | experiment runs and artifact writes | the `reliability` feature of `orbit-lab`, which also turns on `fault-injection` |

- **Off by default.** Without the features the fault points compile to
  nothing, and no hook type or setter exists.
- **Installed only by code.** Hooks are set only by code that calls the
  setter. Nothing reads environment variables. Child processes get their
  role as a command-line argument of the explicitly built `orbit-reliability`
  tool; a malformed or unknown role exits with an error.
- **The API cannot run scenarios.** `orbit-api` never enables either feature.
  It can list the catalog (`GET /api/v1/lab/reliability/scenarios`) and show
  results like any experiment. `POST /api/v1/lab/experiments` with kind
  `reliability` is rejected with `400 unsupported_experiment_kind`, and no
  route runs, kills or corrupts anything. Tests cover both.
- **CI enforces the separation:**
  - `cargo tree -p orbit-api -e features` must not contain `fault-injection`
    or the `reliability` feature;
  - the `orbit-api` binary, built on its own, must not contain strings that
    exist only in the reliability child code. Locally, those strings were
    found in `orbit-reliability` (the control) and not in `orbit-api`.
- **Tests turn the features on** through self dev-dependencies, as before.
  A `cargo test --workspace` build is therefore a test build, not a
  production build.

## Simulated versus real failures

Every case records its `mode` and `real_process`:

| Mode | Real OS process? | What happens |
|------|------------------|--------------|
| `simulated_error` | no | A hook returns an error inside the running process; the code takes its normal error path (rollback). |
| `process_abort` | yes | A child process aborts at the point. Its open transaction is never committed, and the OS releases its file locks. |
| `process_killed` | yes | `SIGKILL` from the parent at an unpredictable moment. |
| `process_paused` | yes | A live child is blocked at a point while another process acts. |
| `deliberate_corruption` | no | Files the scenario created are edited on purpose. |
| `concurrent_processes` | yes | Several children contend for the same registry. |

Process aborts and kills leave the operating system running. Data already
handed to the OS survives, so these cases test process crashes, **not power
loss or OS crashes**. Durability under power loss still rests on SQLite and
the storage device, and is untested here.

## Results

A reliability result (`result_version` 1) records:

- the scenario id, version and title;
- the parameters;
- a history fingerprint: the number of events and a digest of the requests
  (event types and payloads). This is identical on every run, while event
  ids and timestamps, and so event digests, differ per run;
- per case:
  - the case `id`, `fault_point` and `mode`;
  - `fault_observed` with its `fault_evidence`;
  - the `invariants`, each with `expected`, `observed` and `held`;
  - `recovery` and `integrity` details;
  - a `verdict`;
- a `summary`, an overall `verdict`, and the `simulates`,
  `does_not_simulate` and `limitations` lists.

Children's output, with the work directory replaced by `<work>`, is stored
as the `child_output` artifact.

**Case verdicts:**

| Verdict | Meaning |
|---------|---------|
| `inconclusive` | The planned fault was **not** observed, so the checks, written for the faulted path, prove nothing. For example, a child exited normally instead of aborting. |
| `invariant_violated` | The fault happened and at least one check failed. |
| `invariant_preserved` | The fault happened and every check held. |

The overall verdict, stored as the experiment's outcome, is
`invariant_violated` if any case is, else `inconclusive` if any case is, else
`invariants_preserved`.

**Infrastructure failures are not results.** If a scenario cannot set itself
up or observe a case (a child cannot start, a child hangs past its 120 s
watchdog, the work directory cannot be written), the experiment is
**`failed`** with kind `infrastructure`, and no result is stored.

The registry stores `reproducibility: "unverified"` until the experiment is
reproduced. `orbit-reliability reproduce <id>` runs the scenario again in a
new work directory and compares, per case, the verdict, whether the fault
was observed and which invariants held. It never modifies the original
record; the attempt is appended as a verification: `reproduced`, `differs`,
`configuration_mismatch`, or `not_reproducible` when the rerun could not be
carried out.

### Example report

These are actual runs on 2026-10-18, on an Apple M3 with macOS 27.0.1, of the
working tree based on commit `1f18f61`, with default parameters:

| Scenario | Outcome | Cases | Preserved | Real-process cases | Elapsed |
|----------|---------|-------|-----------|--------------------|---------|
| `event-append-interruption` | `invariants_preserved` | 12 | 12 | 6 | 0.87 s |
| `snapshot-write-interruption` | `invariants_preserved` | 6 | 6 | 3 | 0.39 s |
| `experiment-interruption` (v1) | `invariants_preserved` | 7 | 7 | 5 | 0.17 s |
| `registry-concurrency` | `invariants_preserved` | 3 | 3 | 3 | 0.22 s |

After the Phase 9 change (version 2), `experiment-interruption` ran again
on the same machine, from the working tree at commit `3339bfe`:
`invariants_preserved`, 8 cases, 8 preserved, 6 real-process cases, 0.24 s.
Its new case, abridged:

```json
{
  "id": "experiment_created/process_killed",
  "mode": "process_killed",
  "fault_observed": true,
  "fault_evidence": "the child created 01a14fab-ea90-7571-b02e-50e12fbdf4ce, another process opened the lab, then the child was terminated by SIGKILL (9)",
  "invariants": [
    { "name": "live_creator_not_abandoned", "observed": "created", "held": true },
    { "name": "abandoned_after_creator_died", "observed": "failed Some(Abandoned) links: 0", "held": true },
    { "name": "abandoned_exactly_once", "observed": "0 recovered", "held": true },
    { "name": "creation_lock_removed", "observed": "absent", "held": true },
    { "name": "earlier_result_preserved", "held": true }
  ],
  "verdict": "invariant_preserved"
}
```

Reproducing a version 1 `experiment-interruption` result now reports
`configuration_mismatch`: its scenario version is no longer the catalog's.

One case from the first run, verbatim:

```json
{
  "id": "append_after_insert/process_abort",
  "fault_point": "AppendAfterInsert",
  "mode": "process_abort",
  "real_process": true,
  "fault_observed": true,
  "fault_evidence": "child terminated by SIGABRT (6); it did not report a completed append",
  "invariants": [
    { "name": "store_reopens", "expected": "the store opens", "observed": "opened", "held": true },
    { "name": "nothing_committed", "expected": "head stays at 10", "observed": "head is 10", "held": true },
    { "name": "committed_events_unchanged", "expected": "every earlier event has its earlier digest", "observed": "unchanged", "held": true },
    { "name": "dense_sequences", "expected": "sequences 1..=head", "observed": "dense", "held": true },
    { "name": "store_verifies", "expected": "verify_store reports no issue", "observed": "valid", "held": true },
    { "name": "next_append_continues", "expected": "the next append gets sequence 11", "observed": "appended sequence 11", "held": true }
  ],
  "integrity": { "issues": 0, "snapshots_checked": 0, "sqlite_integrity": ["ok"], "streams": 1, "valid": true },
  "verdict": "invariant_preserved"
}
```

In the same run, the kill case reported: "child acknowledged sequence 15,
then was terminated by SIGKILL (9)". Afterwards the head was 15. Reproducing
the snapshot scenario returned `reproduced`, with all 6 cases matching.

`registry-concurrency` was also run three times with 8 processes × 25
experiments. Each run observed "200 reported, 200 distinct, 200 in the
registry"; all completed and verified; there was 1 result digest and 4
artifact rows. The start race was "1 won, 7 lost", the recovery race "1
recoveries, 0 errors".

During development the scenarios found a real defect in themselves.
`artifact_after_partial_write` never wrote anything, because its artifacts
had the same content as the baseline's and were deduplicated before the
fault point. The case was reported, wrongly at that point, as a violation.
Two changes followed:
- interrupted runs now use distinct ranges, so a write really happens;
- a case whose fault is not observed is now `inconclusive`, whatever its
  checks say.

## Registry concurrency guarantees

- **Atomic writes.** Each write is a single statement or one
  `BEGIN IMMEDIATE` transaction: creation, each transition, and completion
  together with its artifact links. A reader never sees half a completion.
- **Lock contention.** Waits are bounded by SQLite's busy timeout (5 s).
  The tests saw no lock errors at 8 processes; heavier contention could
  surface `database is locked` as a registry error.
- **Transitions.** Conditional `UPDATE`s plus triggers make a transition
  happen at most once: exactly one `start` wins.
- **Liveness.** A running experiment's owner holds an OS file lock
  (`flock` on Unix) on `locks/<id>.lock`. The OS releases it when the
  process exits, however it exits. Recovery marks only experiments whose lock
  it can take. A long-running live experiment is never marked interrupted.
  Several concurrent recoveries mark it at most once; the others see a
  conflicting transition and ignore it. There are no leases, heartbeats or
  clocks.
- **Temporary artifact files** are locked by their writer until they are
  renamed into place. Recovery deletes only those it can lock, so it never
  deletes a live writer's file. If a cleaner deletes a fresh file in the
  instant before its writer locks it, the writer's rename fails and the
  write is retried (up to three attempts).
- **Not covered:**
  - Locks are advisory and per host, so the registry must stay on a local
    file system.
  - An experiment whose creator died before starting it was left `created`
    until Phase 9. Now every experiment is created while its creator holds
    an OS lock on `locks/<id>.create`; recovery marks it
    `failed`/`abandoned` once that lock is free, and only while it is still
    `created`. Experiments released for another process to run, and ones
    created by older versions, have no creation lock file and are left
    alone. See [LAB.md](LAB.md#abandoned-experiments).
  - Windows is untested; the CI runners are Linux and macOS.

## Untested failure classes

- Power loss, OS crashes, kernel panics and unflushed device caches.
- Full disks and other I/O errors from the file system.
- Corruption of SQLite's own pages, as opposed to row content.
- Network file systems, and clock changes. Nothing here depends on clocks.
- Interruptions between arbitrary instructions. Only the named fault points,
  and the `SIGKILL` timing, are exercised.
- Hangs. Children are killed by a watchdog, and that is reported as an
  infrastructure failure.
