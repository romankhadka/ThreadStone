//! Sort — branch prediction and irregular memory access.
//!
//! Sorts 1 Mi pseudo-random `u64` values with Rust's `sort_unstable`, a
//! pattern-defeating quicksort. Unlike everything else in the suite, this is a
//! *realistic* workload: unpredictable branches at every comparison, a
//! recursive access pattern that walks memory in a way no prefetcher models,
//! and a working set that spills out of L2. It is the closest thing here to
//! what ordinary application code does to a CPU.
//!
//! # The refill, and why it is inside the timed region
//!
//! Sorting an already-sorted array is a different problem: pdqsort detects the
//! existing order and finishes in a fraction of the time. So each iteration
//! must start from unsorted input, which means copying a pristine array over
//! the working one before every sort.
//!
//! That copy is inside the measurement window, and the reported rate therefore
//! covers refill-plus-sort rather than sort alone. This is deliberate.
//! Excluding it would mean either stopping and restarting the clock around
//! every iteration — adding timer overhead to a loop that runs thousands of
//! times — or timing an increasingly-sorted array, which measures nothing. The
//! copy is a sequential 8 MiB `memcpy` against a sort that performs roughly 20
//! million comparisons, so it accounts for well under 1% of the window; the
//! figure is a sort rate to within its own run-to-run variance.
//!
//! # Sizing
//!
//! 1 Mi elements is 8 MiB, larger than L2 on most cores and comparable to the
//! last-level cache, so the sort's later merge passes genuinely touch memory.
//! Small enough that a full multi-threaded run stays under 128 MiB.

use threadstone_core::kernel::{
    Footprint, Kernel, KernelInfo, KernelState, Scaling, SetupCtx, Unit,
};

use crate::rng::Rng;

/// Elements sorted per iteration.
const ELEMENTS: usize = 1 << 20;

/// One thread's pristine input and its scratch buffer.
struct Sort {
    /// Unsorted reference data, never modified.
    pristine: Vec<u64>,
    /// Working buffer, refilled from `pristine` before each sort.
    scratch: Vec<u64>,
}

impl Sort {
    fn new(seed: u64) -> Sort {
        let mut rng = Rng::new(seed);
        let pristine: Vec<u64> = (0..ELEMENTS).map(|_| rng.next_u64()).collect();
        Sort {
            scratch: pristine.clone(),
            pristine,
        }
    }
}

impl KernelState for Sort {
    fn run(&mut self, iters: u64) -> u64 {
        let mut checksum = 0u64;
        for _ in 0..iters {
            self.scratch.copy_from_slice(&self.pristine);
            self.scratch.sort_unstable();
            // Reading from both ends of the sorted result makes the sort
            // observable, so it cannot be eliminated.
            checksum = checksum
                .wrapping_add(self.scratch[0])
                .wrapping_add(self.scratch[ELEMENTS - 1]);
        }
        checksum
    }
}

/// The sort workload.
pub struct SortKernel;

impl Kernel for SortKernel {
    fn info(&self) -> KernelInfo {
        KernelInfo {
            id: "sort",
            name: "Sort 1Mi u64",
            summary: "Pattern-defeating quicksort: branch mispredicts and irregular access",
            unit: Unit::MelemPerSec,
            footprint: Footprint::PerThread,
            scaling: Scaling::Scales,
            // Roughly 20 comparisons per element, and pdqsort's branchless
            // partitioning retires them at a little over one cycle each, so a
            // 3 GHz core sorts about 50 million elements per second.
            reference: 50.0,
        }
    }

    fn setup(&self, ctx: &SetupCtx) -> Box<dyn KernelState> {
        Box::new(Sort::new(0x503D_A17A ^ ctx.thread_index as u64))
    }

    fn rate(&self, iters_per_thread: u64, threads: usize, secs: f64) -> f64 {
        let elements = iters_per_thread as f64 * threads as f64 * ELEMENTS as f64;
        elements / secs / 1e6
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_sorted() {
        let mut s = Sort::new(1);
        s.run(1);
        assert!(
            s.scratch.windows(2).all(|w| w[0] <= w[1]),
            "result must be in ascending order"
        );
    }

    #[test]
    fn sorting_preserves_the_multiset() {
        let mut s = Sort::new(2);
        s.run(1);
        let mut expected = s.pristine.clone();
        expected.sort_unstable();
        assert_eq!(s.scratch, expected, "elements must be preserved exactly");
    }

    #[test]
    fn input_is_not_already_sorted() {
        // If the generator produced ordered data, pdqsort's pattern detection
        // would make this workload measure almost nothing.
        let s = Sort::new(3);
        let ordered = s.pristine.windows(2).filter(|w| w[0] <= w[1]).count();
        assert!(
            ordered < s.pristine.len() * 6 / 10,
            "{ordered} of {} pairs are ordered; the input looks pre-sorted",
            s.pristine.len() - 1
        );
    }

    #[test]
    fn each_iteration_starts_from_unsorted_input() {
        // The refill is what keeps every iteration doing the same work. Without
        // it, iteration two would sort an already-sorted array and finish in a
        // fraction of the time, so the measured rate would climb with the
        // iteration count instead of describing the machine.
        let mut s = Sort::new(4);
        let first = s.run(1);
        let second = s.run(1);
        assert_eq!(second, first, "each iteration must do identical work");

        // Two iterations in one call must therefore total exactly twice one.
        let mut fresh = Sort::new(4);
        assert_eq!(fresh.run(2), first.wrapping_mul(2));
    }

    #[test]
    fn pristine_data_is_never_modified() {
        let mut s = Sort::new(5);
        let before = s.pristine.clone();
        s.run(3);
        assert_eq!(s.pristine, before);
    }

    #[test]
    fn threads_get_different_data() {
        let a = Sort::new(0x1);
        let b = Sort::new(0x2);
        assert_ne!(a.pristine, b.pristine);
    }

    #[test]
    fn rate_converts_to_millions_of_elements() {
        let k = SortKernel;
        // One pass over 1 Mi elements in one second.
        assert!((k.rate(1, 1, 1.0) - ELEMENTS as f64 / 1e6).abs() < 1e-9);
        assert!((k.rate(1, 1, 1.0) - 1.048_576).abs() < 1e-6);
        // Independent per-thread arrays, so throughput adds.
        assert!((k.rate(1, 4, 1.0) - 4.0 * k.rate(1, 1, 1.0)).abs() < 1e-9);
    }
}
