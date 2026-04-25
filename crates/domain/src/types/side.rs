//! Order side.

use std::fmt;

/// Order side. Wire-stable: numeric discriminants are part of the
/// public contract and must not be reassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Side {
    /// Buy side.
    Bid = 1,
    /// Sell side.
    Ask = 2,
}

impl Side {
    /// Numeric discriminant for the wire encoder.
    #[inline(always)]
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Opposite side. `Bid -> Ask`, `Ask -> Bid`.
    #[inline]
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Bid => Self::Ask,
            Self::Ask => Self::Bid,
        }
    }
}

impl TryFrom<u8> for Side {
    type Error = SideError;
    #[inline]
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            1 => Ok(Self::Bid),
            2 => Ok(Self::Ask),
            other => Err(SideError::Unknown(other)),
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bid => f.write_str("Bid"),
            Self::Ask => f.write_str("Ask"),
        }
    }
}

/// Decode error for [`Side::try_from`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SideError {
    /// Wire payload contained a discriminant outside `{1, 2}`.
    #[error("unknown Side discriminant: {0}")]
    Unknown(u8),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for Side {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            prop_oneof![Just(Side::Bid), Just(Side::Ask)].boxed()
        }
    }

    #[test]
    fn test_side_as_u8_assigns_one_and_two() {
        assert_eq!(Side::Bid.as_u8(), 1);
        assert_eq!(Side::Ask.as_u8(), 2);
    }

    #[test]
    fn test_side_opposite_swaps() {
        assert_eq!(Side::Bid.opposite(), Side::Ask);
        assert_eq!(Side::Ask.opposite(), Side::Bid);
    }

    #[test]
    fn test_side_try_from_unknown_returns_err() {
        assert_eq!(Side::try_from(0), Err(SideError::Unknown(0)));
        assert_eq!(Side::try_from(3), Err(SideError::Unknown(3)));
        assert_eq!(Side::try_from(255), Err(SideError::Unknown(255)));
    }

    proptest! {
        #[test]
        fn proptest_side_u8_roundtrip(s in any::<Side>()) {
            prop_assert_eq!(Side::try_from(s.as_u8()).expect("roundtrip"), s);
        }
    }
}
