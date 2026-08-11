//! Orchestrates a full suite run: every workload, both passes, one report.
//!
//! The two-pass structure is the point. Running only at full thread count
//! conflates per-core speed with core count, so a 64-core server and a fast
//! laptop become indistinguishable. Running both and reporting the ratio
//! separates them, and makes the parallel efficiency of each workload visible
//! rather than buried inside one aggregate number.

use std::time::{Duration, Instant};

use crate::kernel::{Kernel, Scaling};
use crate::report::{now_rfc3339, workload_report, Pass, Report, RunSettings, SCHEMA_VERSION};
use crate::runner::{self, Observer, RunConfig};
use crate::score::{ratio, ScoreCard, ScoreComponent};
use crate::sysinfo::SystemInfo;

/// How to execute a suite.
#[derive(Debug, Clone, Copy)]
pub struct SuiteConfig {
    /// Threads for the multi-core pass. Zero means "every logical core".
    pub threads: usize,
    /// Measured rounds per pass.
    pub samples: u32,
    /// Discarded rounds before each pass.
    pub warmup: u32,
    /// Target measurement window per round.
    pub window: Duration,
    /// Whether to run the single-thread pass.
    pub single_thread: bool,
    /// Whether to run the multi-thread pass.
    pub multi_thread: bool,
}

impl Default for SuiteConfig {
    fn default() -> Self {
        SuiteConfig {
            threads: 0,
            samples: runner::defaults::SAMPLES,
            warmup: runner::defaults::WARMUP,
            window: runner::defaults::WINDOW,
            single_thread: true,
            multi_thread: true,
        }
    }
}

/// Observes suite-level progress, in addition to per-run events.
pub trait SuiteObserver: Observer {
    /// A workload's pass at `threads` threads is starting.
    fn workload_start(&self, id: &str, name: &str, threads: usize) {
        let _ = (id, name, threads);
    }
    /// A workload failed; the suite will continue with the others.
    fn workload_failed(&self, id: &str, error: &str) {
        let _ = (id, error);
    }
}

/// Run `kernels` under `cfg` and assemble a [`Report`].
///
/// A workload that fails is recorded with its error and excluded from scoring;
/// the rest of the suite still runs. Losing one workload should cost that
/// workload's data, not the whole run's.
pub fn run(
    kernels: &[Box<dyn Kernel>],
    cfg: SuiteConfig,
    tool_version: &str,
    obs: &dyn SuiteObserver,
) -> Report {
    let started = Instant::now();
    let system = SystemInfo::detect();
    let mt_threads = if cfg.threads == 0 {
        system.default_threads()
    } else {
        cfg.threads
    };

    let mut workloads = Vec::with_capacity(kernels.len());
    let mut single_components = Vec::new();
    let mut multi_components = Vec::new();

    for kernel in kernels {
        let info = kernel.info();
        let mut errors: Vec<String> = Vec::new();

        // ---- Single-thread pass ------------------------------------------
        let single = if cfg.single_thread {
            obs.workload_start(info.id, info.name, 1);
            let run_cfg = RunConfig {
                threads: 1,
                samples: cfg.samples,
                warmup: cfg.warmup,
                window: cfg.window,
            };
            match runner::run(kernel.as_ref(), run_cfg, obs) {
                Ok(m) => Some(Pass::from_measurement(&m)),
                Err(e) => {
                    let msg = e.to_string();
                    obs.workload_failed(info.id, &msg);
                    errors.push(msg);
                    None
                }
            }
        } else {
            None
        };

        // ---- Multi-thread pass -------------------------------------------
        // Skipped entirely for single-thread-only workloads: see
        // `Scaling::SingleThreadOnly` for why a multi-threaded latency figure
        // would be actively misleading rather than merely uninteresting.
        let runs_multi = cfg.multi_thread && info.scaling == Scaling::Scales && mt_threads > 1;
        let multi = if runs_multi {
            obs.workload_start(info.id, info.name, mt_threads);
            let run_cfg = RunConfig {
                threads: mt_threads,
                samples: cfg.samples,
                warmup: cfg.warmup,
                window: cfg.window,
            };
            match runner::run(kernel.as_ref(), run_cfg, obs) {
                Ok(m) => Some(Pass::from_measurement(&m)),
                Err(e) => {
                    let msg = e.to_string();
                    obs.workload_failed(info.id, &msg);
                    errors.push(msg);
                    None
                }
            }
        } else {
            None
        };

        if let Some(p) = &single {
            if let Some(r) = ratio(p.value, info.reference, info.unit) {
                single_components.push(ScoreComponent {
                    id: info.id.to_string(),
                    measured: p.value,
                    reference: info.reference,
                    ratio: r,
                });
            }
        }
        if let Some(p) = &multi {
            if let Some(r) = ratio(p.value, info.reference, info.unit) {
                multi_components.push(ScoreComponent {
                    id: info.id.to_string(),
                    measured: p.value,
                    reference: info.reference,
                    ratio: r,
                });
            }
        }

        // A workload with neither pass must say why, or a reader — and
        // `threadstone verify` — cannot tell a skipped workload from a silently
        // broken one. The case that reaches here is `--multi-only` against a
        // single-thread-only workload: nothing failed, but nothing ran either.
        if single.is_none() && multi.is_none() && errors.is_empty() {
            errors.push(match info.scaling {
                Scaling::SingleThreadOnly => format!(
                    "not run: '{}' is measured single-threaded only, and the \
                     single-thread pass was disabled",
                    info.id
                ),
                Scaling::Scales => {
                    "not run: both the single- and multi-thread passes were disabled".to_string()
                }
            });
        }

        let error = if errors.is_empty() {
            None
        } else {
            Some(errors.join("; "))
        };
        workloads.push(workload_report(&info, single, multi, error));
    }

    Report {
        schema_version: SCHEMA_VERSION,
        tool_version: tool_version.to_string(),
        generated_at: now_rfc3339(),
        duration_secs: started.elapsed().as_secs_f64(),
        config: RunSettings {
            threads: mt_threads,
            samples: cfg.samples,
            warmup: cfg.warmup,
            window_ms: cfg.window.as_millis() as u64,
        },
        system,
        workloads,
        score: ScoreCard::new(single_components, multi_components),
        signature: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{Footprint, KernelInfo, KernelState, SetupCtx, Unit};

    struct Busy {
        id: &'static str,
        scaling: Scaling,
    }

    struct BusyState {
        acc: u64,
    }

    impl KernelState for BusyState {
        fn run(&mut self, iters: u64) -> u64 {
            let mut acc = self.acc;
            for _ in 0..iters {
                for _ in 0..32 {
                    acc = acc.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                }
            }
            self.acc = acc;
            acc
        }
    }

    impl Kernel for Busy {
        fn info(&self) -> KernelInfo {
            KernelInfo {
                id: self.id,
                name: "Busy",
                summary: "test kernel",
                unit: Unit::MelemPerSec,
                footprint: Footprint::PerThread,
                scaling: self.scaling,
                reference: 1.0,
            }
        }
        fn setup(&self, _ctx: &SetupCtx) -> Box<dyn KernelState> {
            Box::new(BusyState { acc: 1 })
        }
        fn rate(&self, iters: u64, threads: usize, secs: f64) -> f64 {
            iters as f64 * threads as f64 / secs / 1e6
        }
    }

    struct Silent;
    impl Observer for Silent {}
    impl SuiteObserver for Silent {}

    fn quick() -> SuiteConfig {
        SuiteConfig {
            threads: 2,
            samples: 2,
            warmup: 0,
            window: Duration::from_millis(15),
            single_thread: true,
            multi_thread: true,
        }
    }

    #[test]
    fn suite_produces_both_passes_and_scores() {
        let kernels: Vec<Box<dyn Kernel>> = vec![Box::new(Busy {
            id: "busy",
            scaling: Scaling::Scales,
        })];
        let report = run(&kernels, quick(), "test", &Silent);

        assert_eq!(report.schema_version, SCHEMA_VERSION);
        assert_eq!(report.workloads.len(), 1);
        let w = &report.workloads[0];
        assert!(w.single_thread.is_some());
        assert!(w.multi_thread.is_some());
        assert!(w.scaling.is_some());
        assert!(w.error.is_none());
        assert!(report.score.single_core.is_some());
        assert!(report.score.multi_core.is_some());
        assert!(report.duration_secs > 0.0);
    }

    #[test]
    fn single_thread_only_workloads_skip_the_multi_pass() {
        let kernels: Vec<Box<dyn Kernel>> = vec![Box::new(Busy {
            id: "st-only",
            scaling: Scaling::SingleThreadOnly,
        })];
        let report = run(&kernels, quick(), "test", &Silent);
        let w = &report.workloads[0];

        assert!(w.single_thread.is_some());
        assert!(w.multi_thread.is_none(), "must not run a multi-thread pass");
        assert!(
            w.excluded_from_multi_core.is_some(),
            "exclusion must be explained in the report"
        );
        assert!(
            report.score.multi_core.is_none(),
            "no eligible workloads means no multi-core score"
        );
        assert!(report.score.single_core.is_some());
    }

    #[test]
    fn a_failing_workload_does_not_abort_the_suite() {
        struct Vanishing;
        struct VanishingState;
        impl KernelState for VanishingState {
            fn run(&mut self, _iters: u64) -> u64 {
                0
            }
        }
        impl Kernel for Vanishing {
            fn info(&self) -> KernelInfo {
                KernelInfo {
                    id: "vanishing",
                    name: "Vanishing",
                    summary: "never fills a window",
                    unit: Unit::MelemPerSec,
                    footprint: Footprint::PerThread,
                    scaling: Scaling::Scales,
                    reference: 1.0,
                }
            }
            fn setup(&self, _ctx: &SetupCtx) -> Box<dyn KernelState> {
                Box::new(VanishingState)
            }
            fn rate(&self, iters: u64, threads: usize, secs: f64) -> f64 {
                iters as f64 * threads as f64 / secs
            }
        }

        let kernels: Vec<Box<dyn Kernel>> = vec![
            Box::new(Vanishing),
            Box::new(Busy {
                id: "busy",
                scaling: Scaling::Scales,
            }),
        ];
        let report = run(&kernels, quick(), "test", &Silent);

        assert_eq!(report.workloads.len(), 2);
        assert!(report.workloads[0].error.is_some());
        assert!(report.workloads[0].single_thread.is_none());
        assert!(
            report.workloads[1].single_thread.is_some(),
            "a later workload must still run"
        );
        assert!(
            report.score.single_core.is_some(),
            "scoring must proceed on the workloads that succeeded"
        );
    }

    #[test]
    fn disabling_a_pass_omits_it() {
        let kernels: Vec<Box<dyn Kernel>> = vec![Box::new(Busy {
            id: "busy",
            scaling: Scaling::Scales,
        })];
        let cfg = SuiteConfig {
            multi_thread: false,
            ..quick()
        };
        let report = run(&kernels, cfg, "test", &Silent);
        assert!(report.workloads[0].single_thread.is_some());
        assert!(report.workloads[0].multi_thread.is_none());
        assert!(report.score.multi_core.is_none());
    }

    #[test]
    fn a_workload_that_runs_nothing_says_why() {
        // `--multi-only` against a single-thread-only workload: nothing failed,
        // but nothing ran. Without an explanation the entry is indistinguishable
        // from a silent breakage, and `threadstone verify` rejects the file.
        let kernels: Vec<Box<dyn Kernel>> = vec![Box::new(Busy {
            id: "st-only",
            scaling: Scaling::SingleThreadOnly,
        })];
        let cfg = SuiteConfig {
            single_thread: false,
            ..quick()
        };
        let report = run(&kernels, cfg, "test", &Silent);
        let w = &report.workloads[0];

        assert!(w.single_thread.is_none());
        assert!(w.multi_thread.is_none());
        let error = w.error.as_deref().expect("must explain why nothing ran");
        assert!(error.contains("single-threaded only"), "got: {error}");
    }

    #[test]
    fn disabling_both_passes_is_explained_too() {
        let kernels: Vec<Box<dyn Kernel>> = vec![Box::new(Busy {
            id: "busy",
            scaling: Scaling::Scales,
        })];
        let cfg = SuiteConfig {
            single_thread: false,
            multi_thread: false,
            ..quick()
        };
        let report = run(&kernels, cfg, "test", &Silent);
        assert!(report.workloads[0].error.is_some());
    }

    #[test]
    fn report_serialises_and_round_trips() {
        let kernels: Vec<Box<dyn Kernel>> = vec![Box::new(Busy {
            id: "busy",
            scaling: Scaling::Scales,
        })];
        let report = run(&kernels, quick(), "test", &Silent);
        let json = serde_json::to_string(&report).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back.workloads.len(), report.workloads.len());
        assert_eq!(back.tool_version, "test");
        // Canonical bytes must be stable across a round trip, or signatures
        // written by one process could not be verified by another.
        assert_eq!(
            back.signing_bytes().unwrap(),
            report.signing_bytes().unwrap()
        );
    }
}
