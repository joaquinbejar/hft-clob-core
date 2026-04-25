//! Engine-internal trade identifier.

use std::fmt;

/// Engine-internal monotonic trade identifier.
///
/// # Invariants
/// - Inner value increments by exactly one per emitted trade via
///   [`TradeId::next`].
/// - Construction via [`TradeId::INITIAL`] is the engine's normal
///   start-of-day path; [`TradeId::new`] is provided so the replay
///   harness can seed from a recorded value.
/// - Arithmetic is checked — overflow returns [`TradeIdError::Overflow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct TradeId(u64);

impl TradeId {
    /// Initial value. The engine starts every run from here.
    pub const INITIAL: Self = Self(0);

    /// Construct from a raw `u64`. Used by the replay harness.
    #[inline]
    #[must_use]
    pub const fn new(v: u64) -> Self {
        Self(v)
    }

    /// Advance to the next trade id.
    ///
    /// # Errors
    /// Returns [`TradeIdError::Overflow`] when the counter would exceed
    /// [`u64::MAX`].
    #[inline]
    pub const fn next(self) -> Result<Self, TradeIdError> {
        match self.0.checked_add(1) {
            Some(v) => Ok(Self(v)),
            None => Err(TradeIdError::Overflow),
        }
    }

    /// Raw counter value. Use only at the wire / serialization edge.
    #[inline(always)]
    #[must_use]
    pub const fn as_raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TradeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "trd:{}", self.0)
    }
}

/// Error type for [`TradeId::next`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TradeIdError {
    /// Counter would exceed [`u64::MAX`].
    #[error("trade_id overflow")]
    Overflow,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for TradeId {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            (0u64..=u64::MAX).prop_map(TradeId::new).boxed()
        }
    }

    #[test]
    fn test_trade_id_initial_is_zero() {
        assert_eq!(TradeId::INITIAL.as_raw(), 0);
    }

    #[test]
    fn test_trade_id_next_increments_by_one() {
        let s = TradeId::new(99).next().expect("no overflow");
        assert_eq!(s.as_raw(), 100);
    }

    #[test]
    fn test_trade_id_next_overflow_returns_err() {
        assert_eq!(TradeId::new(u64::MAX).next(), Err(TradeIdError::Overflow));
    }

    proptest! {
        #[test]
        fn proptest_trade_id_strictly_monotonic(seed in 0u64..=u64::MAX - 1) {
            let t0 = TradeId::new(seed);
            let t1 = t0.next().expect("no overflow");
            prop_assert!(t1 > t0);
            prop_assert_eq!(t1.as_raw(), seed + 1);
        }
    }
}
