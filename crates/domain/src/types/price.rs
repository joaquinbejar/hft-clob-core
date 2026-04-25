//! Fixed-point price as a tick count.

use std::fmt;

use crate::consts::TICK_SIZE;

/// Fixed-point price expressed as a raw tick count.
///
/// # Invariants
/// - Inner value is strictly positive.
/// - Inner value is a multiple of [`TICK_SIZE`].
/// - The inner field is private; callers obtain a `Price` only through
///   [`Price::new`] or [`TryFrom<i64>`], so the invariants hold by
///   construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Price(i64);

impl Price {
    /// Smallest legal price (one tick).
    pub const MIN: Self = Self(TICK_SIZE);

    /// Largest representable price, rounded down to a `TICK_SIZE`
    /// multiple. While `TICK_SIZE == 1` this equals `i64::MAX`; the
    /// rounding stays in the source so a future PR that raises
    /// `TICK_SIZE` does not silently produce an invalid `MAX`.
    #[allow(clippy::modulo_one)]
    pub const MAX: Self = Self(i64::MAX - (i64::MAX % TICK_SIZE));

    /// Construct from a raw tick count.
    ///
    /// # Errors
    /// Returns [`PriceError::NonPositive`] when `ticks <= 0`.
    /// Returns [`PriceError::TickViolation`] when `ticks % TICK_SIZE != 0`.
    // Clippy's `modulo_one` lint fires while `TICK_SIZE == 1` because the
    // tick check is structurally a no-op. Kept deliberately so the wire-
    // visible `RejectReason::TickViolation` path stays live the moment a
    // future PR raises `TICK_SIZE`.
    #[allow(clippy::modulo_one)]
    #[inline]
    pub const fn new(ticks: i64) -> Result<Self, PriceError> {
        if ticks <= 0 {
            return Err(PriceError::NonPositive);
        }
        if ticks % TICK_SIZE != 0 {
            return Err(PriceError::TickViolation);
        }
        Ok(Self(ticks))
    }

    /// Raw tick count. Use only at the wire / serialization edge —
    /// domain logic must operate on `Price` values, not on the
    /// underlying integer.
    #[inline(always)]
    #[must_use]
    pub const fn as_ticks(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for Price {
    type Error = PriceError;
    #[inline]
    fn try_from(v: i64) -> Result<Self, Self::Error> {
        Self::new(v)
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}t", self.0)
    }
}

/// Validation error for [`Price::new`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PriceError {
    /// `ticks <= 0`.
    #[error("price must be strictly positive")]
    NonPositive,
    /// `ticks` is not a multiple of [`TICK_SIZE`].
    #[error("price must be a multiple of TICK_SIZE")]
    TickViolation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for Price {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            // Generate as a multiple of TICK_SIZE so the strategy stays
            // valid the moment a future PR raises TICK_SIZE above 1.
            (1i64..=1_000_000)
                .prop_map(|n| Price::new(n * TICK_SIZE).expect("strategy yields valid Price"))
                .boxed()
        }
    }

    #[test]
    fn test_price_new_zero_returns_non_positive() {
        assert_eq!(Price::new(0), Err(PriceError::NonPositive));
    }

    #[test]
    fn test_price_new_negative_returns_non_positive() {
        assert_eq!(Price::new(-1), Err(PriceError::NonPositive));
    }

    #[test]
    fn test_price_new_one_tick_returns_ok() {
        assert!(Price::new(TICK_SIZE).is_ok());
    }

    #[test]
    fn test_price_min_is_one_tick() {
        assert_eq!(Price::MIN.as_ticks(), TICK_SIZE);
    }

    proptest! {
        #[test]
        fn proptest_price_roundtrip_ticks(p in any::<Price>()) {
            let raw = p.as_ticks();
            prop_assert_eq!(Price::new(raw).expect("roundtrip"), p);
        }
    }
}
