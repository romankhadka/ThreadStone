//! Cross‑platform high‑resolution monotonic timer
//!
//! Returns nanoseconds since first call as `u128`.
//! • Apple Silicon / AArch64  → CNTVCT_EL0 / CNTFRQ_EL0
//! • x86 / x86_64            → RDTSC calibrated once at startup
//! • Others                  → std::time::Instant fallback
//!
//! No unsafe code escapes this module.

use once_cell::sync::Lazy;

//
// ─── AARCH64 (Apple Silicon & Linux ARM) ────────────────────────────────────────
//
#[cfg(target_arch = "aarch64")]
mod imp {
    use super::*;
    use core::arch::asm;

    /// Counter frequency (ticks per second)
    static FREQ_HZ: Lazy<u64> = Lazy::new(|| unsafe {
        let freq: u64;
        asm!("mrs {freq}, cntfrq_el0", freq = out(reg) freq);
        freq
    });

    #[inline]
    pub fn now_nanos() -> u128 {
        let ticks: u64;
        unsafe { asm!("mrs {ticks}, cntvct_el0", ticks = out(reg) ticks) };
        // ticks * 1_000_000_000 / freq  (converted with full 128‑bit precision)
        (ticks as u128 * 1_000_000_000u128) / (*FREQ_HZ as u128)
    }
}

//
// ─── X86 / X86_64 ───────────────────────────────────────────────────────────────
//
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod imp {
    use super::*;
    use core::arch::x86_64::__rdtsc;
    use std::time::Instant;

    /// TSC ticks per nanosecond, estimated once at startup with a ~50 ms wall‑clock sample
    static TICKS_PER_NS: Lazy<f64> = Lazy::new(|| {
        let start_tsc = unsafe { __rdtsc() };
        let start = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let delta_ns = start.elapsed().as_nanos() as f64;
        let delta_tsc = unsafe { __rdtsc() - start_tsc } as f64;
        delta_tsc / delta_ns // ticks / ns
    });

    #[inline]
    pub fn now_nanos() -> u128 {
        let tsc = unsafe { __rdtsc() } as f64;
        (tsc / *TICKS_PER_NS) as u128
    }
}

//
// ─── Fallback (any other architecture) ─────────────────────────────────────────
//
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86", target_arch = "x86_64")))]
mod imp {
    use super::*;
    use std::time::Instant;

    static START: Lazy<Instant> = Lazy::new(Instant::now);

    #[inline]
    pub fn now_nanos() -> u128 {
        START.elapsed().as_nanos()
    }
}

//
// ─── Public re‑export ──────────────────────────────────────────────────────────
//
pub use imp::now_nanos;

#[cfg(test)]
mod tests {
    use super::now_nanos;

    #[test]
    fn monotonic() {
        let t0 = now_nanos();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let t1 = now_nanos();
        assert!(t1 > t0, "timer should be increasing ({} <= {})", t0, t1);
    }
}
