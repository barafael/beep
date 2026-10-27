//! Reproducible randomness for the phases that draw what they do (the soak, the tour): from a
//! seed printed at the start, which `TESTBOX_SEED` gives again.

use crate::process;

/// `TESTBOX_SEED`, or one from the clock: odd, as xorshift stays at zero from zero.
pub(super) fn seed() -> u64 {
    std::env::var("TESTBOX_SEED")
        .ok()
        .and_then(|seed| seed.parse().ok())
        .unwrap_or_else(|| process::now().unwrap_or(1))
        | 1
}

/// A reproducible sequence from its seed (xorshift64).
pub(super) struct Xorshift(pub(super) u64);

impl Xorshift {
    pub(super) fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }

    /// One of `items`, none of none.
    pub(super) fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            return None;
        }
        items.get(usize::try_from(self.below(items.len() as u64)).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::Xorshift;

    #[test]
    fn a_seed_repeats_its_sequence() {
        let (mut a, mut b) = (Xorshift(42), Xorshift(42));
        let first: Vec<u64> = (0..5).map(|_| a.below(100)).collect();
        let second: Vec<u64> = (0..5).map(|_| b.below(100)).collect();
        assert_eq!(first, second);
    }
}
