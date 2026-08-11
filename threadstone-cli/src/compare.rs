//! Comparing two result files.
//!
//! The question a benchmark suite exists to answer is usually "did this change
//! anything?", and answering it needs more than two numbers side by side. A 3%
//! difference between runs that each vary by 5% is noise; the same 3% between
//! runs that vary by 0.2% is a real regression.
//!
//! So every delta here is reported against the combined uncertainty of both
//! measurements, and only differences that clear it are called significant.

use threadstone_core::report::{Pass, Report};

/// How a measurement changed between two reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Improved by more than the combined uncertainty.
    Faster,
    /// Regressed by more than the combined uncertainty.
    Slower,
    /// Changed by less than the combined uncertainty.
    Unchanged,
    /// Present in only one of the two reports.
    Missing,
}

impl Verdict {
    /// Short marker for terminal output.
    pub fn glyph(self) -> &'static str {
        match self {
            Verdict::Faster => "+",
            Verdict::Slower => "-",
            Verdict::Unchanged => "=",
            Verdict::Missing => "?",
        }
    }
}

/// One workload's change between two reports.
#[derive(Debug, Clone)]
pub struct Delta {
    /// Workload identifier — the stable key, and what `--workload` accepts.
    pub id: String,
    /// Baseline value, if the workload ran there.
    pub baseline: Option<f64>,
    /// Candidate value, if the workload ran there.
    pub candidate: Option<f64>,
    /// Signed percentage change, direction-corrected so positive always means
    /// better. `None` when either side is missing.
    pub percent: Option<f64>,
    /// Whether the change exceeds the combined uncertainty.
    pub verdict: Verdict,
}

/// A full comparison of two reports.
#[derive(Debug, Clone)]
pub struct Comparison {
    /// Per-workload single-thread deltas.
    pub single: Vec<Delta>,
    /// Per-workload multi-thread deltas.
    pub multi: Vec<Delta>,
    /// Percentage change in the single-core score.
    pub single_score: Option<f64>,
    /// Percentage change in the multi-core score.
    pub multi_score: Option<f64>,
    /// Set when the two reports came from different machines, in which case the
    /// comparison measures the machines rather than the change.
    pub machine_mismatch: Option<String>,
}

/// Relative uncertainty below which a measurement is treated as exact.
///
/// A pass with a single sample reports zero confidence interval, which would
/// make every difference look significant. Half a percent is a floor on what
/// this suite claims to be able to resolve.
const MIN_RELATIVE_UNCERTAINTY: f64 = 0.005;

/// Compare `candidate` against `baseline`.
pub fn compare(baseline: &Report, candidate: &Report) -> Comparison {
    let machine_mismatch = describe_mismatch(baseline, candidate);

    let single = deltas(baseline, candidate, |w| &w.single_thread);
    let multi = deltas(baseline, candidate, |w| &w.multi_thread);

    Comparison {
        single,
        multi,
        single_score: percent_change(baseline.score.single_core, candidate.score.single_core),
        multi_score: percent_change(baseline.score.multi_core, candidate.score.multi_core),
        machine_mismatch,
    }
}

fn describe_mismatch(a: &Report, b: &Report) -> Option<String> {
    let a_cpu = a.system.cpu_model.as_deref().unwrap_or("unknown");
    let b_cpu = b.system.cpu_model.as_deref().unwrap_or("unknown");
    if a_cpu != b_cpu {
        return Some(format!("different CPUs: '{a_cpu}' vs '{b_cpu}'"));
    }
    if a.system.target != b.system.target {
        return Some(format!(
            "different targets: '{}' vs '{}'",
            a.system.target, b.system.target
        ));
    }
    if a.config.threads != b.config.threads {
        return Some(format!(
            "different thread counts: {} vs {}",
            a.config.threads, b.config.threads
        ));
    }
    None
}

fn deltas(
    baseline: &Report,
    candidate: &Report,
    select: impl Fn(&threadstone_core::report::WorkloadReport) -> &Option<Pass>,
) -> Vec<Delta> {
    let mut out = Vec::new();
    for base_w in &baseline.workloads {
        let cand_w = candidate.workloads.iter().find(|w| w.id == base_w.id);
        let base_pass = select(base_w).as_ref();
        let cand_pass = cand_w.and_then(|w| select(w).as_ref());

        let (percent, verdict) = match (base_pass, cand_pass) {
            (Some(b), Some(c)) => {
                let higher_is_better = base_w.unit.higher_is_better();
                let pct = signed_percent(b.value, c.value, higher_is_better);
                (Some(pct), classify(b, c, pct))
            }
            _ => (None, Verdict::Missing),
        };

        out.push(Delta {
            id: base_w.id.clone(),
            baseline: base_pass.map(|p| p.value),
            candidate: cand_pass.map(|p| p.value),
            percent,
            verdict,
        });
    }

    // Workloads present only in the candidate still deserve a row.
    for cand_w in &candidate.workloads {
        if baseline.workloads.iter().any(|w| w.id == cand_w.id) {
            continue;
        }
        out.push(Delta {
            id: cand_w.id.clone(),
            baseline: None,
            candidate: select(cand_w).as_ref().map(|p| p.value),
            percent: None,
            verdict: Verdict::Missing,
        });
    }
    out
}

/// Percentage change, positive when `candidate` is better.
fn signed_percent(baseline: f64, candidate: f64, higher_is_better: bool) -> f64 {
    if baseline == 0.0 || !baseline.is_finite() || !candidate.is_finite() {
        return 0.0;
    }
    let raw = (candidate - baseline) / baseline * 100.0;
    if higher_is_better {
        raw
    } else {
        -raw
    }
}

/// Decide whether a change is larger than the noise in both measurements.
///
/// Combines the two confidence intervals in quadrature, which is the standard
/// treatment for independent uncertainties, and floors each at
/// [`MIN_RELATIVE_UNCERTAINTY`] so a single-sample pass cannot claim infinite
/// precision.
fn classify(baseline: &Pass, candidate: &Pass, percent: f64) -> Verdict {
    let relative = |p: &Pass| {
        if p.value == 0.0 || !p.value.is_finite() {
            return MIN_RELATIVE_UNCERTAINTY;
        }
        (p.stats.ci95 / p.value).abs().max(MIN_RELATIVE_UNCERTAINTY)
    };
    let combined = (relative(baseline).powi(2) + relative(candidate).powi(2)).sqrt() * 100.0;

    if percent.abs() <= combined {
        Verdict::Unchanged
    } else if percent > 0.0 {
        Verdict::Faster
    } else {
        Verdict::Slower
    }
}

fn percent_change(baseline: Option<f64>, candidate: Option<f64>) -> Option<f64> {
    let (b, c) = (baseline?, candidate?);
    if b == 0.0 || !b.is_finite() || !c.is_finite() {
        return None;
    }
    // Scores are always higher-is-better by construction.
    Some((c - b) / b * 100.0)
}

/// Render a comparison as a terminal table.
pub fn render(comparison: &Comparison, baseline_label: &str, candidate_label: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("{baseline_label}  →  {candidate_label}\n\n"));

    if let Some(warning) = &comparison.machine_mismatch {
        out.push_str(&format!(
            "warning: {warning}\n         this compares two machines, not two versions\n\n"
        ));
    }

    for (title, deltas) in [
        ("single-thread", &comparison.single),
        ("multi-thread", &comparison.multi),
    ] {
        let rows: Vec<&Delta> = deltas
            .iter()
            .filter(|d| d.baseline.is_some() || d.candidate.is_some())
            .collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("{title}\n"));
        for d in rows {
            let base = d.baseline.map_or("—".to_string(), crate::render::si);
            let cand = d.candidate.map_or("—".to_string(), crate::render::si);
            let change = d
                .percent
                .map_or_else(|| "—".to_string(), |p| format!("{p:+.1}%"));
            out.push_str(&format!(
                "  {} {:<18} {:>10} → {:>10}  {:>8}\n",
                d.verdict.glyph(),
                d.id,
                base,
                cand,
                change,
            ));
        }
        out.push('\n');
    }

    let score = |label: &str, pct: Option<f64>| match pct {
        Some(p) => format!("  {label} score {p:+.1}%\n"),
        None => String::new(),
    };
    out.push_str("score\n");
    out.push_str(&score("single-core", comparison.single_score));
    out.push_str(&score(" multi-core", comparison.multi_score));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadstone_core::kernel::Unit;
    use threadstone_core::report::{RunSettings, WorkloadReport};
    use threadstone_core::score::ScoreCard;
    use threadstone_core::stats::Summary;
    use threadstone_core::sysinfo::SystemInfo;

    fn pass(value: f64, ci95: f64) -> Pass {
        let mut stats = Summary::new(&[value]).unwrap();
        stats.ci95 = ci95;
        Pass {
            threads: 1,
            iters_per_thread: 1000,
            value,
            samples: vec![value],
            stats,
            window_ms: 250.0,
            window_too_short: false,
        }
    }

    fn report(workloads: Vec<WorkloadReport>, score: Option<f64>) -> Report {
        Report {
            schema_version: 2,
            tool_version: "test".into(),
            generated_at: "2026-01-01T00:00:00Z".into(),
            duration_secs: 1.0,
            system: SystemInfo {
                cpu_model: Some("Test CPU".into()),
                target: "test-target".into(),
                ..SystemInfo::detect()
            },
            config: RunSettings {
                threads: 4,
                samples: 7,
                warmup: 2,
                window_ms: 250,
            },
            workloads,
            score: ScoreCard {
                reference: "test".into(),
                single_core: score,
                multi_core: score,
                single_core_components: vec![],
                multi_core_components: vec![],
            },
            signature: None,
        }
    }

    fn workload(id: &str, unit: Unit, single: Option<Pass>) -> WorkloadReport {
        WorkloadReport {
            id: id.into(),
            name: id.into(),
            summary: "test".into(),
            unit,
            reference: 1.0,
            single_thread: single,
            multi_thread: None,
            scaling: None,
            excluded_from_multi_core: None,
            error: None,
        }
    }

    #[test]
    fn a_clear_improvement_is_flagged_faster() {
        let a = report(
            vec![workload("x", Unit::Gflops, Some(pass(100.0, 0.5)))],
            None,
        );
        let b = report(
            vec![workload("x", Unit::Gflops, Some(pass(120.0, 0.5)))],
            None,
        );
        let c = compare(&a, &b);
        assert_eq!(c.single[0].verdict, Verdict::Faster);
        assert!((c.single[0].percent.unwrap() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn a_clear_regression_is_flagged_slower() {
        let a = report(
            vec![workload("x", Unit::Gflops, Some(pass(100.0, 0.5)))],
            None,
        );
        let b = report(
            vec![workload("x", Unit::Gflops, Some(pass(80.0, 0.5)))],
            None,
        );
        let c = compare(&a, &b);
        assert_eq!(c.single[0].verdict, Verdict::Slower);
        assert!((c.single[0].percent.unwrap() + 20.0).abs() < 1e-9);
    }

    #[test]
    fn a_change_inside_the_noise_is_unchanged() {
        // 2% apart, but each measurement is itself ±5%.
        let a = report(
            vec![workload("x", Unit::Gflops, Some(pass(100.0, 5.0)))],
            None,
        );
        let b = report(
            vec![workload("x", Unit::Gflops, Some(pass(102.0, 5.0)))],
            None,
        );
        assert_eq!(compare(&a, &b).single[0].verdict, Verdict::Unchanged);
    }

    #[test]
    fn the_same_change_is_significant_when_the_runs_are_tight() {
        // Identical 2% delta, but now each run varies by only 0.1%.
        let a = report(
            vec![workload("x", Unit::Gflops, Some(pass(100.0, 0.1)))],
            None,
        );
        let b = report(
            vec![workload("x", Unit::Gflops, Some(pass(102.0, 0.1)))],
            None,
        );
        assert_eq!(compare(&a, &b).single[0].verdict, Verdict::Faster);
    }

    #[test]
    fn latency_improvements_are_reported_as_positive() {
        // Lower nanoseconds is better, so the sign must flip.
        let a = report(
            vec![workload("lat", Unit::Nanoseconds, Some(pass(100.0, 0.1)))],
            None,
        );
        let b = report(
            vec![workload("lat", Unit::Nanoseconds, Some(pass(80.0, 0.1)))],
            None,
        );
        let c = compare(&a, &b);
        assert_eq!(c.single[0].verdict, Verdict::Faster);
        assert!(
            c.single[0].percent.unwrap() > 0.0,
            "dropping from 100ns to 80ns is an improvement"
        );
    }

    #[test]
    fn a_missing_workload_is_marked_rather_than_dropped() {
        let a = report(
            vec![workload("x", Unit::Gflops, Some(pass(100.0, 0.1)))],
            None,
        );
        let b = report(vec![], None);
        let c = compare(&a, &b);
        assert_eq!(c.single.len(), 1);
        assert_eq!(c.single[0].verdict, Verdict::Missing);
        assert!(c.single[0].candidate.is_none());
    }

    #[test]
    fn a_new_workload_appears_in_the_comparison() {
        let a = report(vec![], None);
        let b = report(
            vec![workload("new", Unit::Gflops, Some(pass(50.0, 0.1)))],
            None,
        );
        let c = compare(&a, &b);
        assert_eq!(c.single.len(), 1);
        assert_eq!(c.single[0].id, "new");
        assert_eq!(c.single[0].verdict, Verdict::Missing);
    }

    #[test]
    fn identical_reports_show_no_change() {
        let a = report(
            vec![workload("x", Unit::Gflops, Some(pass(100.0, 0.1)))],
            Some(1000.0),
        );
        let c = compare(&a, &a);
        assert_eq!(c.single[0].verdict, Verdict::Unchanged);
        assert!((c.single_score.unwrap()).abs() < 1e-9);
    }

    #[test]
    fn a_different_cpu_is_called_out() {
        let a = report(vec![], None);
        let mut b = report(vec![], None);
        b.system.cpu_model = Some("Other CPU".into());
        let c = compare(&a, &b);
        assert!(
            c.machine_mismatch
                .as_ref()
                .unwrap()
                .contains("different CPUs"),
            "got {:?}",
            c.machine_mismatch
        );
    }

    #[test]
    fn score_change_is_a_plain_percentage() {
        let a = report(vec![], Some(1000.0));
        let b = report(vec![], Some(1250.0));
        assert!((compare(&a, &b).single_score.unwrap() - 25.0).abs() < 1e-9);
    }

    #[test]
    fn rendering_mentions_a_machine_mismatch() {
        let a = report(
            vec![workload("x", Unit::Gflops, Some(pass(1.0, 0.1)))],
            Some(10.0),
        );
        let mut b = report(
            vec![workload("x", Unit::Gflops, Some(pass(2.0, 0.1)))],
            Some(20.0),
        );
        b.system.cpu_model = Some("Other".into());
        let text = render(&compare(&a, &b), "a.json", "b.json");
        assert!(text.contains("warning"));
        assert!(text.contains("two machines"));
        assert!(text.contains("+100.0%"));
    }
}
