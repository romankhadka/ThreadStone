//! Turning six incompatible units into one comparable number.
//!
//! # The reference core
//!
//! Every workload is normalised against a fixed reference value, and the
//! geometric mean of those ratios becomes the score. The reference is the
//! **ThreadStone Reference Core v1**: a nominal 3.0 GHz out-of-order core with
//! 256-bit SIMD and a single DDR4-3200 channel. It is a *definition*, not a
//! machine anyone owns.
//!
//! That matters, and it is worth being blunt about why. A reference derived
//! from whatever hardware the author happened to have makes the author's
//! machine score exactly 1000 and everything else look like a deviation from
//! it. Fixing the reference by fiat, in round numbers, published in this file,
//! means the author's machine lands wherever it lands.
//!
//! The reference values are frozen for the lifetime of schema version 2.
//! Changing one would silently invalidate every previously published score, so
//! a revision would ship as "Reference Core v2" alongside a schema bump.
//!
//! # Why the geometric mean
//!
//! The arithmetic mean of ratios is not a meaningful composite: it depends on
//! which machine you designate as the denominator, so A can beat B under one
//! choice of reference and lose under another. The geometric mean is invariant
//! to that choice — the ratio of two machines' scores is the same whatever the
//! reference — which is the whole point of having a normalised score.
//!
//! # Direction
//!
//! Latency is reported in nanoseconds, where lower is better. Normalising it as
//! `reference / measured` rather than `measured / reference` puts it on the
//! same "bigger is better" footing as everything else, so it can join the
//! geometric mean without special-casing.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::kernel::Unit;
use crate::stats::geometric_mean;

/// Name of the fixed reference, recorded in every result.
pub const REFERENCE_NAME: &str = "ThreadStone Reference Core v1";

/// Score assigned to a machine that exactly matches the reference.
///
/// A round number, so that "1240" reads immediately as "24% faster than
/// reference" rather than requiring arithmetic.
pub const REFERENCE_SCORE: f64 = 1000.0;

/// One workload's contribution to a composite score.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScoreComponent {
    /// Workload identifier.
    pub id: String,
    /// Measured value, in the workload's native unit.
    pub measured: f64,
    /// Reference value for that workload.
    pub reference: f64,
    /// Direction-corrected ratio; 1.0 means "matches the reference".
    pub ratio: f64,
}

/// Composite scores for one machine.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScoreCard {
    /// Name of the reference these scores are relative to.
    pub reference: String,
    /// Single-core score. `REFERENCE_SCORE` means "matches the reference core".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub single_core: Option<f64>,
    /// Multi-core score, across every core the machine has.
    ///
    /// Excludes workloads marked [`crate::kernel::Scaling::SingleThreadOnly`],
    /// so it is not directly comparable to `single_core` on a per-workload
    /// basis — only machine to machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_core: Option<f64>,
    /// Per-workload single-core ratios that produced `single_core`.
    pub single_core_components: Vec<ScoreComponent>,
    /// Per-workload multi-core ratios that produced `multi_core`.
    pub multi_core_components: Vec<ScoreComponent>,
}

/// Normalise one measurement against its reference, correcting for direction.
///
/// Returns `None` when the ratio is undefined — a non-positive or non-finite
/// measurement, or a non-positive reference — so a broken workload drops out of
/// the geometric mean instead of poisoning it.
pub fn ratio(measured: f64, reference: f64, unit: Unit) -> Option<f64> {
    if !measured.is_finite() || !reference.is_finite() || measured <= 0.0 || reference <= 0.0 {
        return None;
    }
    Some(if unit.higher_is_better() {
        measured / reference
    } else {
        reference / measured
    })
}

/// Combine component ratios into a score.
///
/// Returns `None` for an empty component list, which is the honest answer when
/// no workload produced a usable ratio.
pub fn composite(components: &[ScoreComponent]) -> Option<f64> {
    let ratios: Vec<f64> = components.iter().map(|c| c.ratio).collect();
    geometric_mean(&ratios).map(|g| g * REFERENCE_SCORE)
}

impl ScoreCard {
    /// Build a scorecard from single- and multi-core components.
    pub fn new(single: Vec<ScoreComponent>, multi: Vec<ScoreComponent>) -> ScoreCard {
        ScoreCard {
            reference: REFERENCE_NAME.to_string(),
            single_core: composite(&single),
            multi_core: composite(&multi),
            single_core_components: single,
            multi_core_components: multi,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(ratio: f64) -> ScoreComponent {
        ScoreComponent {
            id: "test".to_string(),
            measured: 1.0,
            reference: 1.0,
            ratio,
        }
    }

    #[test]
    fn throughput_ratio_is_measured_over_reference() {
        let r = ratio(20.0, 10.0, Unit::Gflops).unwrap();
        assert!((r - 2.0).abs() < 1e-12, "twice as fast should be 2.0");
    }

    #[test]
    fn latency_ratio_is_inverted() {
        // Half the latency is twice as good.
        let r = ratio(45.0, 90.0, Unit::Nanoseconds).unwrap();
        assert!((r - 2.0).abs() < 1e-12, "half the latency should be 2.0");
    }

    #[test]
    fn degenerate_inputs_yield_no_ratio() {
        assert!(ratio(0.0, 10.0, Unit::Gflops).is_none());
        assert!(ratio(-1.0, 10.0, Unit::Gflops).is_none());
        assert!(ratio(10.0, 0.0, Unit::Gflops).is_none());
        assert!(ratio(f64::NAN, 10.0, Unit::Gflops).is_none());
        assert!(ratio(f64::INFINITY, 10.0, Unit::Gflops).is_none());
    }

    #[test]
    fn matching_the_reference_scores_exactly_one_thousand() {
        let components = vec![component(1.0), component(1.0), component(1.0)];
        let score = composite(&components).unwrap();
        assert!(
            (score - REFERENCE_SCORE).abs() < 1e-9,
            "expected {REFERENCE_SCORE}, got {score}"
        );
    }

    #[test]
    fn composite_of_nothing_is_none() {
        assert!(composite(&[]).is_none());
    }

    #[test]
    fn score_is_invariant_to_the_choice_of_reference() {
        // The property that justifies using a geometric mean: the ratio between
        // two machines' scores must not depend on what we normalised against.
        let a_raw = [30.0, 12.0, 400.0];
        let b_raw = [15.0, 9.0, 500.0];

        let score_with = |raw: &[f64; 3], reference: &[f64; 3]| {
            let comps: Vec<ScoreComponent> = raw
                .iter()
                .zip(reference)
                .map(|(m, r)| component(ratio(*m, *r, Unit::Gflops).unwrap()))
                .collect();
            composite(&comps).unwrap()
        };

        let ref1 = [10.0, 10.0, 100.0];
        let ref2 = [37.0, 1.5, 912.0];

        let with_ref1 = score_with(&a_raw, &ref1) / score_with(&b_raw, &ref1);
        let with_ref2 = score_with(&a_raw, &ref2) / score_with(&b_raw, &ref2);
        assert!(
            (with_ref1 - with_ref2).abs() < 1e-9,
            "geometric mean must be reference-invariant: {with_ref1} vs {with_ref2}"
        );
    }

    #[test]
    fn uniform_doubling_doubles_the_score() {
        let doubled = vec![component(2.0), component(2.0), component(2.0)];
        let score = composite(&doubled).unwrap();
        assert!((score - 2.0 * REFERENCE_SCORE).abs() < 1e-9);
    }

    #[test]
    fn scorecard_reports_both_axes() {
        let card = ScoreCard::new(vec![component(1.0)], vec![component(4.0)]);
        assert_eq!(card.reference, REFERENCE_NAME);
        assert!((card.single_core.unwrap() - 1000.0).abs() < 1e-9);
        assert!((card.multi_core.unwrap() - 4000.0).abs() < 1e-9);
    }

    #[test]
    fn scorecard_with_no_components_reports_no_scores() {
        let card = ScoreCard::new(vec![], vec![]);
        assert!(card.single_core.is_none());
        assert!(card.multi_core.is_none());
    }
}
