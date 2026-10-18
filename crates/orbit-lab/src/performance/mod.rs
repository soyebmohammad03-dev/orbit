//! The performance lab: repeatable measurements of ORBIT operations,
//! recorded as `performance` experiments, summarized statistically and
//! compared with explicit compatibility and regression rules.
//!
//! # Method
//!
//! A [`PerformanceSpec`] names a workload from [`WORKLOADS`], its input size,
//! a seed for generated inputs, and how many warm-up and measured
//! iterations to run. Running it:
//!
//! 1. prepares the workload in `<lab dir>/performance-work/<id>/` (removed
//!    afterwards; a user's event store is never touched);
//! 2. runs the warm-up iterations (timed, recorded, excluded from statistics);
//! 3. runs the measured iterations, timing only the operation itself with a
//!    monotonic clock (`std::time::Instant`), and checks each iteration's
//!    result, untimed. A wrong result fails the experiment: it is never
//!    turned into a number;
//! 4. records every sample, the [`stats::Summary`], the configuration
//!    digest and the [`Environment`] in the result.
//!
//! # Evidence levels
//!
//! A result is `smoke` when it was produced by an unoptimized (debug) build
//! or with fewer than [`MIN_MEASUREMENT_SAMPLES`] measured iterations: it
//! shows that the workload runs, not how fast it is. Otherwise it is a
//! `measurement`: samples from **one run in one process on one machine**.
//! Its intervals describe variability within that run only; they say
//! nothing about other runs, machines or load conditions.
//!
//! # Comparison
//!
//! See [`compare`]. Two results are compared only if they measure the same
//! thing in a comparable environment; otherwise the comparison is
//! `incompatible` and no numbers are compared.

pub mod compare;
pub mod stats;
pub mod workloads;

use std::path::PathBuf;
use std::time::Instant;

use orbit_core::Digest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::differential::RunFailure;
use crate::digest::{RESULT_TAG, value_digest};
use crate::error::{LabError, Result};
use crate::identity::BuildInfo;
use crate::registry::{
    Completion, ExperimentId, Failure, FailureKind, NewExperiment, Status, VerificationKind,
    VerificationRecord,
};
use crate::reproduction::ReproductionStatus;
use crate::{ExperimentRecord, Lab};

pub use compare::{Comparison, RegressionPolicy, Verdict};
pub use stats::Summary;

/// Experiment kind of performance experiments.
pub const PERFORMANCE: &str = "performance";
/// Specification format version.
pub const SPEC_VERSION: u32 = 1;
/// Result layout version.
pub const RESULT_VERSION: u32 = 1;
/// Fewest measured iterations for a result to count as a measurement.
pub const MIN_MEASUREMENT_SAMPLES: u64 = 10;
/// Tag of the configuration digest.
pub const CONFIGURATION_TAG: &str = "ORBIT-LAB-PERFORMANCE-CONFIG-V1";

/// A bounded integer of a spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Bound {
    /// Default.
    pub default: u64,
    /// Smallest value.
    pub min: u64,
    /// Largest value.
    pub max: u64,
}

/// A supported workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WorkloadInfo {
    /// Stable identifier.
    pub id: &'static str,
    /// Bumped whenever what is measured changes; results of different
    /// versions are never compared.
    pub version: u32,
    /// Short title.
    pub title: &'static str,
    /// What one measured iteration does.
    pub iteration: &'static str,
    /// What `input_size` means.
    pub input_size_meaning: &'static str,
    /// Unit of `input_size` for throughput (`events`, `experiments`, `KiB`).
    pub input_unit: &'static str,
    /// Bounds of `input_size`.
    pub input_size: Bound,
}

/// The supported workloads. Nothing else can be run.
pub const WORKLOADS: &[WorkloadInfo] = &[
    WorkloadInfo {
        id: "event-append",
        version: 1,
        title: "Event append",
        iteration: "append input_size events (about 100 bytes of JSON each) to a new stream, one transaction per event, durability normal",
        input_size_meaning: "events appended per iteration",
        input_unit: "events",
        input_size: Bound {
            default: 200,
            min: 1,
            max: 10_000,
        },
    },
    WorkloadInfo {
        id: "historical-reconstruction",
        version: 1,
        title: "Historical reconstruction (full replay)",
        iteration: "reconstruct the latest counter state by replaying every event, snapshots ignored",
        input_size_meaning: "events in the stream",
        input_unit: "events",
        input_size: Bound {
            default: 2_000,
            min: 1,
            max: 100_000,
        },
    },
    WorkloadInfo {
        id: "snapshot-reconstruction",
        version: 1,
        title: "Snapshot-assisted reconstruction",
        iteration: "reconstruct the latest counter state from a validated snapshot at 90 % of the stream, replaying the last tenth (strict snapshot policy)",
        input_size_meaning: "events in the stream",
        input_unit: "events",
        input_size: Bound {
            default: 2_000,
            min: 10,
            max: 100_000,
        },
    },
    WorkloadInfo {
        id: "integrity-verification",
        version: 1,
        title: "Integrity verification",
        iteration: "verify the digest chain of the whole stream",
        input_size_meaning: "events in the stream",
        input_unit: "events",
        input_size: Bound {
            default: 2_000,
            min: 1,
            max: 100_000,
        },
    },
    WorkloadInfo {
        id: "differential-replay",
        version: 1,
        title: "Differential replay",
        iteration: "resolve and verify the input, then replay orbit.counter against example.counter-floor-at-zero (no determinism re-run, no registry writes)",
        input_size_meaning: "events in the stream",
        input_unit: "events",
        input_size: Bound {
            default: 2_000,
            min: 1,
            max: 100_000,
        },
    },
    WorkloadInfo {
        id: "registry-operations",
        version: 1,
        title: "Experiment registry operations",
        iteration: "create and run input_size small differential experiments (10 events) in a scratch lab, including artifact writes",
        input_size_meaning: "experiments per iteration",
        input_unit: "experiments",
        input_size: Bound {
            default: 5,
            min: 1,
            max: 200,
        },
    },
    WorkloadInfo {
        id: "artifact-persistence",
        version: 1,
        title: "Artifact persistence and verification",
        iteration: "write one new artifact of input_size KiB (temporary file, fsync, rename) and read it back with digest verification",
        input_size_meaning: "artifact size in KiB",
        input_unit: "KiB",
        input_size: Bound {
            default: 256,
            min: 1,
            max: 16_384,
        },
    },
];

/// Bounds of the warm-up iteration count.
pub const WARMUP: Bound = Bound {
    default: 3,
    min: 0,
    max: 100,
};
/// Bounds of the measured iteration count.
pub const MEASURED: Bound = Bound {
    default: 20,
    min: 1,
    max: 500,
};

/// The workload with this id.
#[must_use]
pub fn workload(id: &str) -> Option<&'static WorkloadInfo> {
    WORKLOADS.iter().find(|w| w.id == id)
}

/// What to measure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceSpec {
    /// Workload id from [`WORKLOADS`].
    pub workload: String,
    /// Workload version; filled in from the catalog when validated.
    #[serde(default)]
    pub workload_version: Option<u32>,
    /// Input size (see the workload's `input_size_meaning`).
    #[serde(default)]
    pub input_size: Option<u64>,
    /// Warm-up iterations, excluded from the statistics.
    #[serde(default)]
    pub warmup_iterations: Option<u64>,
    /// Measured iterations.
    #[serde(default)]
    pub measured_iterations: Option<u64>,
    /// Seed of the generated inputs.
    #[serde(default)]
    pub seed: Option<u64>,
}

fn bounded(field: &'static str, value: Option<u64>, bound: Bound) -> Result<u64> {
    let v = value.unwrap_or(bound.default);
    if (bound.min..=bound.max).contains(&v) {
        Ok(v)
    } else {
        Err(LabError::InvalidSpec {
            field,
            reason: format!("must be between {} and {}", bound.min, bound.max),
        })
    }
}

impl PerformanceSpec {
    /// Parses a JSON spec, rejecting unknown fields.
    ///
    /// # Errors
    /// [`LabError::InvalidSpec`].
    pub fn from_json(value: Value) -> Result<Self> {
        serde_json::from_value(value).map_err(|e| LabError::InvalidSpec {
            field: "spec",
            reason: e.to_string(),
        })
    }

    /// Checks the spec against the catalog and returns it with every field
    /// filled in.
    ///
    /// # Errors
    /// [`LabError::InvalidSpec`].
    pub fn validated(&self) -> Result<(Self, &'static WorkloadInfo)> {
        let info = workload(&self.workload).ok_or_else(|| LabError::InvalidSpec {
            field: "workload",
            reason: format!(
                "unknown workload {:?}; supported: {}",
                self.workload,
                WORKLOADS
                    .iter()
                    .map(|w| w.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })?;
        if let Some(v) = self.workload_version
            && v != info.version
        {
            return Err(LabError::InvalidSpec {
                field: "workload_version",
                reason: format!("{} is version {}, not {v}", info.id, info.version),
            });
        }
        Ok((
            Self {
                workload: info.id.to_owned(),
                workload_version: Some(info.version),
                input_size: Some(bounded("input_size", self.input_size, info.input_size)?),
                warmup_iterations: Some(bounded(
                    "warmup_iterations",
                    self.warmup_iterations,
                    WARMUP,
                )?),
                measured_iterations: Some(bounded(
                    "measured_iterations",
                    self.measured_iterations,
                    MEASURED,
                )?),
                seed: Some(self.seed.unwrap_or(1)),
            },
            info,
        ))
    }

    /// Digest of what is measured: workload, version, input size and seed.
    /// Iteration counts are excluded: they change how much evidence there
    /// is, not what is measured. Validated specs only.
    #[must_use]
    pub fn configuration_digest(&self) -> Digest {
        value_digest(
            CONFIGURATION_TAG,
            &json!({
                "workload": self.workload,
                "workload_version": self.workload_version,
                "input_size": self.input_size,
                "seed": self.seed,
            }),
        )
    }
}

/// Where a measurement was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    /// `std::env::consts::OS`.
    pub os: String,
    /// `std::env::consts::ARCH`.
    pub arch: String,
    /// `std::thread::available_parallelism`, if known.
    pub available_parallelism: Option<u64>,
    /// `release` (optimized) or `debug`.
    pub build_profile: String,
    /// The build.
    pub build: BuildInfo,
}

impl Environment {
    /// The running environment.
    #[must_use]
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            available_parallelism: std::thread::available_parallelism()
                .ok()
                .map(|n| n.get() as u64),
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_owned(),
            build: BuildInfo::current(),
        }
    }
}

/// The result document of a performance experiment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerformanceResult {
    /// Layout version.
    pub result_version: u32,
    /// The workload (id, version, title, iteration, input unit).
    pub workload: Value,
    /// The validated spec.
    pub configuration: PerformanceSpec,
    /// [`PerformanceSpec::configuration_digest`].
    pub configuration_digest: Digest,
    /// Where it was measured.
    pub environment: Environment,
    /// `smoke` or `measurement` (see the module documentation).
    pub evidence_level: String,
    /// Why it is `smoke`, when it is.
    pub evidence_notes: Vec<String>,
    /// Unit of every sample and statistic.
    pub unit: String,
    /// Duration of each warm-up iteration (excluded from statistics).
    pub warmup_samples_ns: Vec<u64>,
    /// Duration of each measured iteration, in order.
    pub samples_ns: Vec<u64>,
    /// Statistics of `samples_ns`.
    pub summary: Value,
    /// `input_size / median`, in input units per second.
    pub throughput_per_second_at_median: Option<f64>,
    /// What these numbers do and do not show.
    pub limitations: Vec<String>,
}

/// Limitations recorded with every performance result.
pub const LIMITATIONS: &[&str] = &[
    "one run in one process on one machine: intervals describe variability within this run, not across runs, machines or load conditions",
    "the workload runs in its own files under the lab directory; file-system caches, CPU frequency scaling and other processes affect the numbers and are not controlled",
    "durations are wall-clock times of a monotonic clock, including any time the thread was descheduled",
];

/// Executes a workload and assembles the result (no registry access).
///
/// # Errors
/// A [`Failure`]: `infrastructure` if the work directory cannot be
/// prepared, `internal` if an iteration fails or returns a wrong result.
pub fn measure(
    spec: &PerformanceSpec,
    info: &WorkloadInfo,
    work: &std::path::Path,
) -> std::result::Result<PerformanceResult, Failure> {
    let infra = |e: &dyn std::fmt::Display| {
        Failure::new(
            FailureKind::Infrastructure,
            format!("the workload could not be prepared: {e}"),
        )
    };
    if work.exists() {
        std::fs::remove_dir_all(work).map_err(|e| infra(&e))?;
    }
    std::fs::create_dir_all(work).map_err(|e| infra(&e))?;
    let outcome = (|| {
        let n = spec.input_size.unwrap_or(info.input_size.default);
        let mut workload =
            workloads::prepare(info.id, work, n, spec.seed.unwrap_or(1)).map_err(|e| infra(&e))?;
        let mut run = |count: u64, offset: u64| -> std::result::Result<Vec<u64>, Failure> {
            let mut samples = Vec::new();
            for k in 0..count {
                let i = offset + k;
                let bad = |e: String| {
                    Failure::new(
                        FailureKind::Internal,
                        format!("iteration {i} of {} failed: {e}", info.id),
                    )
                };
                workload.before(i).map_err(bad)?;
                let started = Instant::now();
                workload.measure(i).map_err(bad)?;
                let elapsed = started.elapsed();
                workload.check(i).map_err(bad)?;
                samples.push(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX));
            }
            Ok(samples)
        };
        let warmup = spec.warmup_iterations.unwrap_or(WARMUP.default);
        let measured = spec.measured_iterations.unwrap_or(MEASURED.default);
        let warmup_samples = run(warmup, 0)?;
        let samples = run(measured, warmup)?;
        Ok((n, warmup_samples, samples))
    })();
    let _ = std::fs::remove_dir_all(work);
    let (n, warmup_samples_ns, samples_ns) = outcome?;
    #[allow(clippy::cast_precision_loss)]
    let as_f64: Vec<f64> = samples_ns.iter().map(|s| *s as f64).collect();
    let summary = Summary::of(&as_f64)
        .map_err(|e| Failure::new(FailureKind::Internal, format!("invalid measurements: {e}")))?;
    let environment = Environment::current();
    let mut evidence_notes = Vec::new();
    if environment.build_profile == "debug" {
        evidence_notes.push("unoptimized (debug) build".to_owned());
    }
    if (samples_ns.len() as u64) < MIN_MEASUREMENT_SAMPLES {
        evidence_notes.push(format!(
            "{} measured iterations; a measurement needs at least {MIN_MEASUREMENT_SAMPLES}",
            samples_ns.len()
        ));
    }
    #[allow(clippy::cast_precision_loss)]
    let throughput = (summary.median > 0.0).then(|| n as f64 / (summary.median / 1e9));
    Ok(PerformanceResult {
        result_version: RESULT_VERSION,
        workload: json!({
            "id": info.id, "version": info.version, "title": info.title,
            "iteration": info.iteration, "input_unit": info.input_unit,
        }),
        configuration_digest: spec.configuration_digest(),
        configuration: spec.clone(),
        environment,
        evidence_level: if evidence_notes.is_empty() {
            "measurement"
        } else {
            "smoke"
        }
        .to_owned(),
        evidence_notes,
        unit: "nanoseconds per iteration".to_owned(),
        warmup_samples_ns,
        samples_ns,
        summary: serde_json::to_value(&summary)
            .map_err(|e| Failure::new(FailureKind::Internal, e.to_string()))?,
        throughput_per_second_at_median: throughput,
        limitations: LIMITATIONS.iter().map(|s| (*s).to_owned()).collect(),
    })
}

fn record_spec(record: &ExperimentRecord) -> Result<(PerformanceSpec, &'static WorkloadInfo)> {
    let spec = PerformanceSpec::from_json(record.spec.clone())?;
    let (validated, info) = spec.validated()?;
    if validated != spec {
        return Err(LabError::InvalidSpec {
            field: "spec",
            reason: "the stored specification is not in validated form".to_owned(),
        });
    }
    Ok((spec, info))
}

/// The result of a completed performance experiment.
///
/// # Errors
/// [`LabError::InvalidState`] for another kind or status, or
/// [`LabError::CorruptRecord`] if its result cannot be read.
pub fn result_of(record: &ExperimentRecord) -> Result<PerformanceResult> {
    let (true, Status::Completed, Some(result)) = (
        record.kind == PERFORMANCE,
        record.status,
        record.result.as_ref(),
    ) else {
        return Err(LabError::InvalidState {
            id: record.id,
            reason: format!(
                "not a completed performance experiment ({} {})",
                record.kind, record.status
            ),
        });
    };
    serde_json::from_value(result.clone()).map_err(|e| LabError::CorruptRecord {
        record: record.id.to_string(),
        reason: format!("performance result: {e}"),
    })
}

impl Lab {
    /// Validates a performance spec and records a new experiment.
    ///
    /// # Errors
    /// [`LabError::InvalidSpec`] or registry errors.
    pub fn create_performance(&self, spec: &PerformanceSpec) -> Result<ExperimentRecord> {
        let (spec, _) = spec.validated()?;
        self.create_experiment(&NewExperiment {
            kind: PERFORMANCE,
            spec_version: SPEC_VERSION,
            spec: serde_json::to_value(&spec)?,
            stream: None,
            left: None,
            right: None,
            implementations: json!([]),
            build: BuildInfo::current(),
        })
    }

    fn performance_work(&self, name: &str) -> PathBuf {
        self.dir().join("performance-work").join(name)
    }

    /// Runs a `created` performance experiment in this process.
    ///
    /// # Errors
    /// [`LabError::InvalidState`] for another kind, or registry errors.
    pub fn run_performance(&self, id: ExperimentId) -> Result<ExperimentRecord> {
        let record = self.experiment(id)?;
        if record.kind != PERFORMANCE {
            return Err(LabError::InvalidState {
                id,
                reason: "not a performance experiment".to_owned(),
            });
        }
        self.run_locked(id, |record| {
            let (spec, info) = record_spec(record).map_err(|e| {
                RunFailure::from(Failure::new(FailureKind::Internal, e.to_string()))
            })?;
            let result = measure(&spec, info, &self.performance_work(&id.to_string()))?;
            let value = serde_json::to_value(&result).map_err(|e| {
                RunFailure::from(Failure::new(FailureKind::Internal, e.to_string()))
            })?;
            Ok(Completion {
                input: None,
                outcome: result.evidence_level.clone(),
                result_digest: value_digest(RESULT_TAG, &value),
                result: value,
                reproducibility: "unverified".to_owned(),
                artifacts: Vec::new(),
                elapsed_micros: 0,
            })
        })
    }

    /// Validates, records and runs a performance experiment.
    ///
    /// # Errors
    /// As [`create_performance`](Self::create_performance) and
    /// [`run_performance`](Self::run_performance).
    pub fn measure_performance(&self, spec: &PerformanceSpec) -> Result<ExperimentRecord> {
        let record = self.create_performance(spec)?;
        self.run_performance(record.id)
    }

    /// The newest completed performance experiment created before
    /// `candidate` with the same configuration digest, in an environment
    /// [`compare::compatibility`] accepts, or `None` if there is none.
    ///
    /// # Errors
    /// Registry errors, or `candidate` is not a completed performance experiment.
    pub fn find_baseline(&self, candidate: ExperimentId) -> Result<Option<ExperimentRecord>> {
        let current = result_of(&self.experiment(candidate)?)?;
        let mut before = Some(candidate);
        loop {
            let page = self.registry().list_kind(100, before, Some(PERFORMANCE))?;
            let Some(last) = page.last() else {
                return Ok(None);
            };
            before = Some(last.id);
            for row in page {
                if row.status != Status::Completed {
                    continue;
                }
                let record = self.experiment(row.id)?;
                let Ok(result) = result_of(&record) else {
                    continue;
                };
                if result.configuration_digest == current.configuration_digest
                    && compare::compatibility(&result, &current)
                        .incompatible
                        .is_empty()
                {
                    return Ok(Some(record));
                }
            }
        }
    }

    /// Compares completed performance experiments: `baseline` (or, if
    /// `None`, the one [`find_baseline`](Self::find_baseline) finds for the
    /// first candidate) against each candidate. Nothing is stored.
    ///
    /// # Errors
    /// [`LabError::ExperimentNotFound`], or [`LabError::InvalidState`] if an
    /// experiment is not a completed performance experiment.
    pub fn compare_performance(
        &self,
        baseline: Option<ExperimentId>,
        candidates: &[ExperimentId],
        policy: &RegressionPolicy,
    ) -> Result<Value> {
        let first = *candidates.first().ok_or_else(|| LabError::InvalidSpec {
            field: "candidates",
            reason: "at least one candidate is required".to_owned(),
        })?;
        let (baseline, selection) = match baseline {
            Some(id) => (Some(self.experiment(id)?), "given"),
            None => (
                self.find_baseline(first)?,
                "newest_compatible_before_candidate",
            ),
        };
        let Some(baseline) = baseline else {
            return Ok(json!({
                "status": "no_baseline",
                "reason": "no earlier completed performance experiment has the same configuration and a compatible environment",
                "baseline": null,
                "comparisons": [],
                "policy": policy,
            }));
        };
        let base = result_of(&baseline)?;
        let mut comparisons = Vec::new();
        for id in candidates {
            let record = self.experiment(*id)?;
            let candidate = result_of(&record)?;
            comparisons.push(json!({
                "candidate": provenance(&record, &candidate),
                "comparison": compare::compare(&base, &candidate, policy),
            }));
        }
        Ok(json!({
            "status": "compared",
            "baseline_selection": selection,
            "baseline": provenance(&baseline, &base),
            "comparisons": comparisons,
            "policy": policy,
        }))
    }

    /// Measures a completed performance experiment again (a new, linked
    /// experiment with the identical spec) and compares the two. The
    /// original is never modified; the attempt is appended to it as a
    /// verification whose detail names the new experiment.
    ///
    /// | Comparison verdict | Status |
    /// |---|---|
    /// | `within_threshold` | `reproduced` |
    /// | `regression`, `improvement` (statistically supported) | `differs` |
    /// | `suspected_*`, `insufficient_samples` | `inconclusive` |
    /// | `incompatible` | `configuration_mismatch` |
    ///
    /// # Errors
    /// [`LabError::InvalidState`] unless a completed performance experiment.
    pub fn reproduce_performance(
        &self,
        id: ExperimentId,
        policy: &RegressionPolicy,
    ) -> Result<(VerificationRecord, ExperimentRecord)> {
        match self.prepare_performance_reproduction(id)? {
            Ok(attempt) => self.finish_performance_reproduction(id, attempt.id, policy),
            Err(verification) => Ok((verification, self.experiment(id)?)),
        }
    }

    /// First half of [`reproduce_performance`](Self::reproduce_performance):
    /// records the linked attempt (status `created`) without running it.
    /// If the original's spec can no longer be read, appends a
    /// `configuration_mismatch` verification instead and returns it as `Err`.
    ///
    /// # Errors
    /// [`LabError::InvalidState`] unless a completed performance experiment.
    pub fn prepare_performance_reproduction(
        &self,
        id: ExperimentId,
    ) -> Result<std::result::Result<ExperimentRecord, VerificationRecord>> {
        let original = self.experiment(id)?;
        result_of(&original)?;
        match record_spec(&original) {
            Ok((spec, _)) => Ok(Ok(self.create_performance(&spec)?)),
            Err(e) => Ok(Err(self.registry().add_verification(
                id,
                VerificationKind::Reproduction,
                ReproductionStatus::ConfigurationMismatch.as_str(),
                &json!({ "reason": e.to_string() }),
            )?)),
        }
    }

    /// Second half: runs the attempt, compares it with the original and
    /// appends the verification to the original.
    ///
    /// # Errors
    /// [`LabError::InvalidState`] if either is not a performance experiment.
    pub fn finish_performance_reproduction(
        &self,
        id: ExperimentId,
        attempt: ExperimentId,
        policy: &RegressionPolicy,
    ) -> Result<(VerificationRecord, ExperimentRecord)> {
        let recorded = result_of(&self.experiment(id)?)?;
        let attempt = self.run_performance(attempt)?;
        let (status, detail) = match result_of(&attempt) {
            Err(_) => (
                ReproductionStatus::NotReproducible,
                json!({
                    "reason": "the new measurement did not complete",
                    "attempt_experiment_id": attempt.id,
                    "attempt_failure": attempt.failure,
                }),
            ),
            Ok(new) => {
                let comparison = compare::compare(&recorded, &new, policy);
                (
                    reproduction_status(comparison.verdict),
                    json!({
                        "compared": "a new measurement of the same specification",
                        "attempt_experiment_id": attempt.id,
                        "comparison": comparison,
                    }),
                )
            }
        };
        let v = self.registry().add_verification(
            id,
            VerificationKind::Reproduction,
            status.as_str(),
            &detail,
        )?;
        Ok((v, attempt))
    }
}

/// How a comparison verdict classifies a reproduction attempt.
#[must_use]
pub fn reproduction_status(verdict: Verdict) -> ReproductionStatus {
    match verdict {
        Verdict::WithinThreshold => ReproductionStatus::Reproduced,
        Verdict::Regression | Verdict::Improvement => ReproductionStatus::Differs,
        Verdict::Incompatible => ReproductionStatus::ConfigurationMismatch,
        Verdict::SuspectedRegression
        | Verdict::SuspectedImprovement
        | Verdict::InsufficientSamples => ReproductionStatus::Inconclusive,
    }
}

/// Identity of a compared experiment.
fn provenance(record: &ExperimentRecord, result: &PerformanceResult) -> Value {
    json!({
        "experiment_id": record.id,
        "created_at": record.created_at,
        "configuration_digest": result.configuration_digest,
        "evidence_level": result.evidence_level,
        "samples": result.samples_ns.len(),
        "build": record.build,
        "environment": result.environment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_are_validated_and_filled_in() {
        let spec = PerformanceSpec::from_json(json!({ "workload": "event-append" })).unwrap();
        let (v, info) = spec.validated().unwrap();
        assert_eq!(info.id, "event-append");
        assert_eq!(v.workload_version, Some(1));
        assert_eq!(v.input_size, Some(200));
        assert_eq!(v.warmup_iterations, Some(3));
        assert_eq!(v.measured_iterations, Some(20));
        assert_eq!(v.seed, Some(1));
        assert_eq!(v.validated().unwrap().0, v, "validation is idempotent");
        for bad in [
            json!({ "workload": "nope" }),
            json!({ "workload": "event-append", "input_size": 0 }),
            json!({ "workload": "event-append", "input_size": 10_001 }),
            json!({ "workload": "event-append", "measured_iterations": 501 }),
            json!({ "workload": "event-append", "warmup_iterations": 101 }),
            json!({ "workload": "event-append", "workload_version": 2 }),
        ] {
            assert!(
                PerformanceSpec::from_json(bad.clone())
                    .unwrap()
                    .validated()
                    .is_err(),
                "{bad}"
            );
        }
        assert!(
            PerformanceSpec::from_json(json!({ "workload": "event-append", "path": "/etc" }))
                .is_err()
        );
    }

    #[test]
    fn iteration_counts_do_not_change_the_configuration_digest() {
        let a = PerformanceSpec::from_json(
            json!({ "workload": "event-append", "measured_iterations": 5 }),
        )
        .unwrap()
        .validated()
        .unwrap()
        .0;
        let b = PerformanceSpec::from_json(json!({ "workload": "event-append", "measured_iterations": 50, "warmup_iterations": 0 }))
            .unwrap()
            .validated()
            .unwrap()
            .0;
        let c = PerformanceSpec::from_json(json!({ "workload": "event-append", "seed": 2 }))
            .unwrap()
            .validated()
            .unwrap()
            .0;
        assert_eq!(a.configuration_digest(), b.configuration_digest());
        assert_ne!(a.configuration_digest(), c.configuration_digest());
    }
}
