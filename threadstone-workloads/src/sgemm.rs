//! SGEMM — dense double-precision matrix multiply.
//!
//! Computes `C += A · B` over square `N × N` matrices, which is the standard
//! probe for sustained floating-point throughput: it has O(N³) arithmetic over
//! O(N²) data, so a well-blocked implementation is limited by the FMA units
//! rather than by memory.
//!
//! # Why this loop order
//!
//! The textbook `i-j-k` order accumulates a dot product in the inner loop,
//! creating a serial dependency through the accumulator that stalls on FMA
//! latency and defeats vectorisation. This uses `i-k-j`, where the inner loop
//! is `C[i][j] += A[i][k] · B[k][j]` with `j` contiguous — a pure AXPY over two
//! sequential rows, which LLVM turns into unrolled FMA vector ops.
//!
//! On top of that, four rows of `C` are updated per pass over a row of `B`, so
//! each `B` load feeds four FMAs instead of one. That raises arithmetic
//! intensity enough to keep the FP pipelines fed rather than the load units.
//!
//! # Why `N = 256`
//!
//! Three 256 × 256 `f64` matrices are 1.5 MiB, which fits comfortably in a
//! modern L2 but not in L1. That is deliberate: a working set inside L1 would
//! measure peak FLOPs with no memory system involvement at all, and one beyond
//! L2 would measure bandwidth, which [`crate::stream`] already covers. `N` is a
//! multiple of 4 so the row blocking divides evenly with no remainder path.

use threadstone_core::kernel::{
    Footprint, Kernel, KernelInfo, KernelState, Scaling, SetupCtx, Unit,
};

use crate::rng::Rng;

/// Matrix dimension. Must stay a multiple of [`ROW_BLOCK`].
const N: usize = 256;

/// Rows of `C` updated per pass over a row of `B`.
///
/// Four keeps the accumulators in registers on both aarch64 (32 vector
/// registers) and x86-64 with AVX (16), while quadrupling reuse of each `B`
/// element.
const ROW_BLOCK: usize = 4;

/// Floating-point operations in one multiply-accumulate pass: one multiply and
/// one add per inner-loop step, over `N³` steps.
const FLOPS_PER_MULTIPLY: f64 = 2.0 * (N as f64) * (N as f64) * (N as f64);

/// One thread's matrices.
struct Sgemm {
    a: Vec<f64>,
    b: Vec<f64>,
    c: Vec<f64>,
}

impl Sgemm {
    fn new(seed: u64) -> Sgemm {
        let mut rng = Rng::new(seed);
        // Values in [-1, 1): centred on zero so repeated accumulation into `C`
        // performs a random walk instead of growing monotonically into the
        // range where doubles lose precision or reach infinity.
        let mut fill = |len: usize| (0..len).map(|_| rng.next_f64() * 2.0 - 1.0).collect();
        Sgemm {
            a: fill(N * N),
            b: fill(N * N),
            c: vec![0.0; N * N],
        }
    }

    /// One `C += A · B`.
    fn multiply(&mut self) {
        for i0 in (0..N).step_by(ROW_BLOCK) {
            // Four disjoint mutable rows of C. `split_at_mut` is what lets the
            // borrow checker see them as non-overlapping.
            let (rows, _) = self.c[i0 * N..].split_at_mut(ROW_BLOCK * N);
            let (c0, rest) = rows.split_at_mut(N);
            let (c1, rest) = rest.split_at_mut(N);
            let (c2, c3) = rest.split_at_mut(N);

            for k in 0..N {
                let a0 = self.a[i0 * N + k];
                let a1 = self.a[(i0 + 1) * N + k];
                let a2 = self.a[(i0 + 2) * N + k];
                let a3 = self.a[(i0 + 3) * N + k];
                let b_row = &self.b[k * N..k * N + N];

                // Slicing to a common length lets LLVM drop the bounds checks
                // and vectorise the body.
                for j in 0..N {
                    let bv = b_row[j];
                    c0[j] += a0 * bv;
                    c1[j] += a1 * bv;
                    c2[j] += a2 * bv;
                    c3[j] += a3 * bv;
                }
            }
        }
    }
}

impl KernelState for Sgemm {
    fn run(&mut self, iters: u64) -> u64 {
        for _ in 0..iters {
            self.multiply();
        }
        // Eight positions spread across C. Enough of a data dependency to keep
        // the multiplies alive and to stop the optimiser narrowing them to the
        // observed elements, without adding an O(N²) reduction to the window.
        let mut sum = 0u64;
        for i in 0..8 {
            sum = sum.rotate_left(7) ^ self.c[i * (N * N / 8)].to_bits();
        }
        sum
    }
}

/// The dense matrix multiply workload.
pub struct SgemmKernel;

impl Kernel for SgemmKernel {
    fn info(&self) -> KernelInfo {
        KernelInfo {
            id: "sgemm",
            name: "SGEMM 256³",
            summary: "Dense f64 matrix multiply: sustained FMA throughput out of L2",
            unit: Unit::Gflops,
            footprint: Footprint::PerThread,
            scaling: Scaling::Scales,
            // A 3 GHz core with 256-bit FMA peaks near 48 GFLOP/s. The
            // reference is what *this* kernel reaches there — around a quarter
            // of peak — not what a hand-tuned BLAS would. A reference that
            // assumed hand-vectorised code would score every machine running
            // this kernel below 1.0 and measure the kernel, not the CPU.
            reference: 12.0,
        }
    }

    fn setup(&self, ctx: &SetupCtx) -> Box<dyn KernelState> {
        // Distinct data per thread, so no two threads share cache lines and the
        // measurement reflects independent compute.
        Box::new(Sgemm::new(0x56EE_0000 ^ ctx.thread_index as u64))
    }

    fn rate(&self, iters_per_thread: u64, threads: usize, secs: f64) -> f64 {
        iters_per_thread as f64 * threads as f64 * FLOPS_PER_MULTIPLY / secs / 1e9
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Textbook triple loop, used only to check the blocked version.
    fn reference_multiply(a: &[f64], b: &[f64], c: &mut [f64]) {
        for i in 0..N {
            for k in 0..N {
                let a_ik = a[i * N + k];
                for j in 0..N {
                    c[i * N + j] += a_ik * b[k * N + j];
                }
            }
        }
    }

    #[test]
    fn blocked_multiply_matches_the_naive_one() {
        let mut s = Sgemm::new(1);
        let mut expected = vec![0.0; N * N];
        reference_multiply(&s.a, &s.b, &mut expected);
        s.multiply();

        for (i, (got, want)) in s.c.iter().zip(&expected).enumerate() {
            // The two orders sum the same terms in the same sequence, so they
            // should agree bit-for-bit; a small tolerance guards against
            // contraction into FMA changing the rounding.
            assert!(
                (got - want).abs() < 1e-9,
                "element {i}: got {got}, want {want}"
            );
        }
    }

    #[test]
    fn multiply_produces_nontrivial_output() {
        let mut s = Sgemm::new(2);
        s.multiply();
        assert!(s.c.iter().all(|v| v.is_finite()), "results must be finite");
        assert!(
            s.c.iter().any(|v| v.abs() > 1e-6),
            "output should not be all zeros"
        );
    }

    #[test]
    fn repeated_multiplies_stay_finite() {
        // 200 accumulations must not drift to infinity, or long runs would
        // start measuring NaN handling instead of arithmetic.
        let mut s = Sgemm::new(3);
        s.run(200);
        assert!(s.c.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn dimensions_divide_the_row_block() {
        assert_eq!(N % ROW_BLOCK, 0, "N must divide evenly by the row block");
    }

    #[test]
    fn rate_converts_to_gflops() {
        let k = SgemmKernel;
        // One 256³ multiply is 2·256³ = 33,554,432 FLOPs. In one second that is
        // 0.0335 GFLOP/s.
        let r = k.rate(1, 1, 1.0);
        assert!((r - FLOPS_PER_MULTIPLY / 1e9).abs() < 1e-12);
        assert!((r - 0.033_554_432).abs() < 1e-9);
        // Each thread runs its own independent matmul, so FLOPs add up.
        assert!((k.rate(1, 8, 1.0) - 8.0 * r).abs() < 1e-9);
    }

    #[test]
    fn threads_get_different_data() {
        let k = SgemmKernel;
        let mut a = k.setup(&SetupCtx {
            threads: 2,
            thread_index: 0,
        });
        let mut b = k.setup(&SetupCtx {
            threads: 2,
            thread_index: 1,
        });
        assert_ne!(a.run(1), b.run(1), "per-thread seeds should differ");
    }
}
