//! A fast non-cryptographic hasher for the order-id index.
//!
//! `std`'s default SipHash-1-3 is DoS-resistant but costs ~20 ns per `u64`
//! key. Order ids are integers assigned by trusted gateways, so the index uses
//! the multiply-rotate scheme from rustc's `FxHasher`: one multiply per word.
//! Sequential ids land in distinct buckets (the low bits of `id * K` are a
//! bijection of the low bits of `id`), and the high bits hashbrown uses for
//! its control bytes are well mixed by the multiply.
//!
//! Do not use this for keys an adversary controls.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

const K: u64 = 0x517c_c1b7_2722_0a95;

/// Fx-style hasher: `h = (h.rotl(5) ^ word) * K` per word.
#[derive(Debug, Default, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(K);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.add(u64::from_le_bytes(c.try_into().unwrap()));
        }
        for &b in chunks.remainder() {
            self.add(b as u64);
        }
    }

    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }

    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

/// `HashMap` using [`FxHasher`].
pub type FxHashMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{BuildHasher, Hash};

    fn h<T: Hash>(v: T) -> u64 {
        BuildHasherDefault::<FxHasher>::default().hash_one(v)
    }

    #[test]
    fn deterministic_and_distinguishes_neighbours() {
        assert_eq!(h(42u64), h(42u64));
        let hashes: std::collections::HashSet<_> = (0u64..10_000).map(h).collect();
        assert_eq!(hashes.len(), 10_000);
    }

    #[test]
    fn map_round_trip() {
        let mut m: FxHashMap<u64, u32> = FxHashMap::default();
        for i in 0..1_000 {
            m.insert(i * 7, i as u32);
        }
        assert!((0..1_000).all(|i| m[&(i * 7)] == i as u32));
    }
}
