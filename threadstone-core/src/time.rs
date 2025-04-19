//! Cross‑platform high‑resolution monotonic timer
//! Returns *nanoseconds since first use* as `u128`.

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
        // Convert ticks → ns:   ticks * 1_000_000_000 / freq
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

    /// TSC ticks per nanosecond, estimated once at startup with a 50 ms sleep.
    static TICKS_PER_NS: Lazy<f64> = Lazy::new(|| {
        // Measure how many TSC ticks elapse in 50 ms wall‑time.
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
// ─── Fallback implementation ───────────────────────────────────────────────────
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

pub use imp::now_nanos;
