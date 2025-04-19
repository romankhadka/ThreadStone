//! Cross‑platform monotonic nanosecond timer
use once_cell::sync::Lazy;
use std::time::Instant;

/// Common start‑time so subsequent calls count *up*
static START: Lazy<Instant> = Lazy::new(Instant::now);

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[inline]
pub fn now_nanos() -> u128 {
    // Use TSC cycles; convert to ns with the frequency read on first call.
    // For now (simpler) fall back to Instant until we wire frequency‑calc.
    START.elapsed().as_nanos()
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
#[inline]
pub fn now_nanos() -> u128 {
    START.elapsed().as_nanos()
}
