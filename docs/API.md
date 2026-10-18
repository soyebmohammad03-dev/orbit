# ORBIT Observatory API (`/api/v1`)

`orbit-api` is an Axum HTTP server over the `orbit-core` engine. It is meant
for **local use**: by default it binds to `127.0.0.1:7878`, has no
authentication, and refuses non-loopback addresses unless
`ORBIT_API_ALLOW_REMOTE=1` is set.

The machine-readable contract is the OpenAPI 3.1 document at
`GET /api/v1/openapi.json`. It is generated from the handler annotations, so it
always matches the running server.

## Running

```bash
cargo run -p orbit-api            # from the repository root
curl http://127.0.0.1:7878/api/v1/health
```

| Variable | Default | Meaning |
|----------|---------|---------|
| `ORBIT_API_ADDR` | `127.0.0.1:7878` | Listen address. Non-loopback needs `ORBIT_API_ALLOW_REMOTE=1`. |
| `ORBIT_API_ALLOW_REMOTE` | unset | Opt-in for non-loopback binding. There is still no authentication. |
| `ORBIT_DB_PATH` | `data/orbit.db` | SQLite file. Created, with its directory, on first start. Phase 1–3 databases are migrated. |
| `ORBIT_API_CORS_ORIGINS` | `http://localhost:3000,http://127.0.0.1:3000` | Browser origins allowed by CORS (exact match, GET/POST only) |
| `ORBIT_BENCHMARK_DIR` | `target/criterion` if present | Criterion results served read-only by `/benchmarks`. An empty value disables it. |
| `ORBIT_API_ALLOWED_HOSTS` | none | Extra host names accepted in the `Host` header, comma-separated. `localhost`, `127.0.0.1` and `[::1]` are always accepted. |
| `ORBIT_API_REQUEST_TIMEOUT_SECS` | `30` | Per-request deadline (1–600 s). Requests over it get `503 request_timeout`. |
| `ORBIT_LAB_DIR` | `data/lab` | Lab directory: the experiment registry `lab.db` and artifacts. Created on first start. Separate from the event store. An empty value disables the lab endpoints (`503 lab_unavailable`). |
| `ORBIT_RELIABILITY_RUNNER` | unset | Absolute path to a separately built `orbit-reliability` executable. Set, it enables `POST /lab/reliability/runs`; unset, launching is disabled (`503 reliability_runner_disabled`). The API binary itself never contains fault-injection code. |
| `ORBIT_RELIABILITY_TIMEOUT_SECS` | `900` | Deadline of one launched reliability run (1–3600 s). The runner and its child processes are then killed and the run is recorded as an infrastructure failure. |
| `ORBIT_TRUSTED_KEYS` | unset | A trusted-keys file ([EVIDENCE.md](EVIDENCE.md#signatures-and-trust)). Unset, no bundle signature is trusted: signed bundles are reported `signed_untrusted`. |
| `ORBIT_DURABILITY`, `ORBIT_BUSY_TIMEOUT_MS`, `ORBIT_MAX_PAYLOAD_BYTES`, `ORBIT_MAX_METADATA_BYTES` | see [DEVELOPMENT.md](DEVELOPMENT.md#configuration) | Store settings |
| `RUST_LOG` | `info,tower_http=info` | Log filter |

## Design

- **Engine first.** Handlers validate input, call `orbit-core`, and map the
  result. Reconstruction, diffs, snapshots, verification and replay are the
  engine's own operations. The API reimplements none of them.
- **No blocking on the async runtime.** The engine is synchronous (SQLite).
  Every engine call runs in `tokio::task::spawn_blocking`.
- **Projections by name.** Reconstruction-type endpoints take
  `projection=<name>` or `<name>@vN`. The built-in projections are:

  | Projection | Behaviour |
  |------------|-----------|
  | `merge-patch@v1` (default) | Applies each event payload as an RFC 7386 JSON Merge Patch, so it works on any stream. |
  | `counter@v1` | The engine's counter example |
  | `inventory@v1` | The engine's inventory example |

  `GET /capabilities` lists them. An unknown name returns
  `400 unknown_projection`.
- **Resources versus reports.** Streams, events, snapshots and pages have
  dedicated response types. Engine reports are returned exactly as the engine
  serializes them, and are documented in [RECONSTRUCTION.md](RECONSTRUCTION.md),
  [SNAPSHOTS.md](SNAPSHOTS.md), [RECOVERY.md](RECOVERY.md) and
  [REPLAY.md](REPLAY.md). This covers `ReconstructionReport`, `StateDiff`,
  `Explanation`, `IntegrityReport`, `StoreIntegrityReport`, `ReplayRun`,
  `ReplayExecution` and `ExperimentReport`.
- **Timing.** Every response carries `X-Orbit-Elapsed-Micros` and
  `Server-Timing: app;dur=<ms>`, both measured by the server for that request.
  Engine reports also include their own measured `elapsed_micros`. Benchmark
  results (`/benchmarks`) are Criterion measurements read from disk, a
  separate thing.
- **Safety.** There is no SQL from clients and no client-supplied file paths,
  and responses never contain the database path. Request bodies are limited to
  the payload limit plus the metadata limit plus 64 KiB (1.125 MiB by default).
  Only the two bundle upload routes accept more: up to 32 MiB, the bundle size
  limit (see [Lab endpoints](#lab-endpoints)).
  Page sizes are capped at 500, and `/explain` at 1,000 events.
- **Host allowlist (DNS-rebinding defence).** A request is served only if its
  `Host` names an allowed host: `localhost`, `127.0.0.1`, `[::1]`, plus any
  `ORBIT_API_ALLOWED_HOSTS`. Requests without a `Host` are refused too. CORS
  alone does not protect a localhost API: a malicious page can rebind its own
  domain to `127.0.0.1`, and the browser then treats it as same-origin. That
  page's requests still carry the attacker's host name, and are rejected with
  `403 host_not_allowed`.
- **No simple cross-site writes.** `POST` bodies must be `application/json`.
  That forces a CORS preflight, which only the configured origins pass. A
  cross-site form post (`text/plain`) is rejected with `415` before anything is
  written.
- **Deadlines.** Each request has a deadline (30 s by default). When it is
  exceeded, the client gets `503 request_timeout` with class `retryable`.
  Synchronous SQLite work cannot be cancelled midway, so **a timed-out write
  may still complete**. Retry writes idempotently, with `expected_sequence` or
  a caller-chosen `event_id`. Lock waits are bounded separately by the store's
  `busy_timeout`.

## Endpoints

| Method and path | Purpose | Success | Notable errors |
|-----------------|---------|---------|----------------|
| `GET /health` | Runs a real query; returns API, engine and schema versions | 200 | 500 |
| `GET /capabilities` | Projections, snapshot policies, limits, benchmark availability | 200 | |
| `GET /overview` | Store counts, 10 most recent events, 10 most recently active streams | 200 | |
| `GET /streams?after=&limit=` | Streams ordered by id, with a cursor (`next_after`) | 200 | 400 |
| `POST /streams` | `{stream_id, metadata?}` creates a stream | 201 | 400 `invalid_stream_id`, 409 `stream_already_exists`, 422 |
| `GET /streams/{id}` | Summary and metadata | 200 | 404 `stream_not_found` |
| `GET /streams/{id}/events?after=&limit=` | Events in sequence order, with a cursor | 200 | 400, 404 |
| `POST /streams/{id}/events` | `{event_type, payload, metadata?, schema_version?, expected_sequence?, event_id?}` | 201 | 409 `sequence_conflict` (with expected and actual heads), 409 `duplicate_event_id`, 413/422 `payload_too_large` |
| `GET /streams/{id}/events/{seq}` | One event | 200 | 404 `event_not_found` |
| `GET /streams/{id}/state?projection=&at=&snapshots=&record_events=` | Reconstructs the head or `at` | 200 | 422 `target_beyond_head`, 422 `projection_failed`, 422 `snapshot_unusable` (strict) |
| `GET /streams/{id}/diff?projection=&from=&to=&snapshots=` | Structural diff between two positions (`to` defaults to the head) | 200 | as above |
| `GET /streams/{id}/explain?projection=&from=&to=` | Per-event changes, at most 1,000 events | 200 | 400 `interval_too_large`, 400 `invalid_range` |
| `GET /streams/{id}/snapshots` | Snapshots, each validated for this response (`validation.status`, `checked_by`, `problem`) | 200 | 500 `integrity_violation` for a malformed row |
| `POST /streams/{id}/snapshots` | `{projection?, at?}` reconstructs and stores a snapshot | 201 created, or 200 if identical | 422 `validation_failed` (at 0), 409 |
| `GET /snapshots/{snapshot_id}` | One snapshot, validated | 200 | 400, 404 |
| `GET /streams/{id}/checkpoint` | The current head as a checkpoint | 200 | 404 (unknown or empty stream) |
| `POST /streams/{id}/verify` | `{checkpoints: [...]}` returns an `IntegrityReport`. **Defects come back as 200 with `total_issues > 0`.** | 200 | 400 (bad digest), 404 |
| `POST /verify` | Verifies the whole store (`StoreIntegrityReport` plus `valid`) | 200 | 500 |
| `GET /streams/{id}/replays` | Replay runs, with execution counts and last outcome | 200 | 404 |
| `POST /streams/{id}/replays` | `{projection?, start?, to?}` records a run | 201 | 409 `history_integrity` (damaged interval), 422 |
| `GET /replays/{run_id}` | The run (states, trace) and every execution | 200 | 400, 404, 500 (corrupt record) |
| `POST /replays/{run_id}/executions` | `{projection?}` executes the run (default: the recorded projection). **Divergence, mismatch and integrity failures are 201 outcomes.** | 201 | 400, 404 |
| `POST /streams/{id}/experiments` | `{projection?, to?, snapshot_policy?}` returns an `ExperimentReport` | 201 | 404, 422 |
| `GET /benchmarks` | Criterion results (median, CI, mean, measurement time) | 200 | |
| `GET /lab/…` | See [Lab endpoints](#lab-endpoints) | | |
| `GET /openapi.json` | The OpenAPI document | 200 | |

## Lab endpoints

These endpoints serve the experiment lab ([LAB.md](LAB.md)). Its registry is
a separate database, and these endpoints never write to the event store.
Responses are the lab's own serialized records.

| Method and path | Purpose | Success | Notable errors |
|-----------------|---------|---------|----------------|
| `GET /lab/implementations` | Registered implementation descriptors (id, name, declared projection, type, fingerprint) | 200 | 503 `lab_unavailable` |
| `GET /lab/experiments?before=&limit=&kind=` | Experiment summaries, newest first, with a cursor (`next_before`); `kind` filters by `differential_replay`, `reliability` or `performance` | 200 | 400 `invalid_parameter`, 400 `invalid_experiment_id` |
| `POST /lab/experiments` | `{kind: "differential_replay", spec: {stream, left, right, from?, to?, reference?, determinism_check?}}` validates, records and runs an experiment. **A missing or damaged history is a `failed` experiment (201), not an HTTP error.** `{kind: "performance", spec: {workload, input_size?, warmup_iterations?, measured_iterations?, seed?}}` records it and measures it **in the background** (202, status `created`; poll the experiment). | 201, 202 | 400 `invalid_spec` (unknown fields are rejected), 400 `unknown_implementation`, 400 `unsupported_experiment_kind`, 409 `measurement_in_progress` |
| `GET /lab/experiments/{id}` | The stored experiment (spec, descriptors, build, lifecycle timestamps, input range, result, artifacts) and its verifications | 200 | 400, 404 `experiment_not_found` |
| `POST /lab/experiments/{id}/verify` | Re-hashes the stored spec, result and every artifact. Records and returns the report (`intact` or `damaged`). | 200 | 404, 409 `invalid_state` (not finished) |
| `POST /lab/experiments/{id}/reproduce` | Differential replay: re-executes from the stored spec and compares with the original (`reproduced`, `differs`, `configuration_mismatch`, `not_reproducible`, `history_changed`, `history_unavailable`, `implementation_unavailable`, `artifact_missing`; see [LAB.md](LAB.md#reproducing-and-verifying)), and records the attempt (201). Performance: starts a linked new measurement of the same spec (202, the new experiment); the comparison is appended to the original when it finishes. | 201, 202 | 404, 409 `invalid_state` (not completed, or a reliability experiment), 409 `measurement_in_progress` |
| `GET /lab/experiments/{id}/bundle?history=` | The experiment as an evidence bundle: `application/json` attachment `orbit-evidence-<id>.json`, digest in `X-Orbit-Bundle-Digest`. A differential replay's input events are included while the store still holds the pinned history, unless `history=false` ([EVIDENCE.md](EVIDENCE.md)) | 200 | 404, 409 `invalid_state` (not finished, or larger than 32 MiB), 500 `integrity_violation` (damaged artifacts are never exported) |
| `GET /lab/performance/workloads` | Workload catalog with bounds, iteration bounds, default regression policy, the server's build profile and whether a measurement is running | 200 | 503 |
| `POST /lab/performance/compare` | `{baseline?, candidates: [id…] (1–10), threshold_percent?, alpha?}`. Compares completed performance experiments; nothing is stored. Without `baseline`, the newest compatible earlier one is used, or `status: "no_baseline"` ([PERFORMANCE.md](PERFORMANCE.md)) | 200 | 400 `invalid_parameter`, 404, 409 `invalid_state` (not a completed performance experiment) |
| `POST /lab/performance/compare-runs` | `{baseline: [id…], candidates: [id…] (1–20 each), threshold_percent?, alpha?}`. Compares sets of independent runs on their per-run medians, with between-run spread, an exact test for small counts, and the smallest p-value the counts allow | 200 | 400 `invalid_parameter`, 422 `invalid_body` (unknown fields), 404, 409 |
| `POST /lab/bundles/validate` | Body: the bundle file (raw bytes, at most 32 MiB). Validates without storing; always 200 with the report (`valid`, `errors[].code`, `integrity`, `scientific_correctness: "not_verified"`, `history`, and `authenticity`: `unsigned`, `signed_untrusted` or `trusted`, judged only against `ORBIT_TRUSTED_KEYS`) | 200 | 413 |
| `POST /lab/bundles` | Body: the bundle file. Validates and imports; never creates or changes a local experiment | 201 (new), 200 (already imported) | 413, 422 `invalid_bundle` (report in `details`; nothing stored) |
| `GET /lab/bundles?before=&limit=` | Imported bundles, newest first, with a cursor | 200 | 400 |
| `GET /lab/bundles/{digest}` | Manifest, document (without entry contents), import-time validation report, reproduction attempts | 200 | 400 `invalid_bundle_digest`, 404 `bundle_not_found` |
| `POST /lab/bundles/{digest}/reproduce` | Reproduces an imported bundle and appends the attempt: differential replays over the bundle's verified events (201); performance bundles are measured again in the background (202); reliability `unsupported`, failed `not_reproducible` | 201, 202 | 404, 409 `measurement_in_progress` |
| `GET /lab/reliability/scenarios` | The reliability scenario catalog and `runner`: `{enabled: false, reason}` or `{enabled: true, timeout_secs, busy}` ([RELIABILITY.md](RELIABILITY.md)). Results are ordinary experiments (`kind: "reliability"`). | 200 | 503 `lab_unavailable` |
| `POST /lab/reliability/runs` | Body: a reliability spec `{scenario, scenario_version?, parameters?}` (catalog scenarios only; unknown fields and out-of-range parameters are refused). Records it and starts the configured runner process on it with fixed arguments (`--lab <lab dir> run-id <id>`); responds with the experiment in status `created`. One run at a time. | 202 | 400 `invalid_spec`, 409 `reliability_run_in_progress`, 503 `reliability_runner_disabled` |
| `GET /lab/experiments/{id}/trace?from=&limit=` | Per-boundary trace of a completed experiment: `{boundary, left, right, equal}` read from the verified trace artifacts, with a cursor (`next_from`) | 200 | 400 `invalid_parameter` (limit 1–500), 404, 409 `invalid_state` (not completed), 500 `integrity_violation` (trace artifact missing or corrupted) |

Experiment records include `identity` (declared, configuration and build
identity, `completeness` and `gaps`). Results recorded since Sprint 8
(`result_version` 2) include `timeline`, `diagnosis`, `perspective` and
`semantics`; older results are returned as stored. See [LAB.md](LAB.md).

`POST /lab/experiments` accepts `differential_replay` and `performance`. A
request with `kind: "reliability"` gets `400 unsupported_experiment_kind`:
this server never runs a scenario itself. With a configured runner,
`POST /lab/reliability/runs` launches one in that separate process.
Listing, inspecting, verifying and exporting reliability experiments works
like any other kind; reproducing one through the API returns
`409 invalid_state` (use `orbit-reliability reproduce`).

**Execution and cancellation.**
- **Differential replays** run synchronously. If one outlasts the request
  deadline, the response is `503 request_timeout`, and the experiment still
  finishes and is recorded.
- **Performance measurements** (new experiments, reproduction attempts and
  bundle reproductions) run on one background thread per server, one at a
  time; a second request gets `409 measurement_in_progress` and records
  nothing. The registry is the only source of status: poll the experiment.
- **Reliability runs** execute in the separate runner process, one at a time
  per server, with a deadline (`ORBIT_RELIABILITY_TIMEOUT_SECS`) after which
  the runner and its children are killed and the run is recorded as an
  infrastructure failure. A runner that exits without starting the
  experiment is also an infrastructure failure; one that dies mid-run leaves
  it `interrupted`. Finished records are never changed. If the API stops, a
  runner that already started continues and records its result.
- **Nothing can be cancelled by a client.** A request timeout does not stop
  work. If the server stops during a measurement, the experiment stays
  `running` and is marked `failed`/`interrupted` the next time the lab is
  opened.
- Measurements started here run inside the server process, so concurrent
  requests can disturb them; [PERFORMANCE.md](PERFORMANCE.md) describes how
  to measure with the CLI.

**Bundles.** Upload bodies are raw bytes, never parsed by the framework,
and are passed to the validator, which bounds size, nesting, entry count
and sizes and rejects unsafe entry names. Bundle contents are never
executed or written under their names. CORS exposes `X-Orbit-Bundle-Digest`
and `Content-Disposition` to the configured origins.

```bash
curl -s -X POST $API/lab/experiments -H 'content-type: application/json' -d \
  '{"kind":"differential_replay","spec":{"stream":"counter-demo","left":"orbit.counter","right":"example.counter-floor-at-zero"}}'
```

## Errors

Every error has the same shape:

```json
{
  "error": {
    "code": "sequence_conflict",
    "message": "sequence conflict on stream orders-1: expected head 0, actual head 2",
    "class": "conflict",
    "details": { "stream_id": "orders-1", "expected_sequence": 0, "actual_sequence": 2 }
  }
}
```

`class` is the engine's `ErrorClass` (`retryable`, `conflict`,
`invalid_request`, `not_found`, `integrity`, `incompatible`, `database`) when
the error came from the engine. The HTTP status follows from it:

| Status | Meaning |
|--------|---------|
| 400 | Malformed parameter or path, or failed validation |
| 403 | `host_not_allowed`: the `Host` header is not in the allowlist |
| 404 | Missing resource, or an unknown route (`route_not_found`) |
| 409 | Conflict |
| 413 | Request body over the limit |
| 415 | `POST` body not sent as `application/json` |
| 422 | The engine rejected the request: target beyond head, projection failure, unusable snapshot, payload over the store limit, or a malformed body |
| 500 | Integrity violation, database or incompatibility |
| 503 | `busy` (the SQLite lock was not obtained within the busy timeout) or `request_timeout` (the request deadline passed); both are retryable |

## Example session

```bash
API=http://127.0.0.1:7878/api/v1
curl -s -X POST $API/streams -H 'content-type: application/json' -d '{"stream_id":"order-1"}'
curl -s -X POST $API/streams/order-1/events -H 'content-type: application/json' \
  -d '{"event_type":"order.created","payload":{"status":"new"},"expected_sequence":0}'
curl -s -X POST $API/streams/order-1/events -H 'content-type: application/json' \
  -d '{"event_type":"order.paid","payload":{"status":"paid","total":30}}'
curl -s "$API/streams/order-1/state?at=1"          # {"state":{"status":"new"}, ...}
curl -s "$API/streams/order-1/diff?from=1"         # /status modified, /total added
curl -s -X POST $API/streams/order-1/snapshots -H 'content-type: application/json' -d '{}'
curl -s -X POST $API/streams/order-1/replays -H 'content-type: application/json' -d '{}'
curl -s -X POST $API/verify
```

`scripts/seed-demo.sh` builds a richer demonstration dataset through the API.
