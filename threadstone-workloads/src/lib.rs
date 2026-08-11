//! The six benchmark kernels of the ThreadStone suite.
//!
//! Each workload measures a capability the others cannot see. A CPU that is
//! fast at all six is fast; one that is fast at a single one is fast at that
//! one thing, and the point of reporting six numbers next to each other is to
//! make that distinction impossible to hide.
//!
//! | Workload | Bottleneck it exposes |
//! |---|---|
//! | [`dhrystone`] | Integer ALU, branch prediction, call overhead |
//! | [`sgemm`] | Floating-point and SIMD throughput out of L2 |
//! | [`stream`] | Sustained DRAM bandwidth |
//! | [`latency`] | Unhidden memory latency — the one number caches cannot fix |
//! | [`sha256`] | Dependent-chain integer ALU with no memory traffic |
//! | [`sort`] | Branch mispredicts and irregular access, as real code produces |
//!
//! # Two rules every kernel here follows
//!
//! **Inputs are deterministic.** Every buffer is filled from a fixed seed (see
//! [`rng`]), so two machines run byte-identical problems. A sort whose input
//! happened to be more ordered on one machine would be measuring a different
//! problem, not a different CPU.
//!
//! **Results are observable.** Each `run` returns a checksum derived from real
//! computed output, which the runner black-boxes. Without that data dependency
//! LLVM deletes the loop, and the benchmark measures an empty `for`.

#![warn(missing_docs)]

pub mod dhrystone;
pub mod latency;
pub mod rng;
pub mod sgemm;
pub mod sha256;
pub mod sort;
pub mod stream;

use threadstone_core::kernel::Kernel;

/// Every workload, in the order the suite runs them.
///
/// Ordered cheapest-setup first, so an interrupted run still yields the
/// workloads that say the most about a machine before the ones that need
/// hundreds of megabytes of allocation.
pub fn all() -> Vec<Box<dyn Kernel>> {
    vec![
        Box::new(dhrystone::DhrystoneKernel),
        Box::new(sgemm::SgemmKernel),
        Box::new(sha256::Sha256Kernel),
        Box::new(sort::SortKernel),
        Box::new(stream::StreamKernel),
        Box::new(latency::LatencyKernel),
    ]
}

/// Look up one workload by its identifier.
pub fn by_id(id: &str) -> Option<Box<dyn Kernel>> {
    all().into_iter().find(|k| k.info().id == id)
}

/// Every workload identifier, for CLI validation and help text.
pub fn ids() -> Vec<&'static str> {
    all().iter().map(|k| k.info().id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use threadstone_core::kernel::{Footprint, Scaling, SetupCtx};

    #[test]
    fn registry_has_six_workloads() {
        assert_eq!(all().len(), 6);
    }

    #[test]
    fn identifiers_are_unique() {
        let ids = ids();
        let unique: HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "duplicate workload id in {ids:?}");
    }

    #[test]
    fn identifiers_are_lookup_keys() {
        for id in ids() {
            let kernel = by_id(id).unwrap_or_else(|| panic!("{id} not found"));
            assert_eq!(kernel.info().id, id);
        }
        assert!(by_id("no-such-workload").is_none());
    }

    #[test]
    fn every_workload_describes_itself() {
        for kernel in all() {
            let info = kernel.info();
            assert!(!info.id.is_empty());
            assert!(!info.name.is_empty(), "{}: missing name", info.id);
            assert!(
                info.summary.len() > 20,
                "{}: summary is too terse to be useful",
                info.id
            );
            assert!(
                info.reference > 0.0 && info.reference.is_finite(),
                "{}: reference must be positive",
                info.id
            );
        }
    }

    #[test]
    fn every_workload_runs_and_returns_a_live_checksum() {
        for kernel in all() {
            let info = kernel.info();
            let mut state = kernel.setup(&SetupCtx {
                threads: 1,
                thread_index: 0,
            });
            let checksum = state.run(1);
            assert_ne!(
                checksum, 0,
                "{}: checksum of zero suggests the work was elided",
                info.id
            );
        }
    }

    #[test]
    fn every_workload_reports_a_positive_finite_rate() {
        for kernel in all() {
            let info = kernel.info();
            let rate = kernel.rate(1000, 4, 0.25);
            assert!(
                rate > 0.0 && rate.is_finite(),
                "{}: implausible rate {rate}",
                info.id
            );
        }
    }

    #[test]
    fn partitioned_workloads_ignore_the_thread_count_in_their_rate() {
        // Getting this wrong overstates bandwidth by the thread count, which is
        // the single easiest way to publish a wildly wrong benchmark number.
        for kernel in all() {
            let info = kernel.info();
            if info.footprint != Footprint::Partitioned {
                continue;
            }
            let one = kernel.rate(100, 1, 1.0);
            let many = kernel.rate(100, 16, 1.0);
            assert!(
                (one - many).abs() < 1e-9,
                "{}: partitioned rate changed with thread count ({one} vs {many})",
                info.id
            );
        }
    }

    #[test]
    fn per_thread_workloads_scale_their_rate_with_the_thread_count() {
        for kernel in all() {
            let info = kernel.info();
            // Latency is per-thread in footprint but per-access in unit, so its
            // rate is correctly independent of concurrency.
            if info.footprint != Footprint::PerThread || !info.unit.higher_is_better() {
                continue;
            }
            let one = kernel.rate(100, 1, 1.0);
            let four = kernel.rate(100, 4, 1.0);
            assert!(
                (four - 4.0 * one).abs() < 1e-6 * four.abs().max(1.0),
                "{}: four threads should be 4x the work ({four} vs {one})",
                info.id
            );
        }
    }

    #[test]
    fn only_latency_opts_out_of_multi_core_scoring() {
        let excluded: Vec<&str> = all()
            .iter()
            .filter(|k| k.info().scaling == Scaling::SingleThreadOnly)
            .map(|k| k.info().id)
            .collect();
        assert_eq!(excluded, vec!["latency"]);
    }

    #[test]
    fn setup_works_at_realistic_thread_counts() {
        // Guards against a partitioned kernel dividing to zero, or a per-thread
        // kernel indexing by thread id out of range.
        for kernel in all() {
            let info = kernel.info();
            for (threads, thread_index) in [(1, 0), (2, 1), (14, 13), (64, 63)] {
                let mut state = kernel.setup(&SetupCtx {
                    threads,
                    thread_index,
                });
                state.run(1);
                let _ = info.id;
            }
        }
    }
}
