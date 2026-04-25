//! Order quantity as a lot count.

use std::fmt;

use crate::consts::LOT_SIZE;

/// Order quantity expressed as a raw lot count.
///
/// # Invariants
/// - Inner value is strictly positive.
/// - Inner value is a multiple of [`LOT_SIZE`].
/// - The inner field is private; callers obtain a `Qty` only through
///   [`Qty::new`] or [`TryFrom<u64>`], so the invariants hold by
///   construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Qty(u64);

impl Qty {
    /// Smallest legal quantity (one lot).
    pub const MIN: Self = Self(LOT_SIZE);

    /// Largest representable quantity, rounded down to a `LOT_SIZE`
    /// multiple. While `LOT_SIZE == 1` this equals `u64::MAX`; the
    /// rounding stays in the source so a future PR that raises
    /// `LOT_SIZE` does not silently produce an invalid `MAX`.
    #[allow(clippy::modulo_one)]
    pub const MAX: Self = Self(u64::MAX - (u64::MAX % LOT_SIZE));

    /// Construct from a raw lot count.
    ///
    /// # Errors
    /// Returns [`QtyError::Zero`] when `lots == 0`.
    /// Returns [`QtyError::LotViolation`] when `lots % LOT_SIZE != 0`.
    // Clippy's `modulo_one` and `manual_is_multiple_of` lints fire while
    // `LOT_SIZE == 1` because the lot check is structurally a no-op.
    // Kept deliberately so the wire-visible `RejectReason::LotViolation`
    // path stays live the moment a future PR raises `LOT_SIZE`.
    #[allow(clippy::modulo_one, clippy::manual_is_multiple_of)]
    #[inline]
    pub const fn new(lots: u64) -> Result<Self, QtyError> {
        if lots == 0 {
            return Err(QtyError::Zero);
        }
        if lots % LOT_SIZE != 0 {
            return Err(QtyError::LotViolation);
        }
        Ok(Self(lots))
    }

    /// Raw lot count. Use only at the wire / serialization edge.
    #[inline(always)]
    #[must_use]
    pub const fn as_lots(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for Qty {
    type Error = QtyError;
    #[inline]
    fn try_from(v: u64) -> Result<Self, Self::Error> {
        Self::new(v)
    }
}

impl fmt::Display for Qty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}q", self.0)
    }
}

/// Validation error for [`Qty::new`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum QtyError {
    /// `lots == 0`.
    #[error("quantity must be strictly positive")]
    Zero,
    /// `lots` is not a multiple of [`LOT_SIZE`].
    #[error("quantity must be a multiple of LOT_SIZE")]
    LotViolation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for Qty {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            // Generate as a multiple of LOT_SIZE so the strategy stays
            // valid the moment a future PR raises LOT_SIZE above 1.
            (1u64..=1_000_000)
                .prop_map(|n| Qty::new(n * LOT_SIZE).expect("strategy yields valid Qty"))
                .boxed()
        }
    }

    #[test]
    fn test_qty_new_zero_returns_zero_error() {
        assert_eq!(Qty::new(0), Err(QtyError::Zero));
    }

    #[test]
    fn test_qty_new_one_lot_returns_ok() {
        assert!(Qty::new(LOT_SIZE).is_ok());
    }

    #[test]
    fn test_qty_min_is_one_lot() {
        assert_eq!(Qty::MIN.as_lots(), LOT_SIZE);
    }

    proptest! {
        #[test]
        fn proptest_qty_roundtrip_lots(q in any::<Qty>()) {
            let raw = q.as_lots();
            prop_assert_eq!(Qty::new(raw).expect("roundtrip"), q);
        }
    }
}
