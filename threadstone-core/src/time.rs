//! Cross‑platform nanosecond timer
#[cfg(target_arch = "x86_64")]
pub fn now_nanos() -> u128 {
    // Safe wrapper around the RDTSC cycle counter
    unsafe { core::arch::x86_64::_rdtsc() as u128 }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn now_nanos() -> u128 {
    std::time::Instant::now().elapsed().as_nanos()
}
