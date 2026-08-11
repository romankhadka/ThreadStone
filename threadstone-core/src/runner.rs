//! Executes a kernel and turns it into a measured result.
//!
//! # Why this is not just `par_iter().map(run)`
//!
//! The obvious way to benchmark N threads is to hand N samples to a work-
//! stealing pool. That measures the wrong thing, in three separate ways:
//!
//! 1. **Threads do not start together.** A pool spawns work as slots free up,
//!    so early threads run against an idle machine and late threads run against
//!    a loaded one. The samples are drawn from different conditions and
//!    averaging them is meaningless.
//! 2. **Work stealing perturbs the measurement.** The scheduler's own load
//!    balancing is inside the timed region.
//! 3. **Per-sample setup lands in the window.** Allocating and first-touching a
//!    working set is often slower than the kernel itself.
//!
//! This runner fixes all three. Threads are spawned once, allocate their state
//! once, and then execute rounds in lockstep: a barrier releases every thread
//! simultaneously, and a second barrier collects them. The measured window is
//! the span between those barriers, so it is exactly "time for all N threads to
//! complete their work, having started at the same instant".
//!
//! # Calibration
//!
//! Fixed iteration counts are wrong across a 100× spread of machine speeds: a
//! count that takes 300 ms on a laptop takes 3 ms on a server, and 3 ms is
//! close enough to the clock's resolution to be noise. So the iteration count
//! is discovered at run time, and — critically — discovered *with all threads
//! running*, because a count calibrated on an idle machine will overshoot
//! wildly once memory bandwidth is contended.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Barrier;
use std::time::{Duration, Instant};

use crate::kernel::{Kernel, SetupCtx, Unit};
use crate::stats::Summary;

/// Runner defaults, chosen to be trustworthy rather than fast.
pub mod defaults {
    use std::time::Duration;

    /// Target duration of one measurement window.
    ///
    /// 250 ms is roughly 6 million times the ~40 ns clock granularity on Apple
    /// silicon, so quantisation contributes under one part per million, while
    /// still being short enough that a six-workload suite finishes in about a
    /// minute.
    pub const WINDOW: Duration = Duration::from_millis(250);

    /// A window shorter than this is reported as untrustworthy.
    pub const MIN_WINDOW: Duration = Duration::from_millis(20);

    /// Discarded rounds before measurement begins.
    ///
    /// Two is enough to fault in the working set, fill the branch predictors,
    /// and let the CPU reach its boost frequency.
    pub const WARMUP: u32 = 2;

    /// Measured rounds.
    ///
    /// Seven is the smallest odd count that gives the MAD outlier filter enough
    /// points to work with while keeping total suite time reasonable.
    pub const SAMPLES: u32 = 7;
}

/// How a run should be executed.
#[derive(Debug, Clone, Copy)]
pub struct RunConfig {
    /// Number of OS threads. Must be at least 1.
    pub threads: usize,
    /// Measured rounds to collect.
    pub samples: u32,
    /// Rounds to discard before measuring.
    pub warmup: u32,
    /// Duration each round should aim for.
    pub window: Duration,
}

impl Default for RunConfig {
    fn default() -> Self {
        RunConfig {
            threads: 1,
            samples: defaults::SAMPLES,
            warmup: defaults::WARMUP,
            window: defaults::WINDOW,
        }
    }
}

impl RunConfig {
    /// Config for a single-threaded run.
    pub fn single_thread() -> Self {
        RunConfig::default()
    }

    /// Same config at a different thread count.
    pub fn with_threads(self, threads: usize) -> Self {
        RunConfig {
            threads: threads.max(1),
            ..self
        }
    }
}

/// A completed measurement of one kernel at one thread count.
#[derive(Debug, Clone)]
pub struct Measurement {
    /// Kernel identifier.
    pub id: &'static str,
    /// Unit of `samples` and of `summary`.
    pub unit: Unit,
    /// Threads used.
    pub threads: usize,
    /// Work units each thread performed per round, as calibrated.
    pub iters_per_thread: u64,
    /// Per-round rates, in `unit`, in collection order.
    pub samples: Vec<f64>,
    /// Robust summary of `samples`.
    pub summary: Summary,
    /// Median measurement window, in milliseconds.
    pub window_ms: f64,
    /// Set when the calibrated window stayed under [`defaults::MIN_WINDOW`],
    /// meaning clock granularity is a material part of the reading.
    pub window_too_short: bool,
}

impl Measurement {
    /// The figure to quote for this measurement.
    pub fn value(&self) -> f64 {
        self.summary.median
    }
}

/// Why a run could not produce a measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// `threads` was zero.
    ZeroThreads,
    /// `samples` was zero.
    ZeroSamples,
    /// Calibration could not find an iteration count reaching the target
    /// window without overflowing `u64`. In practice this means the kernel's
    /// inner loop was optimised away to nothing.
    CalibrationFailed {
        /// Identifier of the kernel that failed.
        id: &'static str,
    },
    /// Every sample was non-finite; no summary could be formed.
    NoValidSamples {
        /// Identifier of the kernel that failed.
        id: &'static str,
    },
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::ZeroThreads => write!(f, "thread count must be at least 1"),
            RunError::ZeroSamples => write!(f, "sample count must be at least 1"),
            RunError::CalibrationFailed { id } => write!(
                f,
                "workload '{id}': could not calibrate an iteration count; \
                 the inner loop may have been optimised away"
            ),
            RunError::NoValidSamples { id } => {
                write!(f, "workload '{id}': every sample was non-finite")
            }
        }
    }
}

impl std::error::Error for RunError {}

/// Receives progress events during a run. Every method has a no-op default, so
/// implementors override only what they display.
pub trait Observer: Sync {
    /// Calibration for `id` has begun at `threads` threads.
    fn calibrating(&self, id: &str, threads: usize) {
        let _ = (id, threads);
    }
    /// Calibration settled on `iters` per thread, giving a `window_ms` round.
    fn calibrated(&self, id: &str, iters: u64, window_ms: f64) {
        let _ = (id, iters, window_ms);
    }
    /// Round `index` of `total` completed, yielding `rate`.
    fn sample(&self, id: &str, index: u32, total: u32, rate: f64) {
        let _ = (id, index, total, rate);
    }
    /// The kernel finished; `measurement` is its result.
    fn finished(&self, id: &str, measurement: &Measurement) {
        let _ = (id, measurement);
    }
}

/// An [`Observer`] that reports nothing.
pub struct SilentObserver;
impl Observer for SilentObserver {}

/// Shared control block. The main thread writes; workers read.
struct Control {
    /// Iterations each worker should perform in the next round.
    iters: AtomicU64,
    /// Set once to tell workers to exit instead of running another round.
    stop: AtomicBool,
    /// Accumulates every worker's checksum so the optimiser cannot prove the
    /// results unused across the whole program.
    checksum: AtomicU64,
}

/// Run `kernel` under `cfg`, reporting progress to `obs`.
///
/// Spawns `cfg.threads` scoped threads that live for the whole run: state is
/// allocated once, calibration and every round execute in lockstep, and the
/// threads are torn down only at the end.
/// `obs` is generic rather than `&dyn Observer` so that a supertrait object
/// such as `&dyn SuiteObserver` can be passed directly. Trait upcasting only
/// stabilised in Rust 1.86, and this crate supports older toolchains.
pub fn run<O: Observer + ?Sized>(
    kernel: &dyn Kernel,
    cfg: RunConfig,
    obs: &O,
) -> Result<Measurement, RunError> {
    if cfg.threads == 0 {
        return Err(RunError::ZeroThreads);
    }
    if cfg.samples == 0 {
        return Err(RunError::ZeroSamples);
    }

    let info = kernel.info();
    let threads = cfg.threads;
    let total_rounds = cfg.warmup + cfg.samples;

    let control = Control {
        iters: AtomicU64::new(1),
        stop: AtomicBool::new(false),
        checksum: AtomicU64::new(0),
    };
    // `threads` workers plus the coordinating main thread.
    let gate = Barrier::new(threads + 1);

    let mut calibrated_iters = 1u64;
    let mut window_samples: Vec<f64> = Vec::with_capacity(total_rounds as usize);
    let mut rates: Vec<f64> = Vec::with_capacity(cfg.samples as usize);
    let mut calibration_failed = false;

    std::thread::scope(|scope| {
        for thread_index in 0..threads {
            let control = &control;
            let gate = &gate;
            scope.spawn(move || {
                // Allocation and first-touch happen here, outside every window.
                let ctx = SetupCtx {
                    threads,
                    thread_index,
                };
                let mut state = kernel.setup(&ctx);

                loop {
                    // Round start: release together with every other worker.
                    gate.wait();
                    if control.stop.load(Ordering::Acquire) {
                        break;
                    }
                    let iters = control.iters.load(Ordering::Acquire);
                    let sum = state.run(iters);
                    // Publishing the checksum creates a program-visible data
                    // dependency on the kernel's output, which is what keeps
                    // the work from being eliminated.
                    control.checksum.fetch_xor(sum, Ordering::Relaxed);
                    // Round end: the main thread's timer stops when the last
                    // worker reaches here.
                    gate.wait();
                }
            });
        }

        // ---- Calibration -------------------------------------------------
        obs.calibrating(info.id, threads);
        match calibrate(&control, &gate, cfg.window) {
            Some(iters) => calibrated_iters = iters,
            None => {
                calibration_failed = true;
                control.stop.store(true, Ordering::Release);
                gate.wait(); // release workers so they observe `stop` and exit
                return;
            }
        }

        // ---- Warmup and measurement --------------------------------------
        control.iters.store(calibrated_iters, Ordering::Release);

        for round in 0..total_rounds {
            let secs = timed_round(&gate);
            if round == 0 {
                obs.calibrated(info.id, calibrated_iters, secs * 1e3);
            }
            window_samples.push(secs * 1e3);
            if round >= cfg.warmup {
                let rate = kernel.rate(calibrated_iters, threads, secs);
                let index = round - cfg.warmup + 1;
                obs.sample(info.id, index, cfg.samples, rate);
                rates.push(rate);
            }
        }

        // ---- Shutdown ----------------------------------------------------
        control.stop.store(true, Ordering::Release);
        gate.wait();
    });

    if calibration_failed {
        return Err(RunError::CalibrationFailed { id: info.id });
    }

    let summary = Summary::new(&rates).ok_or(RunError::NoValidSamples { id: info.id })?;
    let window_ms = Summary::new(&window_samples).map_or(0.0, |s| s.median);

    let measurement = Measurement {
        id: info.id,
        unit: info.unit,
        threads,
        iters_per_thread: calibrated_iters,
        samples: rates,
        summary,
        window_ms,
        window_too_short: window_ms < defaults::MIN_WINDOW.as_secs_f64() * 1e3,
    };
    obs.finished(info.id, &measurement);
    Ok(measurement)
}

/// Execute one lockstep round and return its wall-clock duration in seconds.
///
/// The timer starts the instant every worker has been released and stops when
/// the last one reports back, so the span covers exactly the concurrent work.
fn timed_round(gate: &Barrier) -> f64 {
    gate.wait();
    let start = Instant::now();
    gate.wait();
    start.elapsed().as_secs_f64()
}

/// Discover an iteration count whose round lands near `target`.
///
/// Runs real rounds with every thread active, so the count accounts for
/// whatever contention the thread count produces. Returns `None` if no count
/// below `u64::MAX` reaches the target — which in practice means the kernel
/// compiled down to nothing.
fn calibrate(control: &Control, gate: &Barrier, target: Duration) -> Option<u64> {
    /// Stop once a round reaches this fraction of the target. Overshooting
    /// slightly is harmless; undershooting costs precision.
    const ACCEPT: f64 = 0.9;
    /// Never grow the count by more than this in one step. Bounds the damage
    /// from a round that was anomalously fast because of a scheduling artefact.
    const MAX_GROWTH: f64 = 20.0;
    /// Enough steps to climb from 1 to `u64::MAX` at the growth cap, with room
    /// to spare for refinement steps near the target.
    const MAX_STEPS: u32 = 48;

    let target_secs = target.as_secs_f64();
    let mut iters: u64 = 1;

    for _ in 0..MAX_STEPS {
        control.iters.store(iters, Ordering::Release);
        let secs = timed_round(gate);

        if secs >= target_secs * ACCEPT {
            return Some(iters);
        }

        // Below the clock's useful range the measured ratio is noise, so climb
        // by a fixed large factor instead of trusting it.
        let growth = if secs < 1e-4 {
            MAX_GROWTH
        } else {
            (target_secs / secs).clamp(1.5, MAX_GROWTH)
        };

        let next = (iters as f64 * growth).ceil();
        if !next.is_finite() || next >= u64::MAX as f64 {
            return None;
        }
        // `growth >= 1.5` guarantees forward progress for iters >= 1.
        iters = next as u64;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{Footprint, KernelInfo, KernelState, Scaling};
    use std::sync::atomic::AtomicUsize;

    /// A kernel that burns a predictable amount of time per iteration.
    struct Spin;

    struct SpinState {
        acc: u64,
    }

    impl KernelState for SpinState {
        fn run(&mut self, iters: u64) -> u64 {
            let mut acc = self.acc;
            for _ in 0..iters {
                for _ in 0..64 {
                    acc = acc.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                }
            }
            self.acc = acc;
            acc
        }
    }

    impl Kernel for Spin {
        fn info(&self) -> KernelInfo {
            KernelInfo {
                id: "spin",
                name: "Spin",
                summary: "test kernel",
                unit: Unit::MelemPerSec,
                footprint: Footprint::PerThread,
                scaling: Scaling::Scales,
                reference: 1.0,
            }
        }
        fn setup(&self, _ctx: &SetupCtx) -> Box<dyn KernelState> {
            Box::new(SpinState { acc: 1 })
        }
        fn rate(&self, iters: u64, threads: usize, secs: f64) -> f64 {
            iters as f64 * threads as f64 / secs / 1e6
        }
    }

    /// Records how many distinct threads called `setup`.
    struct CountingKernel {
        setups: AtomicUsize,
    }

    impl Kernel for CountingKernel {
        fn info(&self) -> KernelInfo {
            KernelInfo {
                id: "counting",
                name: "Counting",
                summary: "test kernel",
                unit: Unit::MelemPerSec,
                footprint: Footprint::Partitioned,
                scaling: Scaling::Scales,
                reference: 1.0,
            }
        }
        fn setup(&self, _ctx: &SetupCtx) -> Box<dyn KernelState> {
            self.setups.fetch_add(1, Ordering::Relaxed);
            Box::new(SpinState { acc: 1 })
        }
        fn rate(&self, iters: u64, threads: usize, secs: f64) -> f64 {
            iters as f64 * threads as f64 / secs / 1e6
        }
    }

    /// Short config so the test suite stays fast.
    fn quick(threads: usize) -> RunConfig {
        RunConfig {
            threads,
            samples: 3,
            warmup: 1,
            window: Duration::from_millis(20),
        }
    }

    #[test]
    fn rejects_zero_threads() {
        let k = Spin;
        let err = run(&k, quick(0), &SilentObserver).unwrap_err();
        assert_eq!(err, RunError::ZeroThreads);
    }

    #[test]
    fn rejects_zero_samples() {
        let k = Spin;
        let cfg = RunConfig {
            samples: 0,
            ..quick(1)
        };
        let err = run(&k, cfg, &SilentObserver).unwrap_err();
        assert_eq!(err, RunError::ZeroSamples);
    }

    #[test]
    fn single_thread_run_produces_expected_shape() {
        let k = Spin;
        let m = run(&k, quick(1), &SilentObserver).unwrap();
        assert_eq!(m.threads, 1);
        assert_eq!(m.samples.len(), 3, "warmup rounds must not be reported");
        assert_eq!(m.summary.n + m.summary.outliers, 3);
        assert!(m.iters_per_thread >= 1);
        assert!(m.value() > 0.0);
    }

    #[test]
    fn calibration_reaches_the_target_window() {
        let k = Spin;
        let cfg = RunConfig {
            window: Duration::from_millis(60),
            ..quick(1)
        };
        let m = run(&k, cfg, &SilentObserver).unwrap();
        // Allow generous slack for a loaded CI machine, but the window must be
        // in the right order of magnitude rather than microseconds.
        assert!(
            m.window_ms > 30.0,
            "calibrated window {}ms far below the 60ms target",
            m.window_ms
        );
    }

    #[test]
    fn every_thread_gets_its_own_state() {
        let k = CountingKernel {
            setups: AtomicUsize::new(0),
        };
        run(&k, quick(4), &SilentObserver).unwrap();
        assert_eq!(
            k.setups.load(Ordering::Relaxed),
            4,
            "setup must run once per thread, not once per sample"
        );
    }

    #[test]
    fn multi_thread_run_completes_and_scales_iterations() {
        let k = Spin;
        let m1 = run(&k, quick(1), &SilentObserver).unwrap();
        let m4 = run(&k, quick(4), &SilentObserver).unwrap();
        assert_eq!(m4.threads, 4);
        // Four threads of a compute-bound kernel should beat one thread. Two
        // is a deliberately loose floor so the test survives a busy CI box.
        assert!(
            m4.value() > m1.value() * 1.2,
            "4 threads ({:.2}) barely beat 1 thread ({:.2})",
            m4.value(),
            m1.value()
        );
    }

    #[test]
    fn observer_sees_one_event_per_measured_sample() {
        struct Counting {
            samples: AtomicUsize,
            finished: AtomicUsize,
        }
        impl Observer for Counting {
            fn sample(&self, _id: &str, _i: u32, _n: u32, _r: f64) {
                self.samples.fetch_add(1, Ordering::Relaxed);
            }
            fn finished(&self, _id: &str, _m: &Measurement) {
                self.finished.fetch_add(1, Ordering::Relaxed);
            }
        }
        let obs = Counting {
            samples: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
        };
        let k = Spin;
        run(&k, quick(1), &obs).unwrap();
        assert_eq!(obs.samples.load(Ordering::Relaxed), 3);
        assert_eq!(obs.finished.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn empty_kernel_fails_calibration_instead_of_hanging() {
        struct Empty;
        struct EmptyState;
        impl KernelState for EmptyState {
            fn run(&mut self, _iters: u64) -> u64 {
                // Returns instantly regardless of `iters`: no count will ever
                // fill the window.
                0
            }
        }
        impl Kernel for Empty {
            fn info(&self) -> KernelInfo {
                KernelInfo {
                    id: "empty",
                    name: "Empty",
                    summary: "test kernel",
                    unit: Unit::MelemPerSec,
                    footprint: Footprint::PerThread,
                    scaling: Scaling::Scales,
                    reference: 1.0,
                }
            }
            fn setup(&self, _ctx: &SetupCtx) -> Box<dyn KernelState> {
                Box::new(EmptyState)
            }
            fn rate(&self, iters: u64, threads: usize, secs: f64) -> f64 {
                iters as f64 * threads as f64 / secs
            }
        }
        let err = run(&Empty, quick(1), &SilentObserver).unwrap_err();
        assert_eq!(err, RunError::CalibrationFailed { id: "empty" });
    }
}
