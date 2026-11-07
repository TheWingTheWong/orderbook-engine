//! Small, fast, deterministic PRNG (xoshiro256** seeded via SplitMix64).
//!
//! Kept in-crate so the synthetic flow is bit-for-bit reproducible across
//! platforms and dependency versions. Not cryptographically secure.

/// xoshiro256** generator.
#[derive(Debug, Clone)]
pub struct Rng {
    s: [u64; 4],
}

#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Rng {
    /// Seed deterministically from a single `u64`.
    pub fn new(seed: u64) -> Self {
        let mut sm = seed;
        Self {
            s: [splitmix64(&mut sm), splitmix64(&mut sm), splitmix64(&mut sm), splitmix64(&mut sm)],
        }
    }

    /// Next 64 random bits.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Uniform integer in `0..n` (Lemire's multiply-shift; bias < 2^-64 · n).
    ///
    /// # Panics
    /// If `n == 0`.
    #[inline]
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "Rng::below(0)");
        ((self.next_u64() as u128 * n as u128) >> 64) as u64
    }

    /// Uniform integer in `lo..=hi`.
    #[inline]
    pub fn range_i64(&mut self, lo: i64, hi: i64) -> i64 {
        debug_assert!(lo <= hi);
        lo.wrapping_add(self.below(hi.wrapping_sub(lo) as u64 + 1) as i64)
    }

    /// `true` with probability `num / den`.
    #[inline]
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }

    /// Fair coin.
    #[inline]
    pub fn coin(&mut self) -> bool {
        self.next_u64() >> 63 == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_a_seed() {
        let a: Vec<_> = {
            let mut r = Rng::new(7);
            (0..8).map(|_| r.next_u64()).collect()
        };
        let mut r = Rng::new(7);
        assert!(a.iter().all(|&x| x == r.next_u64()));
        assert_ne!(Rng::new(8).next_u64(), a[0]);
    }

    #[test]
    fn below_and_range_stay_in_bounds_and_cover() {
        let mut r = Rng::new(1);
        let mut seen = [false; 7];
        for _ in 0..10_000 {
            let x = r.range_i64(-3, 3);
            assert!((-3..=3).contains(&x));
            seen[(x + 3) as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn coin_is_roughly_fair() {
        let mut r = Rng::new(99);
        let heads = (0..100_000).filter(|_| r.coin()).count();
        assert!((48_000..52_000).contains(&heads), "{heads}");
    }
}
