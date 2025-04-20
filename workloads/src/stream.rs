//! A simple STREAM triad implementation:
//!   for i in 0..size { a[i] = b[i] + SCALAR * c[i]; }
//! Measures memory bandwidth in MB/s.

use std::time::Instant;
use rayon::prelude::*;

/// The scalar multiplier in the STREAM triad
const SCALAR: f64 = 3.0;

/// Runs `iterations` passes of the STREAM triad over three size‑`size` arrays.
/// Returns achieved bandwidth in MB/s.
pub fn run_stream(size: usize, iterations: usize) -> f64 {
    // 1) allocate and initialize
    let mut a = vec![0.0f64; size];
    let b = vec![1.0f64; size];
    let c = vec![2.0f64; size];

    // 2) time the triad loop
    let start = Instant::now();
    for _ in 0..iterations {
        a.par_chunks_mut(1024)
            .zip(b.par_chunks(1024))
            .zip(c.par_chunks(1024))
            .for_each(|((a_chunk, b_chunk), c_chunk)| {
                for i in 0..a_chunk.len() {
                    a_chunk[i] = b_chunk[i] + SCALAR * c_chunk[i];
                }
            });
    }
    let secs = start.elapsed().as_secs_f64();

    // 3) compute bytes moved: each pass reads 2×8 bytes and writes 1×8 bytes per element
    let total_bytes = (size * std::mem::size_of::<f64>() * 3) as f64 * (iterations as f64);

    // 4) convert to megabytes/sec
    (total_bytes / secs) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_stream_is_sane() {
        // pick a very small size & iterations
        let bw = run_stream(16, 4);
        // we expect something > 0 and not NaN
        assert!(bw.is_finite() && bw > 0.0);
    }
}