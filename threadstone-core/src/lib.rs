/// High‑resolution monotonic timer implementation.
pub mod time;
/// Re‑export the public API as `now_nanos()`.
pub use time::now_nanos;

#[cfg(test)]
mod tests {
    use super::now_nanos;

    #[test]
    fn it_counts_up() {
        let t0 = now_nanos();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t1 = now_nanos();
        assert!(t1 > t0, "now_nanos should advance ({} <= {})", t0, t1);
    }
}
