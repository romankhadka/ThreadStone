//! STREAM Triad — sustained main-memory bandwidth.
//!
//! Computes `a[i] = b[i] + scalar · c[i]` over arrays far larger than any
//! cache, following John McCalpin's STREAM benchmark. Triad is the most
//! demanding of the four STREAM kernels — two reads and one write per element,
//! with just enough arithmetic that a compiler cannot turn it into a `memcpy` —
//! so it is the one reported here.
//!
//! # Sizing
//!
//! STREAM's rule is that each array must be at least four times the last-level
//! cache, otherwise the benchmark measures cache bandwidth and reports a number
//! several times too high. Each array here is 64 MiB (192 MiB across all
//! three), which clears that bar on every consumer CPU and most servers.
//!
//! The total is fixed regardless of thread count: the arrays are *partitioned*
//! across threads, not replicated. Replicating would grow the footprint with
//! the thread count and change what is being measured partway up the scaling
//! curve. Partitioning also gives correct NUMA placement for free, since each
//! thread allocates and first-touches its own slice, so the pages land in the
//! memory attached to the socket that will read them.
//!
//! # Byte accounting
//!
//! Each element moves 24 bytes: two 8-byte reads and one 8-byte write. This
//! follows STREAM's convention of ignoring the read-for-ownership traffic that
//! a write to a non-resident cache line actually generates on most
//! architectures. Real DRAM traffic is therefore up to a third higher than the
//! reported figure. The convention is kept because every published STREAM
//! number uses it, and a differently-accounted number would not be comparable
//! to any of them.
//!
//! # What went wrong before
//!
//! The previous implementation called Rayon's `par_chunks_mut` *inside* a
//! kernel that the harness was already running under `par_iter`, so N threads
//! each spawned N-way parallel work over their own freshly allocated 384 MiB.
//! It also divided by `1e6` and called the result MB/s while the README called
//! it GB/s.

use threadstone_core::kernel::{
    Footprint, Kernel, KernelInfo, KernelState, Scaling, SetupCtx, Unit,
};

/// The multiplier in the triad expression, from the original STREAM.
const SCALAR: f64 = 3.0;

/// Elements per array, across all threads: 8 Mi × 8 bytes = 64 MiB each.
const TOTAL_ELEMENTS: usize = 8 << 20;

/// Bytes counted as moved per element, per STREAM's convention.
const BYTES_PER_ELEMENT: f64 = 24.0;

/// One thread's slice of the three arrays.
struct Stream {
    a: Vec<f64>,
    b: Vec<f64>,
    c: Vec<f64>,
    /// Elements in this thread's slice.
    len: usize,
}

impl Stream {
    fn new(len: usize) -> Stream {
        // `vec![v; n]` writes every element, which first-touches every page on
        // this thread. Without that, the first measured round would pay the
        // page faults and read low.
        Stream {
            a: vec![1.0; len],
            b: vec![2.0; len],
            c: vec![0.5; len],
            len,
        }
    }

    #[inline]
    fn triad(&mut self) {
        // Equal-length slices let LLVM elide bounds checks and vectorise.
        let a = &mut self.a[..self.len];
        let b = &self.b[..self.len];
        let c = &self.c[..self.len];
        for i in 0..self.len {
            a[i] = b[i] + SCALAR * c[i];
        }
    }
}

impl KernelState for Stream {
    fn run(&mut self, iters: u64) -> u64 {
        for _ in 0..iters {
            self.triad();
        }
        checksum(&self.a)
    }
}

/// Fold eight positions spread across `values` into one number.
///
/// Reading only the two ends would be a weaker guard than it looks. Every
/// element of a triad output holds the same value, so `first ^ last` is
/// identically zero — indistinguishable from a loop that never ran — and it
/// would leave the optimiser free to narrow the triad to just the two elements
/// anyone observes. Spreading the reads and rotating between them costs eight
/// loads and removes both problems.
fn checksum(values: &[f64]) -> u64 {
    let stride = (values.len() / 8).max(1);
    let mut sum = 0u64;
    for i in 0..8 {
        let index = (stride * i).min(values.len() - 1);
        sum = sum.rotate_left(7) ^ values[index].to_bits();
    }
    sum
}

/// The STREAM Triad workload.
pub struct StreamKernel;

impl Kernel for StreamKernel {
    fn info(&self) -> KernelInfo {
        KernelInfo {
            id: "stream",
            name: "STREAM Triad",
            summary: "Sustained memory bandwidth over 192 MiB, far beyond any cache",
            unit: Unit::GibPerSec,
            footprint: Footprint::Partitioned,
            scaling: Scaling::Scales,
            // One DDR4-3200 channel is 25.6 GB/s (23.8 GiB/s) peak. A single
            // core cannot keep enough misses in flight to saturate it and lands
            // around half, which is what the reference describes.
            reference: 12.0,
        }
    }

    fn setup(&self, ctx: &SetupCtx) -> Box<dyn KernelState> {
        Box::new(Stream::new(ctx.share(TOTAL_ELEMENTS)))
    }

    fn rate(&self, iters_per_thread: u64, _threads: usize, secs: f64) -> f64 {
        gib_per_sec(iters_per_thread, secs)
    }
}

/// Bandwidth in GiB/s for `passes` sweeps over the whole array set.
///
/// The thread count does not appear, and that is the point of declaring this
/// kernel [`Footprint::Partitioned`]. In one round each of `T` threads makes
/// `iters_per_thread` passes over its own `TOTAL_ELEMENTS / T` slice, so the
/// round touches `iters_per_thread × TOTAL_ELEMENTS` elements no matter what
/// `T` is. Multiplying by the thread count — which is right for a per-thread
/// kernel like SGEMM — would overstate bandwidth by exactly `T`×.
fn gib_per_sec(passes: u64, secs: f64) -> f64 {
    let elements = passes as f64 * TOTAL_ELEMENTS as f64;
    elements * BYTES_PER_ELEMENT / secs / (1u64 << 30) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triad_computes_the_stream_expression() {
        let mut s = Stream::new(1024);
        s.triad();
        // b = 2.0, c = 0.5, scalar = 3.0, so a should be 2 + 1.5 = 3.5.
        assert!(s.a.iter().all(|v| (v - 3.5).abs() < 1e-12));
    }

    #[test]
    fn triad_is_idempotent() {
        let mut s = Stream::new(256);
        s.triad();
        let first = s.a.clone();
        s.triad();
        assert_eq!(first, s.a, "repeated passes must produce the same output");
    }

    #[test]
    fn slices_partition_the_total_exactly() {
        let k = StreamKernel;
        for threads in [1usize, 2, 3, 7, 14, 64] {
            let total: usize = (0..threads)
                .map(|thread_index| {
                    SetupCtx {
                        threads,
                        thread_index,
                    }
                    .share(TOTAL_ELEMENTS)
                })
                .sum();
            assert_eq!(
                total, TOTAL_ELEMENTS,
                "footprint must stay constant at {threads} threads"
            );
        }
        // The kernel is declared as partitioned, which is what licenses that.
        assert_eq!(k.info().footprint, Footprint::Partitioned);
    }

    #[test]
    fn bandwidth_arithmetic_is_correct() {
        // One pass over the whole 8 Mi-element array in one second.
        let expected = TOTAL_ELEMENTS as f64 * 24.0 / (1u64 << 30) as f64;
        assert!((gib_per_sec(1, 1.0) - expected).abs() < 1e-9);
        // Exactly 192 MiB of traffic, so 0.1875 GiB.
        assert!((gib_per_sec(1, 1.0) - 0.1875).abs() < 1e-12);
        // Twice the passes in the same time is twice the bandwidth.
        assert!((gib_per_sec(2, 1.0) - 2.0 * gib_per_sec(1, 1.0)).abs() < 1e-12);
        // Half the time is twice the bandwidth.
        assert!((gib_per_sec(1, 0.5) - 2.0 * gib_per_sec(1, 1.0)).abs() < 1e-12);
    }

    #[test]
    fn bandwidth_does_not_scale_with_the_thread_count() {
        // The regression this guards: multiplying per-thread passes by the
        // thread count for a partitioned kernel reports T× the real bandwidth.
        let k = StreamKernel;
        let one = k.rate(4, 1, 1.0);
        for threads in [2usize, 8, 14, 64] {
            assert!(
                (k.rate(4, threads, 1.0) - one).abs() < 1e-12,
                "partitioned bandwidth must not depend on thread count"
            );
        }
    }

    #[test]
    fn checksum_is_nonzero_for_a_uniform_result() {
        // A triad output is uniform, so a first-xor-last checksum would be
        // zero and look exactly like a loop that was optimised away.
        let mut s = Stream::new(64);
        assert_ne!(s.run(1), 0);
    }

    #[test]
    fn checksum_tracks_the_data() {
        let mut s = Stream::new(64);
        let before = s.run(1);
        s.b[32] = 100.0;
        s.triad();
        assert_ne!(
            checksum(&s.a),
            before,
            "changing an interior element must change the checksum"
        );
    }

    #[test]
    fn checksum_handles_tiny_slices() {
        assert_ne!(checksum(&[1.0]), 0);
        assert_ne!(checksum(&[1.0, 2.0, 3.0]), 0);
    }

    #[test]
    fn setup_never_produces_an_empty_slice() {
        let k = StreamKernel;
        // Absurd over-subscription must still yield a usable buffer per thread.
        let mut state = k.setup(&SetupCtx {
            threads: TOTAL_ELEMENTS * 2,
            thread_index: TOTAL_ELEMENTS * 2 - 1,
        });
        state.run(1);
    }
}
