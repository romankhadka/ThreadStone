use once_cell::sync::Lazy;
use std::sync::Mutex;

#[link(name = "dhry", kind = "static")]
extern "C" {
    /// C entry point:  int dhry(int iterations);
    fn dhry(number_of_runs: libc::c_int) -> libc::c_int;
}

/// Thread‑safe wrapper; Dhrystone uses globals, so run only one at a time.
static LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// Runs the benchmark for `iterations`.  
/// Returns: loops per second (f64).
pub fn run_dhry(iterations: u32) -> f64 {
    let _guard = LOCK.lock().unwrap();

    let start = std::time::Instant::now();
    let dhrystones_performed = unsafe { dhry(iterations as i32) } as u32;
    let elapsed = start.elapsed().as_secs_f64();

    dhrystones_performed as f64 / elapsed
}
