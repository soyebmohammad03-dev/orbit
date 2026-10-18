# Performance lab

The performance lab measures ORBIT operations as **performance
experiments**: registered in the lab registry like every other experiment,
with a validated specification, the standard lifecycle
(`created → running → completed | failed`), every sample, statistics,
the environment and provenance. Code: `crates/orbit-lab/src/performance/`
(`mod.rs` workloads, specs and execution; `stats.rs`; `compare.rs`).

The Criterion benchmarks in `crates/orbit-core/benches` are unchanged.
They remain the microbenchmark harness (`cargo bench`), and the
Observatory's Benchmarks page reads their output. They are not registered
experiments, and the performance lab does not compare them.

## Workloads

| Id | One measured iteration | `input_size` (default, bounds) |
|---|---|---|
| `event-append` | append `input_size` ~100-byte events to a new stream, one transaction per event, durability `normal` | events (200, 1–10 000) |
| `historical-reconstruction` | reconstruct the latest counter state by full replay, snapshots ignored | events in the stream (2 000, 1–100 000) |
| `snapshot-reconstruction` | reconstruct from a validated snapshot at 90 % of the stream (strict policy), replaying the last tenth | events (2 000, 10–100 000) |
| `integrity-verification` | verify the digest chain of the stream | events (2 000, 1–100 000) |
| `differential-replay` | resolve and verify the input, then replay `orbit.counter` against `example.counter-floor-at-zero` (no determinism re-run, no registry writes) | events (2 000, 1–100 000) |
| `registry-operations` | create and run `input_size` small differential experiments in a scratch lab, including artifact writes | experiments (5, 1–200) |
| `artifact-persistence` | write one new artifact (temporary file, `fsync`, rename) and read it back with digest verification | KiB (256, 1–16 384) |

Each workload prepares its own files in
`<lab dir>/performance-work/<experiment id>/`, which is removed afterwards.
A user's event store is never touched. Inputs are generated from `seed`
(default 1) with a fixed xorshift generator, so equal specs measure equal
inputs.

## Method

1. Warm-up iterations (default 3, 0–100) are timed and recorded but
   excluded from the statistics.
2. Measured iterations (default 20, 1–500) time **only** the operation,
   with `std::time::Instant` (monotonic).
3. After every iteration, outside the timed section, the workload checks
   its result:
   - the appended head;
   - the reconstructed state, and the number of events applied;
   - that verification is valid;
   - that the differential outcome stayed stable;
   - that every experiment completed;
   - that the artifact read back matches.

   A wrong result fails the experiment (`failed`, kind `internal`) and
   produces no numbers. A problem preparing the work directory fails it
   with kind `infrastructure`.
4. The result records every sample in order, the statistics, the
   configuration digest (`ORBIT-LAB-PERFORMANCE-CONFIG-V1` over workload,
   version, input size and seed), the environment (OS, architecture,
   available parallelism, build profile, ORBIT version, git commit,
   executable digest, and the **process** that measured: `orbit-lab` for
   the CLI, `orbit-api` when started through the API), the **overhead**
   around the timed sections (workload preparation, and the untimed
   per-iteration setup and checks), and its limitations.

**Independent execution.** The CLI (`orbit-lab measure`) measures in a
process of its own, with nothing else running in it; that is the path for
real measurements. A measurement started through the API runs inside the
API server, alongside its other requests; such results say so, and
comparing results from different processes produces a warning.

### Evidence levels

- **`smoke`**: produced by an unoptimized (debug) build, or with fewer
  than 10 measured iterations. It shows that the workload runs, not how
  fast it is. The test suite and CI only produce smoke results.
- **`measurement`**: samples from **one run in one process on one
  machine**. Its intervals describe variability within that run. They say
  nothing about other runs, machines or load conditions.

## Statistics (`stats.rs`)

| Statistic | Definition | Reported when |
|---|---|---|
| count, mean, median, min, max | as usual; median of an even count is the mean of the two middle values | always (at least 1 sample) |
| standard deviation | sample standard deviation, divisor `n − 1` | n ≥ 2 |
| MAD | unscaled median absolute deviation from the median | always |
| coefficient of variation | `sd / mean` | n ≥ 2 and mean > 0 |
| percentiles p90, p95, p99 | linear interpolation between order statistics ("type 7", as `numpy.percentile`) | p only when n ≥ 1/(1 − p): p90 from 10, p95 from 20, p99 from 100 |
| 95 % interval of the median | distribution-free: order statistics `[x(k), x(n−k+1)]` with the largest k whose binomial coverage is ≥ 95 %; the actual coverage is reported | n ≥ 6 |
| 95 % interval of the mean | Student t with `n − 1` degrees of freedom; assumes a roughly normal sample mean, which skewed timings only approach with more samples | n ≥ 10 |

Anything not computed is listed in `summary.notes`, never filled in. NaN,
infinite or negative samples are rejected.

The t quantile comes from the regularized incomplete beta function, by
bisection. The normal CDF uses a Chebyshev fit of erfc with fractional
error below 1.2e-7. Both are tested against independent references:
- Student t tables for df 1, 2, 9, 19, 29 and 99;
- Python's `statistics.NormalDist`;
- `statistics.mean`, `median`, `stdev` and
  `quantiles(method="inclusive")` for the descriptive statistics;
- SciPy's documented Mann–Whitney example (U = 17; exact p = 14/126 =
  0.1111; asymptotic p = 0.1113), and exact p-values checked by brute-force
  enumeration of every rank assignment in Python.

**Mann–Whitney p-values** are **exact** when there are no ties and at most
50 values in total: U's null distribution is the Gaussian binomial
coefficient `[n1+n2 choose n1]_q`, computed by recurrence. Otherwise the
normal approximation with tie and continuity corrections is used. The
report names the method (`mann_whitney_u_exact` or
`mann_whitney_u_normal_approximation`). Exactness matters for small
samples: with 5 values per side the smallest possible two-sided p is
2/252 = 0.0079, and with 3 per side it is 0.1, so no 3-versus-3
comparison can be significant.

## Comparison (`compare.rs`)

Numbers are compared only if both results measure the same thing in a
comparable environment.

**Incompatible**, so nothing is compared, when any of these differ:
- workload or workload version;
- input size or seed;
- result layout;
- OS, architecture or build profile.

**Warnings**, with the comparison still made:
- iteration counts or available CPUs;
- the executable (comparing builds is the point);
- either side being smoke evidence.

The change is measured on medians: `relative_change = (candidate −
baseline) / baseline`, so positive means slower. The policy is
configurable: `threshold_percent` (0.1–100, default 10) and `alpha`
(0.0001–0.1, default 0.01).

| Verdict | Condition |
|---|---|
| `incompatible` | see above |
| `insufficient_samples` | fewer than 3 samples on a side |
| `within_threshold` | \|relative change\| < threshold, **whatever the test says** |
| `suspected_regression` / `suspected_improvement` | above the threshold, but the two-sided Mann–Whitney U test is not significant at α, or is not performed (fewer than 10 samples on a side) |
| `regression` / `improvement` | above the threshold **and** significant at α |

Only `regression` and `improvement` are statistically supported findings,
and only about the two runs compared. CI never fails on a verdict: it only
checks that the workloads run.

### Comparing sets of independent runs

A single run's intervals describe variation **within** that run. Whether
two configurations or builds really differ needs several independent runs
of each (ideally separate processes), compared on their per-run medians:
`compare-runs` (CLI), `POST /lab/performance/compare-runs` (API),
`Lab::compare_performance_runs`.

- Every run must be compatible with the first baseline run; a single
  incompatible run stops the comparison and is named.
- 2–20 runs per side. The test on the run medians needs at least 3 per
  side, and the report gives `min_achievable_p`: the smallest p-value the
  run counts allow. When it is not below α, the explanation says that no
  significant result was possible.
- The verdicts are the same as for single runs, applied to the median of
  the run medians. The between-run standard deviation is reported.

**Baselines.** Pass one explicitly, or omit it. Without one, the newest
completed performance experiment created before the first candidate is
used, provided it has the same configuration digest and a compatible
environment. With none, the response is `status: no_baseline` and makes no
comparison. Each side's provenance is returned: id, creation time, build,
environment, evidence level and sample count.

**Reproduction.** `orbit-lab reproduce ID`, or `POST
/lab/experiments/{id}/reproduce`, measures the identical spec again as a
new, linked experiment, compares the two and appends a verification to the
original. The original is never modified.

| Verdict | Reproduction status |
|---|---|
| `within_threshold` | `reproduced` |
| `regression` or `improvement` | `differs` |
| suspected, or insufficient samples | `inconclusive` |
| `incompatible` | `configuration_mismatch` |

## Usage

```bash
cargo run --release -p orbit-lab -- --lab data/lab workloads
```

```bash
cargo run --release -p orbit-lab -- --lab data/lab measure historical-reconstruction --input-size 2000 --warmup 3 --iterations 30
```

```bash
cargo run --release -p orbit-lab -- --lab data/lab compare --threshold 10 --alpha 0.01 CANDIDATE_ID
```

Independent runs are separate invocations of `measure`; then:

```bash
cargo run --release -p orbit-lab -- --lab data/lab compare-runs --baseline ID1,ID2,ID3,ID4,ID5 --candidate ID6,ID7,ID8,ID9,ID10
```

Through the API, `POST /api/v1/lab/experiments` with `{"kind":
"performance", "spec": {...}}` returns 202 and measures in the background
(see [API.md](API.md)). The Observatory's **Performance** page does the
same.

## Recorded measurements (one run)

These are real outputs of the commands above. They were recorded once, on
2026-10-18, on one machine: macOS, aarch64, 8 available CPUs, release
build at commit `3339bfe`. Each workload had 3 warm-up and 30 measured
iterations. They are single-run measurements, not statistically reliable
benchmarks, and no claim beyond this run is made.

| Workload (input) | Median | Mean | SD | Min – max | p95 | 95 % median interval |
|---|---|---|---|---|---|---|
| event-append (200 events) | 17.01 ms | 16.86 ms | 1.19 ms | 13.40 – 18.36 ms | 18.18 ms | 16.85 – 17.25 ms |
| historical-reconstruction (2 000) | 2.120 ms | 2.122 ms | 8.1 µs | 2.110 – 2.146 ms | 2.135 ms | 2.117 – 2.124 ms |
| snapshot-reconstruction (2 000) | 254.3 µs | 255.9 µs | 4.4 µs | 252.9 – 270.8 µs | 266.4 µs | 253.7 – 255.1 µs |
| integrity-verification (2 000) | 2.245 ms | 2.247 ms | 8.5 µs | 2.236 – 2.274 ms | 2.259 ms | 2.242 – 2.249 ms |
| differential-replay (2 000) | 5.878 ms | 5.882 ms | 15.3 µs | 5.858 – 5.916 ms | 5.907 ms | 5.872 – 5.891 ms |
| registry-operations (5 experiments) | 5.440 ms | 5.579 ms | 0.672 ms | 4.866 – 7.839 ms | 6.646 ms | 5.059 – 5.902 ms |
| artifact-persistence (256 KiB) | 3.821 ms | 3.558 ms | 0.749 ms | 2.357 – 5.619 ms | 4.332 ms | 3.142 – 3.877 ms |

The coverage of every median interval at n = 30 is 0.9572 (order
statistics 10 and 21). The snapshot workload replays the last 200 events
instead of 2 000. The difference from full replay is a measured fact of
this run, not a general speedup claim.

**A real comparison.** A second historical-reconstruction run, compared
with no baseline given (so the earlier run was selected automatically):

```text
compared newest_compatible_before_candidate 01a14fa9-c6bc-7283-b0ea-fd3d8ec2d2c5
within_threshold base=2119792 cand=2079646 rel=-0.0189 U=38.0 p=1.174e-09 overlap= False
the median changed by -1.89 %, less than the 10 % threshold
```

The test is highly significant, but the change is below the threshold, so
it is not reported as an improvement. This is the intended behaviour: with
tight samples, a tiny difference can be "significant" without mattering.
A run with `--input-size 4000` against the same baseline:

```text
incompatible not compared: input_size: Some(2000) vs Some(4000)
```

Reproducing the integrity-verification experiment:

```text
reproduced within_threshold rel=-0.0012 p=0.2428 01a14fa9-f81d-7404-80a1-e676e7acd13b
```

## Recorded comparison of independent runs (5 + 5)

Real outputs, recorded on 2026-10-18 on the same machine (macOS, aarch64,
8 available CPUs), release build at commit `6254309` (version 0.2.0),
measured by the CLI (`process: orbit-lab`). Ten separate `orbit-lab
measure historical-reconstruction --input-size 2000 --warmup 3
--iterations 30` processes ran one after another: five as the baseline,
five as the candidate. Same code on both sides, so this measures
run-to-run variation, not an improvement.

| | Run medians (ms) | Median of medians | Between-run SD |
|---|---|---|---|
| Baseline | 1.2587, 1.2107, 1.2082, 1.2502, 1.2248 | 1.2248 ms | 22.9 µs |
| Candidate | 1.2236, 1.2062, 1.2133, 1.2228, 1.2076 | 1.2133 ms | 8.2 µs |

```text
verdict: within_threshold
relative_change: -0.0094 (−0.94 %)
test: mann_whitney_u_exact, p = 0.2222 (min_achievable_p = 0.0079)
explanation: the median of the run medians changed by -0.94 %, less than the 10 % threshold
```

Five snapshot-reconstruction runs offered as the candidate against the same
baseline:

```text
verdict: incompatible
explanation: not compared: candidate run 1: workload: historical-reconstruction vs snapshot-reconstruction; … candidate run 5: …
```

The first baseline run's overhead: 97.9 ms preparing the workload (seeding
2,000 events), 44.4 ms for the whole loop of 33 iterations, of which
8.6 µs was untimed setup and checks.

**Between-session variation.** The single run of the same workload
recorded earlier the same day (table above, commit `3339bfe`) had a median
of 2.120 ms; these runs, a few hours later, had about 1.22 ms. The code
of this workload did not change in between. The machine's state did
(other load, thermal and power conditions), which is why one run is not a
benchmark, and why only comparisons of runs taken together under the same
conditions are meaningful.

## Limitations

- **One process, one machine.** File-system caches, CPU frequency scaling
  and other processes are not controlled. Measurements started from the
  API run inside the API server process, alongside its other requests.
- **Wall-clock time.** Durations include any time the thread was
  descheduled.
- **Between-run comparisons need several runs.** One run's intervals are
  within-run. `compare-runs` uses independent runs, but how many runs, how
  far apart in time, and under what load is up to whoever measures; the
  lab does not schedule or isolate them.
- **Approximate p-values for large or tied samples.** Above 50 values in
  total, or with ties, the normal approximation is used. The sample-level
  test runs only with at least 10 samples per side.
- **No cancellation.** A running measurement cannot be cancelled. If its
  process stops, it is marked `failed`/`interrupted` when the lab is next
  opened.
