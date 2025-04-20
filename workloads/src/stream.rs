/// Run the STREAM Triad kernel: A[i] = B[i] + scalar * C[i]
///
/// # Arguments
///
/// * `size` - length of each vector
/// * `iterations` - number of times to repeat the triad
///
/// # Returns
///
/// Measured bandwidth in gigabytes per second (GB/s).
pub fn run_stream(size: usize, iterations: usize) -> f64 {
    let mut a = vec![0.0f64; size];
    let b = vec![1.0f64; size];
    let c = vec![2.0f64; size];
    let scalar = 3.0f64;

    let start = std::time::Instant::now();
    for _ in 0..iterations {
        for i in 0..size {
            a[i] = b[i] + scalar * c[i];
        }
    }
    let elapsed = start.elapsed();
    let elapsed_ns = elapsed.as_nanos() as f64;
    
    let bytes_per_iteration = size * 3 * std::mem::size_of::<f64>();
    let total_bytes = bytes_per_iteration * iterations;
    
    (total_bytes as f64) / elapsed_ns * 1e9 / 1e9 // Convert to GB/s
}

#[cfg(test)]
mod tests {
    use super::run_stream;

    /// A simple smoke test (ignored until full implementation)
    #[test]
    fn stream_smoke() {
        let bw = run_stream(1 << 20, 10);
        assert!(bw > 0.0, "STREAM triad returned {} GB/s, expected positive", bw);
    }
}
