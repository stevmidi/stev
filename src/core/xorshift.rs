//! A tiny xorshift generator, so the few places wanting cheap randomness (the
//! agent bridge's humanising, the timing probe's pattern) need no dependency,
//! and the same seed gives the same run.

/// Xorshift64: fast, small, not for anything that must be unpredictable.
pub(crate) struct XorShift(u64);

impl XorShift {
    /// A generator from `seed`; the same seed gives the same values.
    pub(crate) fn new(seed: u64) -> XorShift {
        // Xorshift never leaves zero.
        XorShift(seed | 1)
    }

    /// The next value in `0..bound`.
    pub(crate) fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_values_within_the_bound() {
        let mut a = XorShift::new(42);
        let mut b = XorShift::new(42);
        let values: Vec<u64> = (0..100).map(|_| a.below(13)).collect();
        assert!(values.iter().all(|&v| v < 13));
        assert_eq!(values, (0..100).map(|_| b.below(13)).collect::<Vec<_>>());
        // A zero seed still moves.
        let mut zero = XorShift::new(0);
        assert!((0..10).any(|_| zero.below(1000) != 0));
    }
}
