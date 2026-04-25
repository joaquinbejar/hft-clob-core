//! Client-assigned order identifier.

use std::fmt;

/// Client-assigned order identifier, unique per `(account_id, order_id)`.
///
/// # Invariants
/// - Inner value is non-zero. `0` is reserved as a sentinel for "no
///   identifier" in upstream protocols and never legal as a payload.
/// - Identifier uniqueness across an account is enforced by the engine,
///   not by this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct OrderId(u64);

impl OrderId {
    /// Construct from a raw `u64`.
    ///
    /// # Errors
    /// Returns [`OrderIdError::Zero`] when `id == 0`.
    #[inline]
    pub const fn new(id: u64) -> Result<Self, OrderIdError> {
        if id == 0 {
            return Err(OrderIdError::Zero);
        }
        Ok(Self(id))
    }

    /// Raw identifier. Use only at the wire / serialization edge.
    #[inline(always)]
    #[must_use]
    pub const fn as_raw(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for OrderId {
    type Error = OrderIdError;
    #[inline]
    fn try_from(v: u64) -> Result<Self, Self::Error> {
        Self::new(v)
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ord:{}", self.0)
    }
}

/// Validation error for [`OrderId::new`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OrderIdError {
    /// `id == 0` (reserved sentinel).
    #[error("order_id must be non-zero")]
    Zero,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for OrderId {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            (1u64..=u64::MAX)
                .prop_map(|v| OrderId::new(v).expect("strategy yields valid OrderId"))
                .boxed()
        }
    }

    #[test]
    fn test_order_id_new_zero_returns_err() {
        assert_eq!(OrderId::new(0), Err(OrderIdError::Zero));
    }

    #[test]
    fn test_order_id_new_one_returns_ok() {
        assert!(OrderId::new(1).is_ok());
    }

    proptest! {
        #[test]
        fn proptest_order_id_roundtrip(id in any::<OrderId>()) {
            prop_assert_eq!(OrderId::new(id.as_raw()).expect("roundtrip"), id);
        }
    }
}
