//! Comparing two performance results.
//!
//! # Compatibility
//!
//! Numbers are compared only if both results measure the same thing in a
//! comparable environment. These differences make a comparison
//! `incompatible` (nothing is compared):
//!
//! - workload id, workload version, input size or seed (the configuration
//!   digest) differ;
//! - result layout versions differ;
//! - operating system, CPU architecture or build profile (debug/release)
//!   differ.
//!
//! These are reported as warnings but do not prevent a comparison: different
//! iteration counts, a different number of available CPUs, a different
//! executable (comparing builds is the point), or either result being only
//! `smoke` evidence.
//!
//! # Verdicts
//!
//! The change is measured on medians: `relative_change = (candidate −
//! baseline) / baseline`. Positive means slower. With the policy's
//! `threshold` (default 10 %) and significance level `alpha` (default 0.01):
//!
//! | Verdict | Condition |
//! |---|---|
//! | `incompatible` | see above |
//! | `insufficient_samples` | either side has fewer than [`MIN_COMPARE_SAMPLES`] samples |
//! | `within_threshold` | `|relative_change| < threshold`, whatever the test says |
//! | `suspected_regression` / `suspected_improvement` | above the threshold, but the test is not significant at `alpha`, or not performed (fewer than [`MIN_TEST_SAMPLES`] samples on a side) |
//! | `regression` / `improvement` | above the threshold **and** significant at `alpha` (two-sided Mann–Whitney U) |
//!
//! Only `regression` and `improvement` are statistically supported
//! findings, and only about the two runs compared: a single run per side
//! does not account for variation between runs. CI never fails on these
//! verdicts.

use serde::{Deserialize, Serialize};

use super::PerformanceResult;
use super::stats::{self, Summary};

/// Fewest samples per side for any comparison.
pub const MIN_COMPARE_SAMPLES: usize = 3;
/// Fewest samples per side for the significance test.
pub const MIN_TEST_SAMPLES: usize = 10;

/// Thresholds of a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionPolicy {
    /// Smallest relative change of the median that counts, in percent.
    pub threshold_percent: f64,
    /// Significance level of the two-sided test.
    pub alpha: f64,
}

impl Default for RegressionPolicy {
    fn default() -> Self {
        Self {
            threshold_percent: 10.0,
            alpha: 0.01,
        }
    }
}

impl RegressionPolicy {
    /// Checks the bounds: threshold in `[0.1, 100]` percent, alpha in
    /// `[0.0001, 0.1]`.
    ///
    /// # Errors
    /// A description of the bad value.
    pub fn validated(self) -> Result<Self, String> {
        if !(0.1..=100.0).contains(&self.threshold_percent) {
            return Err("threshold_percent must be between 0.1 and 100".to_owned());
        }
        if !(0.0001..=0.1).contains(&self.alpha) {
            return Err("alpha must be between 0.0001 and 0.1".to_owned());
        }
        Ok(self)
    }
}

/// Outcome of a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The results do not measure the same thing comparably.
    Incompatible,
    /// Too few samples to compare.
    InsufficientSamples,
    /// The change of the median is below the threshold.
    WithinThreshold,
    /// Slower by more than the threshold, without statistical support.
    SuspectedRegression,
    /// Faster by more than the threshold, without statistical support.
    SuspectedImprovement,
    /// Slower by more than the threshold, and significant.
    Regression,
    /// Faster by more than the threshold, and significant.
    Improvement,
}

/// What differs between two results.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Compatibility {
    /// Differences that prevent a comparison.
    pub incompatible: Vec<String>,
    /// Differences reported alongside a comparison.
    pub warnings: Vec<String>,
}

/// Compatibility of `baseline` and `candidate`.
#[must_use]
pub fn compatibility(baseline: &PerformanceResult, candidate: &PerformanceResult) -> Compatibility {
    let mut c = Compatibility::default();
    let (b, k) = (&baseline.configuration, &candidate.configuration);
    let mut differ = |name: &str, x: String, y: String, fatal: bool| {
        if x != y {
            let line = format!("{name}: {x} vs {y}");
            if fatal {
                c.incompatible.push(line);
            } else {
                c.warnings.push(line);
            }
        }
    };
    differ("workload", b.workload.clone(), k.workload.clone(), true);
    differ(
        "workload_version",
        format!("{:?}", b.workload_version),
        format!("{:?}", k.workload_version),
        true,
    );
    differ(
        "input_size",
        format!("{:?}", b.input_size),
        format!("{:?}", k.input_size),
        true,
    );
    differ(
        "seed",
        format!("{:?}", b.seed),
        format!("{:?}", k.seed),
        true,
    );
    differ(
        "result_version",
        baseline.result_version.to_string(),
        candidate.result_version.to_string(),
        true,
    );
    let (be, ke) = (&baseline.environment, &candidate.environment);
    differ("os", be.os.clone(), ke.os.clone(), true);
    differ("arch", be.arch.clone(), ke.arch.clone(), true);
    differ(
        "build_profile",
        be.build_profile.clone(),
        ke.build_profile.clone(),
        true,
    );
    differ(
        "available_parallelism",
        format!("{:?}", be.available_parallelism),
        format!("{:?}", ke.available_parallelism),
        false,
    );
    differ(
        "warmup_iterations",
        format!("{:?}", b.warmup_iterations),
        format!("{:?}", k.warmup_iterations),
        false,
    );
    differ(
        "measured_iterations",
        format!("{:?}", b.measured_iterations),
        format!("{:?}", k.measured_iterations),
        false,
    );
    if be.build.executable_digest != ke.build.executable_digest {
        c.warnings
            .push("executable: different builds (expected when comparing code changes)".to_owned());
    }
    for (side, r) in [("baseline", baseline), ("candidate", candidate)] {
        if r.evidence_level != "measurement" {
            c.warnings.push(format!(
                "{side} is {} evidence: {}",
                r.evidence_level,
                r.evidence_notes.join("; ")
            ));
        }
    }
    c
}

/// A comparison of two results.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Comparison {
    /// Verdict.
    pub verdict: Verdict,
    /// What differs.
    pub compatibility: Compatibility,
    /// Median of the baseline, ns.
    pub baseline_median_ns: Option<f64>,
    /// Median of the candidate, ns.
    pub candidate_median_ns: Option<f64>,
    /// `candidate − baseline` medians, ns.
    pub absolute_change_ns: Option<f64>,
    /// `(candidate − baseline) / baseline` medians (0.1 = 10 % slower).
    pub relative_change: Option<f64>,
    /// Distribution-free 95 % intervals of both medians, when available.
    pub median_intervals: Option<[Option<stats::Interval>; 2]>,
    /// Whether those intervals overlap (informational, not the test).
    pub median_intervals_overlap: Option<bool>,
    /// The significance test, when performed.
    pub test: Option<stats::MannWhitney>,
    /// The policy applied.
    pub policy: RegressionPolicy,
    /// Why the verdict is what it is.
    pub explanation: String,
}

#[allow(clippy::cast_precision_loss)]
fn samples(r: &PerformanceResult) -> Vec<f64> {
    r.samples_ns.iter().map(|s| *s as f64).collect()
}

/// Compares `candidate` against `baseline` under `policy`.
#[must_use]
pub fn compare(
    baseline: &PerformanceResult,
    candidate: &PerformanceResult,
    policy: &RegressionPolicy,
) -> Comparison {
    let compatibility = compatibility(baseline, candidate);
    let mut out = Comparison {
        verdict: Verdict::Incompatible,
        compatibility,
        baseline_median_ns: None,
        candidate_median_ns: None,
        absolute_change_ns: None,
        relative_change: None,
        median_intervals: None,
        median_intervals_overlap: None,
        test: None,
        policy: *policy,
        explanation: String::new(),
    };
    if !out.compatibility.incompatible.is_empty() {
        out.explanation = format!(
            "not compared: {}",
            out.compatibility.incompatible.join("; ")
        );
        return out;
    }
    let (b, k) = (samples(baseline), samples(candidate));
    let (Ok(bs), Ok(ks)) = (Summary::of(&b), Summary::of(&k)) else {
        out.verdict = Verdict::InsufficientSamples;
        out.explanation = "a side has no valid samples".to_owned();
        return out;
    };
    out.baseline_median_ns = Some(bs.median);
    out.candidate_median_ns = Some(ks.median);
    out.median_intervals = Some([bs.median_ci, ks.median_ci]);
    out.median_intervals_overlap = match (bs.median_ci, ks.median_ci) {
        (Some(x), Some(y)) => Some(x.lower <= y.upper && y.lower <= x.upper),
        _ => None,
    };
    if b.len() < MIN_COMPARE_SAMPLES || k.len() < MIN_COMPARE_SAMPLES {
        out.verdict = Verdict::InsufficientSamples;
        out.explanation = format!(
            "{} and {} samples; at least {MIN_COMPARE_SAMPLES} per side are needed",
            b.len(),
            k.len()
        );
        return out;
    }
    let absolute = ks.median - bs.median;
    out.absolute_change_ns = Some(absolute);
    if bs.median <= 0.0 {
        out.verdict = Verdict::InsufficientSamples;
        out.explanation = "the baseline median is zero; a relative change is undefined".to_owned();
        return out;
    }
    let relative = absolute / bs.median;
    out.relative_change = Some(relative);
    let threshold = policy.threshold_percent / 100.0;
    out.test = (b.len() >= MIN_TEST_SAMPLES && k.len() >= MIN_TEST_SAMPLES)
        .then(|| stats::mann_whitney(&k, &b))
        .flatten();
    let pct = relative * 100.0;
    if relative.abs() < threshold {
        out.verdict = Verdict::WithinThreshold;
        out.explanation = format!(
            "the median changed by {pct:+.2} %, less than the {} % threshold",
            policy.threshold_percent
        );
        return out;
    }
    let slower = relative > 0.0;
    let significant = out.test.is_some_and(|t| t.p_value < policy.alpha);
    out.verdict = match (slower, significant) {
        (true, true) => Verdict::Regression,
        (false, true) => Verdict::Improvement,
        (true, false) => Verdict::SuspectedRegression,
        (false, false) => Verdict::SuspectedImprovement,
    };
    out.explanation = match out.test {
        None => format!(
            "the median changed by {pct:+.2} % (threshold {} %), but the significance test needs at least {MIN_TEST_SAMPLES} samples per side",
            policy.threshold_percent
        ),
        Some(t) if significant => format!(
            "the median changed by {pct:+.2} % (threshold {} %) and the Mann–Whitney test is significant (p = {:.2e} < {})",
            policy.threshold_percent, t.p_value, policy.alpha
        ),
        Some(t) => format!(
            "the median changed by {pct:+.2} % (threshold {} %), but the Mann–Whitney test is not significant (p = {:.3} ≥ {})",
            policy.threshold_percent, t.p_value, policy.alpha
        ),
    };
    out
}

#[cfg(test)]
mod tests {
    use super::super::{Environment, PerformanceSpec};
    use super::*;

    fn result(samples: &[u64]) -> PerformanceResult {
        let spec = PerformanceSpec::from_json(serde_json::json!({ "workload": "event-append" }))
            .unwrap()
            .validated()
            .unwrap()
            .0;
        PerformanceResult {
            result_version: 1,
            workload: serde_json::Value::Null,
            configuration_digest: spec.configuration_digest(),
            configuration: spec,
            environment: Environment::current(),
            evidence_level: "measurement".into(),
            evidence_notes: vec![],
            unit: "ns".into(),
            warmup_samples_ns: vec![],
            samples_ns: samples.to_vec(),
            summary: serde_json::Value::Null,
            throughput_per_second_at_median: None,
            limitations: vec![],
        }
    }

    /// 20 deterministic samples around `center` with ±2 % spread.
    fn around(center: u64) -> Vec<u64> {
        (0..20u64)
            .map(|i| center - center / 50 + (i * center / 500))
            .collect()
    }

    #[test]
    fn identical_results_are_within_threshold() {
        let c = compare(
            &result(&around(1_000_000)),
            &result(&around(1_000_000)),
            &RegressionPolicy::default(),
        );
        assert_eq!(c.verdict, Verdict::WithinThreshold);
        assert_eq!(c.relative_change, Some(0.0));
        assert!(c.test.unwrap().p_value > 0.9);
    }

    #[test]
    fn a_clear_slowdown_is_a_supported_regression() {
        // Every candidate sample is 50 % slower: fully separated samples.
        let c = compare(
            &result(&around(1_000_000)),
            &result(&around(1_500_000)),
            &RegressionPolicy::default(),
        );
        assert_eq!(c.verdict, Verdict::Regression);
        assert!((c.relative_change.unwrap() - 0.5).abs() < 1e-9);
        assert!(c.test.unwrap().p_value < 1e-6);
        let c = compare(
            &result(&around(1_500_000)),
            &result(&around(1_000_000)),
            &RegressionPolicy::default(),
        );
        assert_eq!(c.verdict, Verdict::Improvement);
    }

    #[test]
    fn a_tiny_difference_is_not_a_regression_even_if_significant() {
        // 3 % slower, fully separated (so the test alone would be significant).
        let base: Vec<u64> = (0..20).map(|i| 1_000_000 + i).collect();
        let cand: Vec<u64> = (0..20).map(|i| 1_030_000 + i).collect();
        let c = compare(&result(&base), &result(&cand), &RegressionPolicy::default());
        assert!(c.test.unwrap().p_value < 0.01);
        assert_eq!(c.verdict, Verdict::WithinThreshold);
    }

    #[test]
    fn a_large_noisy_difference_is_only_suspected() {
        // Medians 20 % apart, but the samples overlap heavily.
        let base = [100, 300, 100, 300, 100, 300, 100, 300, 100, 300, 200, 200];
        let cand = [100, 300, 100, 300, 100, 300, 100, 300, 100, 300, 240, 240];
        let c = compare(&result(&base), &result(&cand), &RegressionPolicy::default());
        assert!((c.relative_change.unwrap() - 0.2).abs() < 1e-9);
        assert!(c.test.unwrap().p_value >= 0.01);
        assert_eq!(c.verdict, Verdict::SuspectedRegression);
    }

    #[test]
    fn few_samples_are_never_significant() {
        let c = compare(
            &result(&[100, 101, 102, 103, 104]),
            &result(&[300, 301, 302, 303, 304]),
            &RegressionPolicy::default(),
        );
        assert_eq!(
            c.verdict,
            Verdict::SuspectedRegression,
            "no test below 10 samples"
        );
        assert!(c.test.is_none());
        let c = compare(
            &result(&[100, 101]),
            &result(&[300, 301]),
            &RegressionPolicy::default(),
        );
        assert_eq!(c.verdict, Verdict::InsufficientSamples);
    }

    #[test]
    fn different_configurations_or_environments_are_incompatible() {
        let base = result(&around(1_000));
        let mut other = result(&around(1_000));
        other.configuration.input_size = Some(201);
        let c = compare(&base, &other, &RegressionPolicy::default());
        assert_eq!(c.verdict, Verdict::Incompatible);
        assert!(c.relative_change.is_none(), "no numbers are compared");
        let mut other = result(&around(1_000));
        other.environment.build_profile = "release-elsewhere".into();
        assert_eq!(
            compare(&base, &other, &RegressionPolicy::default()).verdict,
            Verdict::Incompatible
        );
        let mut other = result(&around(1_000));
        other.environment.os = "plan9".into();
        assert_eq!(
            compare(&base, &other, &RegressionPolicy::default()).verdict,
            Verdict::Incompatible
        );
        // Iteration counts and CPU count only warn.
        let mut other = result(&around(1_000));
        other.configuration.measured_iterations = Some(40);
        other.environment.available_parallelism = Some(999);
        let c = compare(&base, &other, &RegressionPolicy::default());
        assert_eq!(c.verdict, Verdict::WithinThreshold);
        assert_eq!(c.compatibility.warnings.len(), 2);
    }

    #[test]
    fn policies_are_bounded() {
        assert!(RegressionPolicy::default().validated().is_ok());
        for (t, a) in [(0.0, 0.01), (101.0, 0.01), (10.0, 0.5), (10.0, 0.0)] {
            assert!(
                RegressionPolicy {
                    threshold_percent: t,
                    alpha: a
                }
                .validated()
                .is_err()
            );
        }
        // A 3 % change is a supported regression under a 1 % threshold.
        let base: Vec<u64> = (0..20).map(|i| 1_000_000 + i).collect();
        let cand: Vec<u64> = (0..20).map(|i| 1_030_000 + i).collect();
        let strict = RegressionPolicy {
            threshold_percent: 1.0,
            alpha: 0.01,
        };
        assert_eq!(
            compare(&result(&base), &result(&cand), &strict).verdict,
            Verdict::Regression
        );
    }
}
