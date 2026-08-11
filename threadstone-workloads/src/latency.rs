//! Memory latency — dependent-load pointer chasing.
//!
//! Measures how long a single load takes when nothing can hide it. The chase
//! array holds a permutation forming one closed cycle, and each step's address
//! is the previous step's *value*, so the CPU cannot issue the next load until
//! the current one returns. No prefetcher can help, no amount of
//! memory-level parallelism applies, and the result is the honest end-to-end
//! latency of one cache miss.
//!
//! # Defeating the prefetchers
//!
//! Two properties make the chase unpredictable:
//!
//! * **One node per cache line.** Nodes are 64 bytes apart, so no two
//!   consecutive hops share a line and every hop is a fresh miss.
//! * **A single random cycle, not random jumps.** A uniformly random index
//!   would revisit nodes and touch only ~63% of the buffer. Building one
//!   Hamiltonian cycle over a shuffled permutation guarantees every node is
//!   visited exactly once per lap, so the working set is exactly the buffer
//!   size — which is what makes the cache-hierarchy sweep meaningful.
//!
//! # Why this is single-threaded only
//!
//! This kernel is [`Scaling::SingleThreadOnly`], and the reason is worth
//! stating plainly. Partitioning a 256 MiB buffer across sixteen threads gives
//! each a 16 MiB slice that fits in last-level cache, so the "multi-threaded
//! latency" would be an LLC hit time — several times better than the real
//! number, and a straightforward lie about the machine. Replicating the full
//! buffer per thread instead measures *loaded* latency, a legitimate but
//! entirely different metric, and would need 4 GiB on a sixteen-thread box.
//!
//! Rather than report a flattering wrong number or silently change the metric,
//! the suite measures one honest figure and excludes it from the multi-core
//! score.

use threadstone_core::kernel::{
    Footprint, Kernel, KernelInfo, KernelState, Scaling, SetupCtx, Unit,
};

use crate::rng::Rng;

/// Cache line size assumed when spacing nodes.
///
/// 64 bytes on x86-64 and on Apple silicon's 128-byte-line cores this still
/// guarantees at most two nodes per line, which does not materially help a
/// randomised chase.
const LINE_BYTES: usize = 64;

/// `usize` values per node, so each node occupies one cache line.
const WORDS_PER_NODE: usize = LINE_BYTES / std::mem::size_of::<usize>();

/// Buffer size for the headline measurement.
///
/// 256 MiB is far past the largest last-level cache in a consumer machine, so
/// every hop reaches DRAM.
pub const DEFAULT_BYTES: usize = 256 << 20;

/// Seed for the permutation, fixed so every machine chases the same cycle.
const SEED: u64 = 0x1A7E_4C7A_5EED;

/// A pointer-chase buffer: `chase[i]` holds the word index of the next node.
struct Chase {
    chase: Vec<usize>,
    /// Where the next `run` resumes, so consecutive calls continue the cycle
    /// rather than restarting from a node that may still be cached.
    cursor: usize,
}

impl Chase {
    /// Build a single closed cycle covering every node in a `bytes` buffer.
    fn new(bytes: usize, seed: u64) -> Chase {
        let nodes = (bytes / LINE_BYTES).max(2);
        let mut order: Vec<usize> = (0..nodes).collect();
        Rng::new(seed).shuffle(&mut order);

        // Link the shuffled order into one cycle: order[i] points at
        // order[i+1], and the last points back at the first. Because `order` is
        // a permutation, following the links visits every node exactly once
        // before returning to the start.
        let mut chase = vec![0usize; nodes * WORDS_PER_NODE];
        for i in 0..nodes {
            let from = order[i];
            let to = order[(i + 1) % nodes];
            chase[from * WORDS_PER_NODE] = to * WORDS_PER_NODE;
        }

        Chase {
            chase,
            cursor: order[0] * WORDS_PER_NODE,
        }
    }
}

impl KernelState for Chase {
    fn run(&mut self, iters: u64) -> u64 {
        let mut p = self.cursor;
        // The loop body is one dependent load. Everything else — the counter,
        // the bounds check LLVM hoists — overlaps with the outstanding miss.
        for _ in 0..iters {
            p = self.chase[p];
        }
        self.cursor = p;
        p as u64
    }
}

/// The memory latency workload.
pub struct LatencyKernel;

impl Kernel for LatencyKernel {
    fn info(&self) -> KernelInfo {
        KernelInfo {
            id: "latency",
            name: "Memory Latency",
            summary: "Dependent-load pointer chase over 256 MiB: unhidden DRAM latency",
            unit: Unit::Nanoseconds,
            footprint: Footprint::PerThread,
            scaling: Scaling::SingleThreadOnly,
            // Typical DDR4 loaded latency for a random access from a core.
            reference: 90.0,
        }
    }

    fn setup(&self, ctx: &SetupCtx) -> Box<dyn KernelState> {
        // Distinct permutations per thread, so that if a caller does force a
        // multi-threaded run the threads do not share a chase pattern.
        Box::new(Chase::new(DEFAULT_BYTES, SEED ^ ctx.thread_index as u64))
    }

    fn rate(&self, iters_per_thread: u64, _threads: usize, secs: f64) -> f64 {
        // Nanoseconds per hop. Concurrent threads do not make any single
        // dependent load faster, so this is per-thread and the count is not
        // multiplied.
        secs / iters_per_thread as f64 * 1e9
    }
}

/// One point on a cache-hierarchy sweep.
#[derive(Debug, Clone, Copy)]
pub struct SweepPoint {
    /// Working set size in bytes.
    pub bytes: usize,
    /// Measured latency per access, in nanoseconds.
    pub latency_ns: f64,
}

/// Measure chase latency across a range of working-set sizes.
///
/// The resulting curve makes the cache hierarchy directly visible: latency sits
/// flat inside each level and steps up at every boundary, so the plateaus name
/// the cache sizes and the step heights name their access costs. This is the
/// data behind the hierarchy chart, and it is a far more useful description of
/// a memory system than any single number.
///
/// Each size is measured for at least `min_millis`, with the iteration count
/// calibrated per size so small buffers are not measured over a window too
/// short for the clock.
pub fn sweep(sizes: &[usize], min_millis: u64) -> Vec<SweepPoint> {
    sizes
        .iter()
        .map(|&bytes| SweepPoint {
            bytes,
            latency_ns: measure(bytes, min_millis),
        })
        .collect()
}

/// Latency in nanoseconds for one working-set size.
fn measure(bytes: usize, min_millis: u64) -> f64 {
    use std::time::Instant;

    let mut chase = Chase::new(bytes, SEED);
    let target = std::time::Duration::from_millis(min_millis);

    // Touch every node once so the timed pass measures steady-state behaviour
    // rather than first-touch page faults.
    let nodes = chase.chase.len() / WORDS_PER_NODE;
    chase.run(nodes as u64);

    // Grow the hop count until the window is long enough to trust.
    let mut hops: u64 = 1024;
    loop {
        let start = Instant::now();
        let sink = chase.run(hops);
        let elapsed = start.elapsed();
        std::hint::black_box(sink);
        if elapsed >= target {
            return elapsed.as_secs_f64() / hops as f64 * 1e9;
        }
        let Some(next) = hops.checked_mul(4) else {
            return elapsed.as_secs_f64() / hops as f64 * 1e9;
        };
        hops = next;
    }
}

/// Sizes for a hierarchy sweep: powers of two from 4 KiB to 256 MiB.
///
/// Powers of two are chosen so the plateaus line up with real cache capacities,
/// which are themselves powers of two.
pub fn default_sweep_sizes() -> Vec<usize> {
    (12..=28).map(|shift| 1usize << shift).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Follow the chase and collect every node it visits.
    fn visited(chase: &Chase, nodes: usize) -> Vec<usize> {
        let mut seen = Vec::with_capacity(nodes);
        let mut p = chase.cursor;
        for _ in 0..nodes {
            seen.push(p);
            p = chase.chase[p];
        }
        seen
    }

    #[test]
    fn chase_forms_one_cycle_covering_every_node() {
        // The property the whole measurement depends on: the working set is
        // exactly the buffer, with no node skipped and none visited twice.
        let bytes = 64 * 1024;
        let nodes = bytes / LINE_BYTES;
        let chase = Chase::new(bytes, 1);

        let seen = visited(&chase, nodes);
        let unique: HashSet<usize> = seen.iter().copied().collect();
        assert_eq!(
            unique.len(),
            nodes,
            "every node must be visited exactly once"
        );

        // After a full lap the chase must return to where it started.
        let mut p = chase.cursor;
        for _ in 0..nodes {
            p = chase.chase[p];
        }
        assert_eq!(p, chase.cursor, "the cycle must close");
    }

    #[test]
    fn nodes_are_one_cache_line_apart() {
        let chase = Chase::new(64 * 1024, 2);
        for step in visited(&chase, 100) {
            assert_eq!(
                step % WORDS_PER_NODE,
                0,
                "every hop must land on a line boundary"
            );
        }
    }

    #[test]
    fn chase_is_not_sequential() {
        // A sequential walk would be perfectly prefetchable and measure nothing.
        let chase = Chase::new(256 * 1024, 3);
        let seen = visited(&chase, 200);
        let sequential = seen
            .windows(2)
            .filter(|w| w[1] == w[0] + WORDS_PER_NODE)
            .count();
        assert!(
            sequential < 20,
            "{sequential} of 199 hops were sequential; the shuffle is not working"
        );
    }

    #[test]
    fn run_resumes_where_it_left_off() {
        let bytes = 64 * 1024;
        let mut a = Chase::new(bytes, 4);
        let mut b = Chase::new(bytes, 4);
        // 100 hops then 50 must land in the same place as 150 in one go.
        a.run(100);
        a.run(50);
        b.run(150);
        assert_eq!(a.cursor, b.cursor);
    }

    #[test]
    fn same_seed_builds_the_same_cycle() {
        let a = Chase::new(32 * 1024, 7);
        let b = Chase::new(32 * 1024, 7);
        assert_eq!(a.chase, b.chase);
        assert_eq!(a.cursor, b.cursor);
    }

    #[test]
    fn tiny_buffers_do_not_panic() {
        // Guards the `.max(2)` floor: a zero- or one-node cycle is degenerate.
        for bytes in [0usize, 1, 63, 64, 65] {
            let mut c = Chase::new(bytes, 1);
            c.run(10);
        }
    }

    #[test]
    fn rate_is_nanoseconds_per_hop_and_ignores_threads() {
        let k = LatencyKernel;
        // A million hops in 100 ms is 100 ns each.
        assert!((k.rate(1_000_000, 1, 0.1) - 100.0).abs() < 1e-9);
        assert!(
            (k.rate(1_000_000, 8, 0.1) - 100.0).abs() < 1e-9,
            "latency is per-access; concurrency must not divide it"
        );
        assert!(!k.info().unit.higher_is_better());
        assert_eq!(k.info().scaling, Scaling::SingleThreadOnly);
    }

    #[test]
    fn sweep_sizes_are_ascending_powers_of_two() {
        let sizes = default_sweep_sizes();
        assert!(sizes.len() >= 10);
        assert_eq!(sizes[0], 4096);
        assert_eq!(*sizes.last().unwrap(), 256 << 20);
        for pair in sizes.windows(2) {
            assert_eq!(pair[1], pair[0] * 2);
        }
    }

    #[test]
    fn sweep_latency_grows_with_the_working_set() {
        // L1-resident accesses must be measurably faster than ones that miss to
        // DRAM. Only two sizes and a short window, to keep the test quick.
        let points = sweep(&[16 * 1024, 64 << 20], 20);
        assert_eq!(points.len(), 2);
        assert!(points.iter().all(|p| p.latency_ns > 0.0));
        assert!(
            points[1].latency_ns > points[0].latency_ns * 2.0,
            "a 64 MiB chase ({:.1}ns) should be far slower than a 16 KiB one ({:.1}ns)",
            points[1].latency_ns,
            points[0].latency_ns
        );
    }
}
