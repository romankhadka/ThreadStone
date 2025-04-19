pub mod time;

#[cfg(test)]
mod tests {
    use super::time::now_nanos;

    #[test]
    fn it_counts_up() {
        let t0 = now_nanos();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t1 = now_nanos();
        assert!(t1 > t0);
    }
}
