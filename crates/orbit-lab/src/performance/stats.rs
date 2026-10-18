//! Descriptive statistics and the two tests the performance lab uses.
//!
//! Everything here is a pure function of the samples, so it is tested with
//! fixed synthetic samples whose expected answers were computed
//! independently (see `tests` below and docs/PERFORMANCE.md).
//!
//! Assumptions, stated where they are used:
//!
//! - Samples are independent measurements of one quantity. Timings from one
//!   process are not strictly independent (caches, frequency scaling, other
//!   load), so every interval here describes *within-run* variability only.
//! - The confidence interval of the mean uses Student's t and is only
//!   reported from [`MIN_MEAN_CI_SAMPLES`] samples on, because timing
//!   distributions are skewed and the t interval relies on the sample mean
//!   being roughly normal.
//! - The confidence interval of the median is distribution-free (order
//!   statistics and the binomial distribution) and reports its exact
//!   coverage, which is at least the nominal level.
//! - Percentiles use linear interpolation between order statistics (the
//!   "type 7" definition, as `numpy.percentile` and Python's
//!   `statistics.quantiles(method="inclusive")`), and a percentile `p` is
//!   only reported with at least `1 / (1 - p)` samples, so p99 needs 100.

use serde::Serialize;

/// Nominal level of every confidence interval.
pub const CONFIDENCE_LEVEL: f64 = 0.95;
/// Fewest samples for a confidence interval of the mean.
pub const MIN_MEAN_CI_SAMPLES: usize = 10;
/// Percentiles reported when the sample count supports them.
pub const PERCENTILES: [f64; 3] = [0.90, 0.95, 0.99];

/// Why samples cannot be summarized.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StatsError {
    /// No samples.
    #[error("no samples")]
    Empty,
    /// A sample is NaN, infinite or negative.
    #[error("sample {0} is not a finite, non-negative number")]
    Invalid(usize),
}

/// A confidence interval.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Interval {
    /// Lower bound.
    pub lower: f64,
    /// Upper bound.
    pub upper: f64,
    /// Nominal level (0.95).
    pub level: f64,
    /// Actual coverage when it differs from the nominal level (order
    /// statistics give discrete coverages), otherwise the nominal level.
    pub coverage: f64,
    /// How it was computed.
    pub method: &'static str,
}

/// One reported percentile.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Percentile {
    /// Probability, for example 0.95.
    pub p: f64,
    /// Value.
    pub value: f64,
}

/// Descriptive statistics of a sample.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    /// Number of samples.
    pub count: usize,
    /// Arithmetic mean.
    pub mean: f64,
    /// Median (mean of the two middle values for an even count).
    pub median: f64,
    /// Sample standard deviation (divisor `n − 1`); absent below 2 samples.
    pub std_dev: Option<f64>,
    /// Median absolute deviation from the median (unscaled), a variability
    /// measure that is not dominated by a few slow outliers.
    pub mad: f64,
    /// `std_dev / mean`; absent without a standard deviation or with a zero mean.
    pub coefficient_of_variation: Option<f64>,
    /// Smallest sample.
    pub min: f64,
    /// Largest sample.
    pub max: f64,
    /// The percentiles of [`PERCENTILES`] that the count supports.
    pub percentiles: Vec<Percentile>,
    /// 95 % interval of the mean (Student t), from [`MIN_MEAN_CI_SAMPLES`] samples.
    pub mean_ci: Option<Interval>,
    /// Distribution-free interval of the median, from 6 samples.
    pub median_ci: Option<Interval>,
    /// What was not computed and why.
    pub notes: Vec<String>,
}

/// Linear-interpolation quantile ("type 7") of sorted samples.
///
/// # Panics
/// Never for a non-empty slice and `p` in `[0, 1]`.
#[must_use]
pub fn quantile(sorted: &[f64], p: f64) -> f64 {
    let h = (sorted.len() - 1) as f64 * p;
    let lo = h.floor();
    let i = lo as usize;
    match sorted.get(i + 1) {
        Some(next) => sorted[i] + (h - lo) * (next - sorted[i]),
        None => sorted[i],
    }
}

fn median_of_sorted(sorted: &[f64]) -> f64 {
    quantile(sorted, 0.5)
}

impl Summary {
    /// Summarizes `samples`.
    ///
    /// # Errors
    /// [`StatsError`] for no samples or an invalid sample.
    pub fn of(samples: &[f64]) -> Result<Self, StatsError> {
        if samples.is_empty() {
            return Err(StatsError::Empty);
        }
        if let Some(i) = samples.iter().position(|x| !x.is_finite() || *x < 0.0) {
            return Err(StatsError::Invalid(i));
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let n = sorted.len();
        let nf = n as f64;
        let mean = sorted.iter().sum::<f64>() / nf;
        let median = median_of_sorted(&sorted);
        let mut notes = Vec::new();
        let std_dev = if n >= 2 {
            let ss: f64 = sorted.iter().map(|x| (x - mean).powi(2)).sum();
            Some((ss / (nf - 1.0)).sqrt())
        } else {
            notes.push("standard deviation needs at least 2 samples".to_owned());
            None
        };
        let mut deviations: Vec<f64> = sorted.iter().map(|x| (x - median).abs()).collect();
        deviations.sort_by(f64::total_cmp);
        let mad = median_of_sorted(&deviations);
        let percentiles = PERCENTILES
            .iter()
            .filter(|p| nf >= 1.0 / (1.0 - **p) - 1e-9)
            .map(|p| Percentile {
                p: *p,
                value: quantile(&sorted, *p),
            })
            .collect::<Vec<_>>();
        if percentiles.len() < PERCENTILES.len() {
            notes.push(format!(
                "percentile p is reported only with at least 1/(1-p) samples ({n} available)"
            ));
        }
        let mean_ci = match std_dev {
            Some(sd) if n >= MIN_MEAN_CI_SAMPLES => {
                let t = t_quantile(1.0 - (1.0 - CONFIDENCE_LEVEL) / 2.0, nf - 1.0);
                let half = t * sd / nf.sqrt();
                Some(Interval {
                    lower: mean - half,
                    upper: mean + half,
                    level: CONFIDENCE_LEVEL,
                    coverage: CONFIDENCE_LEVEL,
                    method: "student_t",
                })
            }
            _ => {
                notes.push(format!(
                    "no interval of the mean below {MIN_MEAN_CI_SAMPLES} samples"
                ));
                None
            }
        };
        let median_ci = median_interval(&sorted);
        if median_ci.is_none() {
            notes.push("no interval of the median below 6 samples".to_owned());
        }
        Ok(Self {
            count: n,
            mean,
            median,
            coefficient_of_variation: std_dev.filter(|_| mean > 0.0).map(|sd| sd / mean),
            std_dev,
            mad,
            min: sorted[0],
            max: sorted[n - 1],
            percentiles,
            mean_ci,
            median_ci,
            notes,
        })
    }
}

/// `P(B ≤ k)` for `B ~ Binomial(n, 1/2)`.
fn binomial_half_cdf(n: usize, k: usize) -> f64 {
    let nf = n as f64;
    let ln_total = nf * std::f64::consts::LN_2;
    let ln_choose =
        |i: usize| ln_gamma(nf + 1.0) - ln_gamma(i as f64 + 1.0) - ln_gamma(nf - i as f64 + 1.0);
    (0..=k.min(n))
        .map(|i| (ln_choose(i) - ln_total).exp())
        .sum::<f64>()
        .min(1.0)
}

/// The order-statistic interval `[x(k), x(n−k+1)]` with the largest `k`
/// whose coverage `1 − 2·P(B ≤ k−1)` is at least the nominal level.
fn median_interval(sorted: &[f64]) -> Option<Interval> {
    let n = sorted.len();
    let alpha_half = (1.0 - CONFIDENCE_LEVEL) / 2.0;
    let mut best = None;
    for k in 1..=n.div_ceil(2) {
        let tail = binomial_half_cdf(n, k - 1);
        if tail <= alpha_half + 1e-12 {
            best = Some((k, 1.0 - 2.0 * tail));
        } else {
            break;
        }
    }
    let (k, coverage) = best?;
    Some(Interval {
        lower: sorted[k - 1],
        upper: sorted[n - k],
        level: CONFIDENCE_LEVEL,
        coverage,
        method: "order_statistics",
    })
}

/// `ln Γ(x)` for `x > 0` (Lanczos approximation, g = 7, n = 9; relative
/// error below 1e-13 over the range used here).
#[must_use]
pub fn ln_gamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Reflection: Γ(x)Γ(1−x) = π / sin(πx).
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = C[0];
    let t = x + G + 0.5;
    for (i, c) in C.iter().enumerate().skip(1) {
        a += c / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// Continued fraction of the incomplete beta function (modified Lentz).
fn beta_cf(a: f64, b: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=300 {
        let m = f64::from(m);
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 1e-15 {
            break;
        }
    }
    h
}

/// Regularized incomplete beta function `I_x(a, b)`.
#[must_use]
pub fn incomplete_beta(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let ln_front = ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln();
    if x < (a + 1.0) / (a + b + 2.0) {
        ln_front.exp() * beta_cf(a, b, x) / a
    } else {
        1.0 - ln_front.exp() * beta_cf(b, a, 1.0 - x) / b
    }
}

/// CDF of Student's t distribution with `df` degrees of freedom.
#[must_use]
pub fn t_cdf(t: f64, df: f64) -> f64 {
    let tail = 0.5 * incomplete_beta(df / 2.0, 0.5, df / (df + t * t));
    if t >= 0.0 { 1.0 - tail } else { tail }
}

/// Quantile of Student's t (bisection on [`t_cdf`], to 1e-12).
#[must_use]
pub fn t_quantile(p: f64, df: f64) -> f64 {
    if p == 0.5 {
        return 0.0;
    }
    if p < 0.5 {
        return -t_quantile(1.0 - p, df);
    }
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    while t_cdf(hi, df) < p && hi < 1e12 {
        hi *= 2.0;
    }
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if t_cdf(mid, df) < p {
            lo = mid;
        } else {
            hi = mid;
        }
        if hi - lo < 1e-12 {
            break;
        }
    }
    0.5 * (lo + hi)
}

/// Standard normal CDF, from the complementary error function.
#[must_use]
pub fn normal_cdf(z: f64) -> f64 {
    0.5 * erfc(-z / std::f64::consts::SQRT_2)
}

/// Complementary error function (Chebyshev fit, fractional error below
/// 1.2e-7 everywhere; Numerical Recipes `erfcc`).
#[must_use]
pub fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let poly = -z * z - 1.265_512_23
        + t * (1.000_023_68
            + t * (0.374_091_96
                + t * (0.096_784_18
                    + t * (-0.186_288_06
                        + t * (0.278_868_07
                            + t * (-1.135_203_98
                                + t * (1.488_515_87 + t * (-0.822_152_23 + t * 0.170_872_77))))))));
    let r = t * poly.exp();
    if x >= 0.0 { r } else { 2.0 - r }
}

/// Result of a two-sided Mann–Whitney U test.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MannWhitney {
    /// U statistic of the first sample.
    pub u: f64,
    /// Normal approximation with tie and continuity corrections.
    pub z: f64,
    /// Two-sided p-value of that approximation.
    pub p_value: f64,
    /// Method label.
    pub method: &'static str,
}

/// Two-sided Mann–Whitney U test of `a` against `b` (normal approximation
/// with tie correction and continuity correction). It tests whether one
/// sample tends to be larger than the other, without assuming normality.
/// Returns `None` if either sample is empty or every value is tied.
#[must_use]
pub fn mann_whitney(a: &[f64], b: &[f64]) -> Option<MannWhitney> {
    let (n1, n2) = (a.len(), b.len());
    if n1 == 0 || n2 == 0 {
        return None;
    }
    let mut all: Vec<(f64, bool)> = a
        .iter()
        .map(|x| (*x, true))
        .chain(b.iter().map(|x| (*x, false)))
        .collect();
    all.sort_by(|x, y| x.0.total_cmp(&y.0));
    let n = all.len();
    let mut rank_sum_a = 0.0;
    let mut tie_term = 0.0;
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && all[j + 1].0 == all[i].0 {
            j += 1;
        }
        // Ranks i+1 ..= j+1 share their average.
        let avg = (i + j + 2) as f64 / 2.0;
        let t = (j - i + 1) as f64;
        tie_term += t * t * t - t;
        rank_sum_a += avg * all[i..=j].iter().filter(|x| x.1).count() as f64;
        i = j + 1;
    }
    let (n1f, n2f, nf) = (n1 as f64, n2 as f64, n as f64);
    let u = rank_sum_a - n1f * (n1f + 1.0) / 2.0;
    let mu = n1f * n2f / 2.0;
    let variance = n1f * n2f / 12.0 * ((nf + 1.0) - tie_term / (nf * (nf - 1.0)));
    if variance <= 0.0 {
        return None;
    }
    let diff = u - mu;
    let corrected = (diff.abs() - 0.5).max(0.0) * diff.signum();
    let z = corrected / variance.sqrt();
    let p_value = (2.0 * (1.0 - normal_cdf(z.abs()))).clamp(0.0, 1.0);
    Some(MannWhitney {
        u,
        z,
        p_value,
        method: "mann_whitney_u_normal_approximation",
    })
}

#[cfg(test)]
mod tests {
    //! Expected values were computed independently with Python 3's standard
    //! library (`statistics.mean`, `median`, `stdev`,
    //! `quantiles(method="inclusive")`, `NormalDist`) and checked against
    //! published Student t tables; they are not outputs of this module.
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn rejects_empty_and_invalid_samples() {
        assert_eq!(Summary::of(&[]), Err(StatsError::Empty));
        assert_eq!(Summary::of(&[1.0, f64::NAN]), Err(StatsError::Invalid(1)));
        assert_eq!(Summary::of(&[-1.0]), Err(StatsError::Invalid(0)));
        assert_eq!(
            Summary::of(&[1.0, f64::INFINITY]),
            Err(StatsError::Invalid(1))
        );
    }

    #[test]
    fn single_sample_reports_what_it_cannot_compute() {
        let s = Summary::of(&[5.0]).unwrap();
        assert_eq!(
            (s.count, s.mean, s.median, s.min, s.max),
            (1, 5.0, 5.0, 5.0, 5.0)
        );
        assert!(s.std_dev.is_none() && s.mean_ci.is_none() && s.median_ci.is_none());
        assert!(s.percentiles.is_empty());
        assert_eq!(s.notes.len(), 4);
    }

    #[test]
    fn known_sample_of_ten() {
        // statistics.mean = 5.5, median = 5.5, stdev = 3.0276503540974917
        // quantiles(range(1, 11), n=10, method="inclusive")[8] = 9.1 (p90)
        let xs: Vec<f64> = (1..=10).map(f64::from).collect();
        let s = Summary::of(&xs).unwrap();
        assert!(close(s.mean, 5.5, 1e-12));
        assert!(close(s.median, 5.5, 1e-12));
        assert!(close(s.std_dev.unwrap(), 3.027_650_354_097_491_7, 1e-12));
        assert!(close(s.mad, 2.5, 1e-12));
        assert_eq!(s.percentiles.len(), 1, "p95 needs 20 samples, p99 100");
        assert!(close(s.percentiles[0].value, 9.1, 1e-12));
        // t(0.975, 9) = 2.262157 (tables); half width = t * sd / sqrt(10).
        let ci = s.mean_ci.unwrap();
        let half = 2.262_157_163_4 * 3.027_650_354_097_491_7 / 10f64.sqrt();
        assert!(close(ci.lower, 5.5 - half, 1e-8) && close(ci.upper, 5.5 + half, 1e-8));
        // n = 10: k = 2, coverage 1 − 2·P(B ≤ 1) = 1 − 2·11/1024 = 0.978515625.
        let m = s.median_ci.unwrap();
        assert_eq!((m.lower, m.upper), (2.0, 9.0));
        assert!(close(m.coverage, 0.978_515_625, 1e-12));
    }

    #[test]
    fn percentiles_follow_the_sample_count() {
        let xs: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = Summary::of(&xs).unwrap();
        // quantiles(range(1, 101), n=100, method="inclusive"): p90 = 90.1,
        // p95 = 95.05, p99 = 99.01.
        let values: Vec<f64> = s.percentiles.iter().map(|p| p.value).collect();
        assert_eq!(values.len(), 3);
        for (got, want) in values.iter().zip([90.1, 95.05, 99.01]) {
            assert!(close(*got, want, 1e-9), "{got} != {want}");
        }
        // Order of the input does not matter.
        let mut reversed = xs.clone();
        reversed.reverse();
        assert_eq!(Summary::of(&reversed).unwrap(), s);
    }

    #[test]
    fn median_interval_needs_six_samples() {
        assert!(
            Summary::of(&[1.0, 2.0, 3.0, 4.0, 5.0])
                .unwrap()
                .median_ci
                .is_none()
        );
        // n = 6: only [x(1), x(6)], coverage 1 − 2/64 = 0.96875.
        let m = Summary::of(&[6.0, 1.0, 5.0, 2.0, 4.0, 3.0])
            .unwrap()
            .median_ci
            .unwrap();
        assert_eq!((m.lower, m.upper), (1.0, 6.0));
        assert!(close(m.coverage, 0.968_75, 1e-12));
    }

    #[test]
    fn student_t_quantiles_match_tables() {
        for (df, want) in [
            (1.0, 12.706_204_736),
            (2.0, 4.302_652_730),
            (9.0, 2.262_157_163),
            (19.0, 2.093_024_054),
            (29.0, 2.045_229_642),
            (99.0, 1.984_216_952),
        ] {
            let got = t_quantile(0.975, df);
            assert!(close(got, want, 1e-6), "df {df}: {got} != {want}");
        }
        assert!(close(t_cdf(0.0, 5.0), 0.5, 1e-12));
    }

    #[test]
    fn normal_cdf_matches_the_standard_library_reference() {
        // NormalDist().cdf(z) in Python.
        for (z, want) in [
            (0.0, 0.5),
            (1.0, 0.841_344_746_068_542_9),
            (1.959_963_984_540_054, 0.975),
            (-2.575_829_303_548_901, 0.005),
            (3.5, 0.999_767_370_920_964),
        ] {
            assert!(close(normal_cdf(z), want, 2e-7), "{z}");
        }
    }

    #[test]
    fn mann_whitney_textbook_example() {
        // SciPy's documented example: U = 17 for these samples, asymptotic
        // two-sided p = 0.11134688653314041 (continuity corrected):
        // z = (17 − 10 − 0.5) / sqrt(5·4·10/12) and p = 2·(1 − Φ(z)).
        let males = [19.0, 22.0, 16.0, 29.0, 24.0];
        let females = [20.0, 11.0, 17.0, 12.0];
        let r = mann_whitney(&males, &females).unwrap();
        assert!(close(r.u, 17.0, 1e-12));
        assert!(close(r.p_value, 0.111_346_886_533_140_41, 1e-6));
        // Swapping the samples gives U = n1·n2 − 17 and the same p.
        let s = mann_whitney(&females, &males).unwrap();
        assert!(close(s.u, 3.0, 1e-12) && close(s.p_value, r.p_value, 1e-12));
    }

    #[test]
    fn mann_whitney_ties_and_degenerate_cases() {
        assert!(mann_whitney(&[], &[1.0]).is_none());
        assert!(mann_whitney(&[1.0, 1.0], &[1.0, 1.0]).is_none(), "all tied");
        // Separated samples: U = 0 (a all smaller). With ties in b.
        // Ranks: a = 1, 2, 3; b = 4.5, 4.5, 6 → U_a = 6 − 6 = 0.
        let r = mann_whitney(&[1.0, 2.0, 3.0], &[4.0, 4.0, 5.0]).unwrap();
        assert!(close(r.u, 0.0, 1e-12));
        // Tie-corrected variance = 9/12·(7 − 6/30) = 5.1; z = (−4.5 + 0.5)/√5.1.
        assert!(close(r.z, -4.0 / 5.1f64.sqrt(), 1e-12));
        // Identical samples are never significantly different.
        let same: Vec<f64> = (1..=20).map(f64::from).collect();
        let r = mann_whitney(&same, &same).unwrap();
        assert!(r.p_value > 0.9);
    }
}
