//! The contract every workload implements.
//!
//! Splitting the workload into a stateless [`Kernel`] and a per-thread
//! [`KernelState`] is what makes honest multi-thread measurement possible.
//! State is allocated once per thread, *before* the clock starts, so
//! allocation and page-faulting never land inside a measurement window. The
//! kernel itself is `Sync` and shared; nothing is contended at run time.
//!
//! Both traits are object-safe, so the CLI can hold a registry of
//! `Box<dyn Kernel>` without generics leaking through the whole program. The
//! cost is one virtual call per measurement window — the runner calibrates
//! windows to hundreds of milliseconds, so that call is unmeasurable.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a workload's reported number means.
///
/// Carrying the direction alongside the unit is what lets the scorer combine
/// latency (lower is better) with throughput (higher is better) without any
/// per-workload special-casing at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// Dhrystones per second.
    DhrystonesPerSec,
    /// Billions of floating-point operations per second.
    Gflops,
    /// Gibibytes per second (2^30 bytes), as STREAM has always reported.
    GibPerSec,
    /// Mebibytes per second (2^20 bytes).
    MibPerSec,
    /// Millions of elements per second.
    MelemPerSec,
    /// Nanoseconds per operation.
    Nanoseconds,
}

impl Unit {
    /// Short label for tables and axis titles.
    pub fn label(self) -> &'static str {
        match self {
            Unit::DhrystonesPerSec => "Dhry/s",
            Unit::Gflops => "GFLOP/s",
            Unit::GibPerSec => "GiB/s",
            Unit::MibPerSec => "MiB/s",
            Unit::MelemPerSec => "Melem/s",
            Unit::Nanoseconds => "ns",
        }
    }

    /// Whether a larger value indicates better performance.
    pub fn higher_is_better(self) -> bool {
        !matches!(self, Unit::Nanoseconds)
    }
}

/// How a workload's working set relates to the thread count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Footprint {
    /// Every thread allocates the full working set. Total memory scales with
    /// thread count. Correct for compute-bound kernels whose data fits in
    /// private cache.
    PerThread,
    /// One logical working set is partitioned across threads, so total memory
    /// is constant. Correct for bandwidth kernels, where growing the footprint
    /// with the thread count would change what is being measured.
    Partitioned,
}

/// Whether a workload's multi-thread number means anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scaling {
    /// Meaningful at any thread count; contributes to the multi-core score.
    Scales,
    /// Only meaningful on one thread, and excluded from the multi-core score.
    ///
    /// Memory *latency* is the motivating case. Splitting a 256 MiB chase
    /// buffer across sixteen threads gives each a 16 MiB slice that fits in
    /// last-level cache, so the "multi-threaded latency" number would measure
    /// cache hits and look several times better than reality. Reporting one
    /// honest single-thread figure beats reporting a flattering wrong one.
    SingleThreadOnly,
}

/// Static description of a workload.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KernelInfo {
    /// Stable machine-readable identifier, e.g. `"sgemm"`.
    pub id: &'static str,
    /// Human-readable name for display.
    pub name: &'static str,
    /// One line on what CPU capability this stresses.
    pub summary: &'static str,
    /// What the reported number means.
    pub unit: Unit,
    /// How the working set relates to thread count.
    pub footprint: Footprint,
    /// Whether the multi-thread figure is meaningful.
    pub scaling: Scaling,
    /// Reference value on the ThreadStone Reference Core, in `unit`.
    ///
    /// See `score.rs` for what the reference core is and why these numbers are
    /// what they are.
    pub reference: f64,
}

/// Everything a kernel needs to size itself for one thread.
#[derive(Debug, Clone, Copy)]
pub struct SetupCtx {
    /// Total number of threads participating in this run.
    pub threads: usize,
    /// Index of the thread being set up, in `0..threads`.
    pub thread_index: usize,
}

impl SetupCtx {
    /// Split `total` units of work across threads, giving the remainder to the
    /// lowest-indexed threads so the parts always sum to exactly `total`.
    ///
    /// Used by [`Footprint::Partitioned`] kernels to size their per-thread
    /// slice. Returns at least 1 so an over-subscribed run cannot hand a
    /// thread a zero-length buffer.
    pub fn share(&self, total: usize) -> usize {
        let base = total / self.threads;
        let extra = usize::from(self.thread_index < total % self.threads);
        (base + extra).max(1)
    }
}

/// Per-thread mutable state and the measured inner loop.
pub trait KernelState: Send {
    /// Perform exactly `iters` units of work and return a checksum.
    ///
    /// The runner passes the returned checksum through [`std::hint::black_box`],
    /// which is what stops LLVM from deleting the entire loop as dead code.
    /// Implementations must derive the checksum from real computed results, not
    /// from the loop counter — a checksum of `iters` is not a data dependency
    /// and will not keep the work alive.
    fn run(&mut self, iters: u64) -> u64;
}

/// A workload: stateless, shared across threads, and able to describe itself.
pub trait Kernel: Send + Sync {
    /// Static metadata for this workload.
    fn info(&self) -> KernelInfo;

    /// Allocate this thread's state. Called outside every measurement window.
    fn setup(&self, ctx: &SetupCtx) -> Box<dyn KernelState>;

    /// Convert completed work into the reported rate.
    ///
    /// `iters_per_thread` is what each thread performed; `threads` is how many
    /// did so concurrently; `secs` is the wall-clock span in which they all
    /// ran, having started together.
    ///
    /// Both counts are passed separately rather than pre-multiplied because
    /// the correct combination depends on the footprint. A
    /// [`Footprint::PerThread`] kernel did `iters_per_thread × threads` units
    /// of work. A [`Footprint::Partitioned`] kernel did `iters_per_thread`
    /// passes over one logical working set, and multiplying by `threads` would
    /// overstate it by exactly that factor. And a [`Unit::Nanoseconds`] kernel
    /// wants `secs / iters_per_thread`, since concurrent accesses do not make
    /// any individual access faster.
    fn rate(&self, iters_per_thread: u64, threads: usize, secs: f64) -> f64;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(thread_index: usize, threads: usize) -> SetupCtx {
        SetupCtx {
            threads,
            thread_index,
        }
    }

    #[test]
    fn shares_sum_to_the_total() {
        for threads in 1..=16 {
            for total in [0, 1, 7, 100, 1_000_003] {
                let sum: usize = (0..threads).map(|i| ctx(i, threads).share(total)).sum();
                if total >= threads {
                    assert_eq!(sum, total, "threads={threads} total={total}");
                }
            }
        }
    }

    #[test]
    fn remainder_goes_to_low_indices() {
        // 10 units across 4 threads: 3, 3, 2, 2.
        let shares: Vec<usize> = (0..4).map(|i| ctx(i, 4).share(10)).collect();
        assert_eq!(shares, vec![3, 3, 2, 2]);
    }

    #[test]
    fn share_never_returns_zero() {
        // More threads than work: every thread still gets a usable buffer.
        assert_eq!(ctx(15, 16).share(2), 1);
        assert_eq!(ctx(0, 8).share(0), 1);
    }

    #[test]
    fn latency_is_the_only_lower_is_better_unit() {
        assert!(!Unit::Nanoseconds.higher_is_better());
        for u in [
            Unit::DhrystonesPerSec,
            Unit::Gflops,
            Unit::GibPerSec,
            Unit::MibPerSec,
            Unit::MelemPerSec,
        ] {
            assert!(u.higher_is_better(), "{u:?} should be higher-is-better");
        }
    }
}
