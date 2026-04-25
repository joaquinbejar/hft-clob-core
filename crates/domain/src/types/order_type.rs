//! Order type.

use std::fmt;

/// Order type. Wire-stable: numeric discriminants are part of the
/// public contract and must not be reassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum OrderType {
    /// Limit order — rests on the book if not fully matched.
    Limit = 1,
    /// Market order — partial-fill-and-cancel; never rests.
    Market = 2,
}

impl OrderType {
    /// Numeric discriminant for the wire encoder.
    #[inline(always)]
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for OrderType {
    type Error = OrderTypeError;
    #[inline]
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            1 => Ok(Self::Limit),
            2 => Ok(Self::Market),
            other => Err(OrderTypeError::Unknown(other)),
        }
    }
}

impl fmt::Display for OrderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit => f.write_str("Limit"),
            Self::Market => f.write_str("Market"),
        }
    }
}

/// Decode error for [`OrderType::try_from`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OrderTypeError {
    /// Wire payload contained a discriminant outside `{1, 2}`.
    #[error("unknown OrderType discriminant: {0}")]
    Unknown(u8),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for OrderType {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            prop_oneof![Just(OrderType::Limit), Just(OrderType::Market)].boxed()
        }
    }

    #[test]
    fn test_order_type_as_u8_assigns_one_and_two() {
        assert_eq!(OrderType::Limit.as_u8(), 1);
        assert_eq!(OrderType::Market.as_u8(), 2);
    }

    #[test]
    fn test_order_type_try_from_unknown_returns_err() {
        assert_eq!(OrderType::try_from(0), Err(OrderTypeError::Unknown(0)));
        assert_eq!(OrderType::try_from(3), Err(OrderTypeError::Unknown(3)));
    }

    proptest! {
        #[test]
        fn proptest_order_type_u8_roundtrip(ot in any::<OrderType>()) {
            prop_assert_eq!(OrderType::try_from(ot.as_u8()).expect("roundtrip"), ot);
        }
    }
}
