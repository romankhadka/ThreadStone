//! Robust summary statistics for benchmark samples.
//!
//! A benchmark that reports only mean/min/max cannot tell you whether to
//! believe it. These statistics exist so a result can characterise its own
//! reliability: the coefficient of variation says how noisy the machine was,
//! and the MAD-based outlier filter says how many samples were disturbed.
//!
//! Median is preferred over mean throughout. Benchmark noise is one-sided —
//! interrupts, migrations, and thermal events only ever make a sample slower —
//! so the distribution has a hard floor and a long right tail. The mean chases
//! that tail; the median does not.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Scale factor converting median absolute deviation to a standard-deviation
/// equivalent for normally distributed data (1 / Φ⁻¹(0.75)).
const MAD_TO_SIGMA: f64 = 1.482_602_218_505_602;

/// Samples deviating by more than this many MAD-sigmas are flagged as outliers.
/// Three is conventional and, for a hard-floored distribution, conservative.
const OUTLIER_SIGMAS: f64 = 3.0;

/// How much run-to-run variation a result exhibited.
///
/// Thresholds are expressed on the coefficient of variation of the retained
/// samples. They are deliberately strict: a CPU benchmark on an idle machine
/// should comfortably reach `Stable`, and anything worse is a signal that the
/// measurement environment — not the CPU — is what is being observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Stability {
    /// CV below 1%. Differences of a few percent between runs are meaningful.
    Stable,
    /// CV below 3%. Usable, but only trust differences larger than the spread.
    Acceptable,
    /// CV below 10%. The machine was busy or thermally constrained.
    Noisy,
    /// CV at or above 10%. Do not draw conclusions from this run.
    Unreliable,
}

impl Stability {
    /// Classify a coefficient of variation, expressed as a fraction (0.01 = 1%).
    pub fn from_cv(cv: f64) -> Self {
        if !cv.is_finite() {
            return Stability::Unreliable;
        }
        match cv {
            c if c < 0.01 => Stability::Stable,
            c if c < 0.03 => Stability::Acceptable,
            c if c < 0.10 => Stability::Noisy,
            _ => Stability::Unreliable,
        }
    }

    /// Whether results at this stability level support drawing conclusions.
    pub fn is_trustworthy(self) -> bool {
        matches!(self, Stability::Stable | Stability::Acceptable)
    }

    /// Single-character marker for terminal output.
    pub fn glyph(self) -> char {
        match self {
            Stability::Stable => '=',
            Stability::Acceptable => '~',
            Stability::Noisy => '!',
            Stability::Unreliable => 'x',
        }
    }
}

/// Summary of a set of benchmark samples.
///
/// All fields are in the same unit as the input samples. `median` is the
/// headline figure; the rest exist to qualify it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Summary {
    /// Number of samples retained after outlier rejection.
    pub n: usize,
    /// Number of samples discarded as outliers.
    pub outliers: usize,
    /// Median of retained samples. This is the value to quote.
    pub median: f64,
    /// Arithmetic mean of retained samples.
    pub mean: f64,
    /// Sample standard deviation (Bessel-corrected) of retained samples.
    pub stddev: f64,
    /// Standard deviation as a fraction of the mean.
    pub cv: f64,
    /// Smallest retained sample.
    pub min: f64,
    /// Largest retained sample.
    pub max: f64,
    /// 5th percentile of retained samples (linear interpolation).
    pub p05: f64,
    /// 95th percentile of retained samples (linear interpolation).
    pub p95: f64,
    /// Half-width of the 95% confidence interval on the mean.
    ///
    /// Uses a normal approximation (1.96·σ/√n), which is adequate at the sample
    /// counts this suite collects and errs slightly narrow below n≈10.
    pub ci95: f64,
    /// Verdict on whether this result is trustworthy.
    pub stability: Stability,
}

impl Summary {
    /// Summarise `samples`, rejecting outliers by median absolute deviation.
    ///
    /// Returns `None` for an empty input. Non-finite samples are dropped before
    /// any statistic is computed, so a single NaN cannot poison the result.
    ///
    /// Outlier rejection is skipped for fewer than four samples: with three or
    /// fewer points the MAD is not estimable and rejection would amount to
    /// discarding data at random.
    pub fn new(samples: &[f64]) -> Option<Summary> {
        let clean: Vec<f64> = samples.iter().copied().filter(|v| v.is_finite()).collect();
        if clean.is_empty() {
            return None;
        }

        let (kept, outliers) = if clean.len() < 4 {
            (clean, 0)
        } else {
            let med = median_of(&sorted(&clean));
            let mad = median_absolute_deviation(&clean, med);
            if mad == 0.0 {
                // Over half the samples equal the median exactly; the scale
                // estimate collapses and rejection would be arbitrary.
                (clean, 0)
            } else {
                let limit = OUTLIER_SIGMAS * MAD_TO_SIGMA * mad;
                let before = clean.len();
                let kept: Vec<f64> = clean
                    .iter()
                    .copied()
                    .filter(|v| (v - med).abs() <= limit)
                    .collect();
                // Guard against a pathological distribution rejecting
                // everything; if it would, keep the original set.
                if kept.is_empty() {
                    (clean, 0)
                } else {
                    let removed = before - kept.len();
                    (kept, removed)
                }
            }
        };

        let sorted = sorted(&kept);
        let n = kept.len();
        let mean = kept.iter().sum::<f64>() / n as f64;
        let stddev = if n > 1 {
            let var = kept.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0);
            var.sqrt()
        } else {
            0.0
        };
        let cv = if mean != 0.0 {
            (stddev / mean).abs()
        } else {
            0.0
        };

        Some(Summary {
            n,
            outliers,
            median: median_of(&sorted),
            mean,
            stddev,
            cv,
            min: sorted[0],
            max: sorted[n - 1],
            p05: percentile_of(&sorted, 0.05),
            p95: percentile_of(&sorted, 0.95),
            ci95: if n > 1 {
                1.96 * stddev / (n as f64).sqrt()
            } else {
                0.0
            },
            stability: Stability::from_cv(cv),
        })
    }
}

/// Geometric mean of strictly positive values.
///
/// Computed in log space so that a suite spanning six orders of magnitude
/// (nanoseconds to gigaflops) cannot overflow the product. Returns `None` if
/// the input is empty or contains a non-positive value, since the geometric
/// mean is undefined there.
pub fn geometric_mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() || values.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return None;
    }
    let log_sum: f64 = values.iter().map(|v| v.ln()).sum();
    Some((log_sum / values.len() as f64).exp())
}

fn sorted(values: &[f64]) -> Vec<f64> {
    let mut v = values.to_vec();
    // `clean` has already excluded non-finite values, so a total order exists.
    v.sort_by(|a, b| a.partial_cmp(b).expect("non-finite value reached sort"));
    v
}

/// Median of an already-sorted, non-empty slice.
fn median_of(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// Linearly interpolated percentile of an already-sorted, non-empty slice.
fn percentile_of(sorted: &[f64], q: f64) -> f64 {
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let pos = q * (n - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = pos - lo as f64;
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    }
}

fn median_absolute_deviation(values: &[f64], median: f64) -> f64 {
    let devs: Vec<f64> = values.iter().map(|v| (v - median).abs()).collect();
    median_of(&sorted(&devs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn empty_input_has_no_summary() {
        assert!(Summary::new(&[]).is_none());
    }

    #[test]
    fn all_non_finite_input_has_no_summary() {
        assert!(Summary::new(&[f64::NAN, f64::INFINITY]).is_none());
    }

    #[test]
    fn single_sample_summarises_without_dividing_by_zero() {
        let s = Summary::new(&[42.0]).unwrap();
        approx(s.median, 42.0);
        approx(s.mean, 42.0);
        approx(s.stddev, 0.0);
        approx(s.ci95, 0.0);
        assert_eq!(s.n, 1);
        assert_eq!(s.stability, Stability::Stable);
    }

    #[test]
    fn median_uses_midpoint_for_even_counts() {
        let s = Summary::new(&[1.0, 2.0, 3.0, 4.0]).unwrap();
        approx(s.median, 2.5);
    }

    #[test]
    fn nan_samples_are_dropped_not_propagated() {
        let s = Summary::new(&[10.0, f64::NAN, 10.0, 10.0]).unwrap();
        assert_eq!(s.n, 3);
        approx(s.median, 10.0);
        assert!(s.mean.is_finite());
    }

    #[test]
    fn outlier_is_rejected() {
        // Nine tight samples and one that is wildly slow, as an interrupt
        // during a run would produce.
        let mut samples = vec![100.0, 101.0, 99.0, 100.5, 99.5, 100.2, 99.8, 100.1, 99.9];
        samples.push(500.0);
        let s = Summary::new(&samples).unwrap();
        assert_eq!(s.outliers, 1, "the 500.0 sample should be rejected");
        assert_eq!(s.n, 9);
        assert!(
            s.max < 200.0,
            "outlier must not survive into max: {}",
            s.max
        );
        assert_eq!(s.stability, Stability::Stable);
    }

    #[test]
    fn identical_samples_reject_nothing() {
        let s = Summary::new(&[7.0; 8]).unwrap();
        assert_eq!(s.outliers, 0);
        assert_eq!(s.n, 8);
        approx(s.cv, 0.0);
    }

    #[test]
    fn fewer_than_four_samples_skip_rejection() {
        // With three samples, MAD is meaningless; keep all of them.
        let s = Summary::new(&[1.0, 1.0, 100.0]).unwrap();
        assert_eq!(s.n, 3);
        assert_eq!(s.outliers, 0);
    }

    #[test]
    fn percentiles_interpolate() {
        let v: Vec<f64> = (1..=101).map(f64::from).collect();
        let s = Summary::new(&v).unwrap();
        approx(s.p05, 6.0);
        approx(s.p95, 96.0);
    }

    #[test]
    fn stability_thresholds_are_ordered() {
        assert_eq!(Stability::from_cv(0.005), Stability::Stable);
        assert_eq!(Stability::from_cv(0.02), Stability::Acceptable);
        assert_eq!(Stability::from_cv(0.05), Stability::Noisy);
        assert_eq!(Stability::from_cv(0.5), Stability::Unreliable);
        assert_eq!(Stability::from_cv(f64::NAN), Stability::Unreliable);
        assert!(Stability::Acceptable.is_trustworthy());
        assert!(!Stability::Noisy.is_trustworthy());
    }

    #[test]
    fn geometric_mean_of_powers_is_exact() {
        approx(geometric_mean(&[1.0, 4.0]).unwrap(), 2.0);
        approx(geometric_mean(&[8.0, 8.0, 8.0]).unwrap(), 8.0);
    }

    #[test]
    fn geometric_mean_rejects_non_positive_and_empty() {
        assert!(geometric_mean(&[]).is_none());
        assert!(geometric_mean(&[1.0, 0.0]).is_none());
        assert!(geometric_mean(&[1.0, -2.0]).is_none());
        assert!(geometric_mean(&[1.0, f64::NAN]).is_none());
    }

    #[test]
    fn geometric_mean_survives_extreme_spread() {
        // Nanoseconds alongside gigaflops: a naive product would overflow.
        let v = [1e-9, 1e9, 1e-9, 1e9];
        approx(geometric_mean(&v).unwrap(), 1.0);
    }
}
