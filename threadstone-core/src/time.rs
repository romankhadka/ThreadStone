//! Timing primitives, and honesty about what they can resolve.
//!
//! Two distinct clocks live here, because benchmarks need two distinct things:
//!
//! * [`now_nanos`] — monotonic nanoseconds since process start. Backed by
//!   [`Instant`], which is the right answer on every platform we target: it is
//!   already `clock_gettime(CLOCK_MONOTONIC)` on Linux and `mach_absolute_time`
//!   on macOS. Use this for measurement windows.
//! * [`cycles`] — the raw architectural counter (`CNTVCT_EL0` / `RDTSC`). Use
//!   this only to report cycle-level figures such as cycles-per-access, and
//!   only over windows long enough that the counter's granularity disappears.
//!
//! ## Why not build the measurement clock on RDTSC?
//!
//! The previous implementation calibrated `RDTSC` against a 50 ms sleep on the
//! first call, then divided by a float on every subsequent call. That buys
//! nothing: the TSC is not guaranteed invariant across sockets or power states,
//! the calibration stalls process startup, and `Instant` is already a `vDSO`
//! read of the same underlying hardware. So the measurement clock is `Instant`,
//! and the raw counter is opt-in.
//!
//! ## Resolution is measured, not assumed
//!
//! [`resolution_nanos`] empirically determines the smallest non-zero interval
//! the clock can report. This matters: `CNTVCT_EL0` on Apple silicon ticks at
//! 24 MHz, so its granularity is ~41.7 ns — coarser than people expect. Every
//! result file records the measured resolution so a reader can judge whether a
//! measurement window was long enough to trust.

use std::sync::OnceLock;
use std::time::Instant;

/// Process-start reference point for [`now_nanos`].
fn origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// Monotonic nanoseconds elapsed since the first call into this module.
///
/// `u64` nanoseconds covers 584 years of uptime, so the `u128` the previous
/// implementation returned only cost arithmetic without buying range.
#[inline]
pub fn now_nanos() -> u64 {
    origin().elapsed().as_nanos() as u64
}

/// Name of the hardware source backing [`cycles`], for the result provenance.
pub fn cycle_source() -> &'static str {
    imp::SOURCE
}

/// Raw architectural cycle counter.
///
/// On aarch64 this is `CNTVCT_EL0`, a fixed-frequency counter (24 MHz on Apple
/// silicon) — *not* a core clock cycle count. On x86 it is `RDTSC`, which ticks
/// at the CPU's nominal (not actual) frequency. Convert with
/// [`cycles_per_second`], and never assume it corresponds to retired clocks.
#[inline]
pub fn cycles() -> u64 {
    imp::cycles()
}

/// Frequency of the [`cycles`] counter, in ticks per second.
///
/// Read directly from `CNTFRQ_EL0` on aarch64. On x86 there is no architectural
/// way to query it, so it is calibrated against [`Instant`] on first use — this
/// costs ~20 ms, but only if you actually call it.
pub fn cycles_per_second() -> f64 {
    static FREQ: OnceLock<f64> = OnceLock::new();
    *FREQ.get_or_init(imp::calibrate_frequency)
}

/// Smallest non-zero interval [`now_nanos`] can distinguish, in nanoseconds.
///
/// Measured by spinning until the clock advances, repeated to take the minimum
/// observed step. Computed once and cached.
pub fn resolution_nanos() -> u64 {
    static RES: OnceLock<u64> = OnceLock::new();
    *RES.get_or_init(|| {
        let mut best = u64::MAX;
        for _ in 0..64 {
            let start = Instant::now();
            // Spin until the clock reports a different value. The first
            // observed change is one tick of the underlying counter.
            let step = loop {
                let d = start.elapsed().as_nanos() as u64;
                if d > 0 {
                    break d;
                }
            };
            best = best.min(step);
        }
        best.max(1)
    })
}

/// Overhead of a single [`now_nanos`] call, in nanoseconds.
///
/// Reported alongside the results so that short measurement windows can be
/// recognised as untrustworthy rather than silently believed.
pub fn call_overhead_nanos() -> f64 {
    static OVERHEAD: OnceLock<u64> = OnceLock::new();
    let bits = *OVERHEAD.get_or_init(|| {
        const N: u32 = 10_000;
        let start = Instant::now();
        for _ in 0..N {
            std::hint::black_box(Instant::now());
        }
        let total = start.elapsed().as_nanos() as f64;
        (total / f64::from(N)).to_bits()
    });
    f64::from_bits(bits)
}

#[cfg(target_arch = "aarch64")]
mod imp {
    pub const SOURCE: &str = "cntvct_el0";

    #[inline]
    pub fn cycles() -> u64 {
        let ticks: u64;
        // SAFETY: `mrs` from CNTVCT_EL0 is an unprivileged read of a
        // architecturally-defined counter register. It has no side effects and
        // cannot fault at EL0 on any supported platform.
        unsafe {
            core::arch::asm!("mrs {t}, cntvct_el0", t = out(reg) ticks, options(nomem, nostack))
        };
        ticks
    }

    pub fn calibrate_frequency() -> f64 {
        let freq: u64;
        // SAFETY: as above; CNTFRQ_EL0 is readable at EL0 and holds the
        // counter frequency in Hz.
        unsafe {
            core::arch::asm!("mrs {f}, cntfrq_el0", f = out(reg) freq, options(nomem, nostack))
        };
        freq as f64
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod imp {
    use std::time::Instant;

    pub const SOURCE: &str = "rdtsc";

    #[inline]
    pub fn cycles() -> u64 {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: `rdtsc` is unprivileged on all supported x86_64 CPUs and has
        // no memory side effects.
        unsafe {
            core::arch::x86_64::_rdtsc()
        }
        #[cfg(target_arch = "x86")]
        // SAFETY: as above.
        unsafe {
            core::arch::x86::_rdtsc()
        }
    }

    pub fn calibrate_frequency() -> f64 {
        // Only paid if a caller actually asks for the frequency.
        let t0 = Instant::now();
        let c0 = cycles();
        while t0.elapsed().as_millis() < 20 {
            std::hint::spin_loop();
        }
        let elapsed = t0.elapsed().as_secs_f64();
        let delta = cycles().wrapping_sub(c0) as f64;
        delta / elapsed
    }
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86", target_arch = "x86_64")))]
mod imp {
    pub const SOURCE: &str = "monotonic-fallback";

    #[inline]
    pub fn cycles() -> u64 {
        super::now_nanos()
    }

    pub fn calibrate_frequency() -> f64 {
        1e9
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_nanos_advances() {
        let t0 = now_nanos();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let t1 = now_nanos();
        assert!(t1 > t0, "clock must advance: {t0} -> {t1}");
        assert!(
            t1 - t0 >= 4_000_000,
            "5ms sleep should register as at least 4ms, got {}ns",
            t1 - t0
        );
    }

    #[test]
    fn cycle_counter_advances() {
        let c0 = cycles();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let c1 = cycles();
        assert!(c1 > c0, "cycle counter must advance: {c0} -> {c1}");
    }

    #[test]
    fn cycle_frequency_is_plausible() {
        let hz = cycles_per_second();
        // 1 MHz to 100 GHz brackets every real counter while still catching a
        // calibration that returned zero, NaN, or a nanosecond count.
        assert!(
            (1e6..1e11).contains(&hz),
            "counter frequency implausible: {hz} Hz"
        );
    }

    #[test]
    fn resolution_is_bounded() {
        let r = resolution_nanos();
        assert!(r >= 1, "resolution must be at least 1ns");
        assert!(
            r < 1_000_000,
            "resolution worse than 1ms is unusable: {r}ns"
        );
    }

    #[test]
    fn overhead_is_bounded() {
        let o = call_overhead_nanos();
        assert!(o > 0.0 && o < 10_000.0, "implausible clock overhead: {o}ns");
    }
}
