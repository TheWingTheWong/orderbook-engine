//! Fixed-memory log-linear latency histogram.
//!
//! Values below 256 are recorded exactly. Above that, each power-of-two range
//! is split into 128 equal sub-buckets, bounding the relative error of any
//! reported percentile to under 0.8%. The whole `u64` range fits in 7,424
//! counters (~58 KiB), so recording is a couple of integer ops and an
//! increment, with no allocation after construction.

const SUB_BITS: u32 = 8;
const EXACT: u64 = 1 << SUB_BITS; // 256 values recorded exactly
const HALF: u64 = EXACT / 2; // 128 sub-buckets per octave above that
const BUCKETS: usize = EXACT as usize + (64 - SUB_BITS as usize) * HALF as usize;

/// Histogram of `u64` samples (typically nanoseconds).
#[derive(Debug, Clone)]
pub struct Histogram {
    counts: Box<[u64]>,
    total: u64,
    sum: u128,
    min: u64,
    max: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn index_of(v: u64) -> usize {
    if v < EXACT {
        return v as usize;
    }
    let msb = 63 - v.leading_zeros(); // >= SUB_BITS
    let shift = msb - (SUB_BITS - 1); // >= 1
    let mantissa = v >> shift; // in [HALF, EXACT)
    EXACT as usize + (shift as usize - 1) * HALF as usize + (mantissa - HALF) as usize
}

/// Largest value that maps to bucket `i`.
#[inline]
fn upper_bound_of(i: usize) -> u64 {
    if i < EXACT as usize {
        return i as u64;
    }
    let k = (i - EXACT as usize) as u64;
    let shift = k / HALF + 1;
    let mantissa = k % HALF + HALF;
    (((mantissa + 1) as u128) << shift).saturating_sub(1).min(u64::MAX as u128) as u64
}

impl Histogram {
    /// Empty histogram.
    pub fn new() -> Self {
        Self {
            counts: vec![0; BUCKETS].into_boxed_slice(),
            total: 0,
            sum: 0,
            min: u64::MAX,
            max: 0,
        }
    }

    /// Record one sample.
    #[inline]
    pub fn record(&mut self, v: u64) {
        self.counts[index_of(v)] += 1;
        self.total += 1;
        self.sum += v as u128;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
    }

    /// Number of samples.
    pub fn len(&self) -> u64 {
        self.total
    }

    /// Whether no samples were recorded.
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Smallest sample (0 if empty).
    pub fn min(&self) -> u64 {
        if self.total == 0 {
            0
        } else {
            self.min
        }
    }

    /// Largest sample.
    pub fn max(&self) -> u64 {
        self.max
    }

    /// Arithmetic mean (0 if empty).
    pub fn mean(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.sum as f64 / self.total as f64
        }
    }

    /// Value at percentile `p` (0.0..=100.0): the smallest bucket upper bound
    /// such that at least `p`% of samples are at or below it, clamped to the
    /// observed maximum.
    pub fn percentile(&self, p: f64) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let rank = ((p.clamp(0.0, 100.0) / 100.0) * self.total as f64).ceil().max(1.0) as u64;
        let mut seen = 0;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                return upper_bound_of(i).min(self.max);
            }
        }
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_mapping_is_monotonic_and_covers_u64() {
        let mut values: Vec<u64> = (0..64)
            .flat_map(|s| {
                let p = 1u64 << s;
                [p, p + 1, p + p / 2, p + (p - 1)]
            })
            .collect();
        values.sort_unstable();
        let mut last = 0;
        for v in values {
            let i = index_of(v);
            assert!(i < BUCKETS && i >= last, "v={v} i={i} last={last}");
            assert!(upper_bound_of(i) >= v, "v={v} i={i}");
            last = i;
        }
        assert_eq!(index_of(u64::MAX), BUCKETS - 1);
        assert_eq!(upper_bound_of(BUCKETS - 1), u64::MAX);
    }

    #[test]
    fn exact_below_256_and_bounded_error_above() {
        for v in 0..EXACT {
            assert_eq!(upper_bound_of(index_of(v)), v);
        }
        for v in (EXACT..10_000_000).step_by(7919) {
            let ub = upper_bound_of(index_of(v));
            assert!(ub >= v && (ub - v) as f64 / v as f64 <= 1.0 / HALF as f64, "v={v} ub={ub}");
        }
    }

    #[test]
    fn percentiles_of_uniform_samples() {
        let mut h = Histogram::new();
        for v in 1..=10_000 {
            h.record(v);
        }
        assert_eq!(h.len(), 10_000);
        assert_eq!((h.min(), h.max()), (1, 10_000));
        let p50 = h.percentile(50.0);
        assert!((5_000..=5_040).contains(&p50), "{p50}");
        let p99 = h.percentile(99.0);
        assert!((9_900..=9_980).contains(&p99), "{p99}");
        assert_eq!(h.percentile(100.0), 10_000);
        assert!((h.mean() - 5_000.5).abs() < 1e-9);
    }

    #[test]
    fn empty_histogram_is_all_zero() {
        let h = Histogram::new();
        assert!(h.is_empty());
        assert_eq!((h.min(), h.max(), h.percentile(99.0)), (0, 0, 0));
    }
}
