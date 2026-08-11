//! A deterministic pseudo-random generator for building workload inputs.
//!
//! Benchmark inputs must be identical on every machine and every run, or the
//! numbers are not comparable — a sort whose input happens to be more ordered
//! on one run is measuring a different problem. So the generator is seeded by
//! constant and never by the clock.
//!
//! This is `splitmix64`: two multiplies and three xor-shifts, no state beyond a
//! counter, and good enough statistical quality for filling arrays. It is not
//! cryptographic, and nothing here needs it to be.

/// Deterministic `splitmix64` generator.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Create a generator from an explicit seed.
    pub fn new(seed: u64) -> Rng {
        Rng { state: seed }
    }

    /// Next 64-bit value.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform value in `0..bound`, without modulo bias.
    ///
    /// Uses Lemire's multiply-shift reduction with a rejection step, which
    /// costs one multiply in the common case instead of the division a
    /// remainder-based approach would need.
    #[inline]
    pub fn below(&mut self, bound: u64) -> u64 {
        debug_assert!(bound > 0, "bound must be positive");
        let mut x = self.next_u64();
        let mut m = (x as u128) * (bound as u128);
        let mut low = m as u64;
        if low < bound {
            // Reject the short tail that would otherwise be over-represented.
            let threshold = bound.wrapping_neg() % bound;
            while low < threshold {
                x = self.next_u64();
                m = (x as u128) * (bound as u128);
                low = m as u64;
            }
        }
        (m >> 64) as u64
    }

    /// Uniform `f64` in `[0, 1)`, using the top 53 bits.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Fisher-Yates shuffle of `items`.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_gives_same_sequence() {
        let a: Vec<u64> = (0..32)
            .scan(Rng::new(7), |r, _| Some(r.next_u64()))
            .collect();
        let b: Vec<u64> = (0..32)
            .scan(Rng::new(7), |r, _| Some(r.next_u64()))
            .collect();
        assert_eq!(a, b, "generator must be reproducible");
    }

    #[test]
    fn different_seeds_diverge() {
        let a: Vec<u64> = (0..8)
            .scan(Rng::new(1), |r, _| Some(r.next_u64()))
            .collect();
        let b: Vec<u64> = (0..8)
            .scan(Rng::new(2), |r, _| Some(r.next_u64()))
            .collect();
        assert_ne!(a, b);
    }

    #[test]
    fn below_respects_its_bound() {
        let mut rng = Rng::new(42);
        for bound in [1u64, 2, 3, 7, 1000, u64::MAX / 3] {
            for _ in 0..500 {
                assert!(rng.below(bound) < bound, "bound {bound} violated");
            }
        }
    }

    #[test]
    fn below_one_is_always_zero() {
        let mut rng = Rng::new(9);
        for _ in 0..100 {
            assert_eq!(rng.below(1), 0);
        }
    }

    #[test]
    fn below_is_roughly_uniform() {
        // A chi-square-flavoured smoke test: 60k draws over 6 buckets should
        // put every bucket within 10% of 10k. Loose enough never to flake,
        // tight enough to catch a truncation or sign bug.
        let mut rng = Rng::new(1234);
        let mut buckets = [0u32; 6];
        for _ in 0..60_000 {
            buckets[rng.below(6) as usize] += 1;
        }
        for (i, count) in buckets.iter().enumerate() {
            assert!(
                (9_000..11_000).contains(count),
                "bucket {i} got {count}, expected ~10000"
            );
        }
    }

    #[test]
    fn next_f64_stays_in_the_unit_interval() {
        let mut rng = Rng::new(5);
        for _ in 0..10_000 {
            let v = rng.next_f64();
            assert!((0.0..1.0).contains(&v), "out of range: {v}");
        }
    }

    #[test]
    fn shuffle_is_a_permutation() {
        let mut items: Vec<u32> = (0..1000).collect();
        Rng::new(3).shuffle(&mut items);
        assert_ne!(items, (0..1000).collect::<Vec<u32>>(), "nothing moved");
        items.sort_unstable();
        assert_eq!(items, (0..1000).collect::<Vec<u32>>(), "elements were lost");
    }

    #[test]
    fn shuffle_handles_degenerate_slices() {
        let mut rng = Rng::new(1);
        let mut empty: [u32; 0] = [];
        rng.shuffle(&mut empty);
        let mut one = [42];
        rng.shuffle(&mut one);
        assert_eq!(one, [42]);
    }
}
