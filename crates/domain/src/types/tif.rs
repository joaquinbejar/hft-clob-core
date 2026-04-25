//! Time-in-force.

use std::fmt;

/// Time-in-force policy. Wire-stable: numeric discriminants are part
/// of the public contract and must not be reassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Tif {
    /// Good-til-cancel — rests on the book until matched or cancelled.
    Gtc = 1,
    /// Immediate-or-cancel — fills as much as possible, cancels the
    /// remainder. Never rests.
    Ioc = 2,
    /// Post-only — rejected on arrival if it would cross. Otherwise
    /// behaves as `Gtc`.
    PostOnly = 3,
}

impl Tif {
    /// Numeric discriminant for the wire encoder.
    #[inline(always)]
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for Tif {
    type Error = TifError;
    #[inline]
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            1 => Ok(Self::Gtc),
            2 => Ok(Self::Ioc),
            3 => Ok(Self::PostOnly),
            other => Err(TifError::Unknown(other)),
        }
    }
}

impl fmt::Display for Tif {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gtc => f.write_str("GTC"),
            Self::Ioc => f.write_str("IOC"),
            Self::PostOnly => f.write_str("POST_ONLY"),
        }
    }
}

/// Decode error for [`Tif::try_from`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TifError {
    /// Wire payload contained a discriminant outside `{1, 2, 3}`.
    #[error("unknown Tif discriminant: {0}")]
    Unknown(u8),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for Tif {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            prop_oneof![Just(Tif::Gtc), Just(Tif::Ioc), Just(Tif::PostOnly)].boxed()
        }
    }

    #[test]
    fn test_tif_as_u8_assigns_one_two_three() {
        assert_eq!(Tif::Gtc.as_u8(), 1);
        assert_eq!(Tif::Ioc.as_u8(), 2);
        assert_eq!(Tif::PostOnly.as_u8(), 3);
    }

    #[test]
    fn test_tif_try_from_unknown_returns_err() {
        assert_eq!(Tif::try_from(0), Err(TifError::Unknown(0)));
        assert_eq!(Tif::try_from(4), Err(TifError::Unknown(4)));
    }

    proptest! {
        #[test]
        fn proptest_tif_u8_roundtrip(t in any::<Tif>()) {
            prop_assert_eq!(Tif::try_from(t.as_u8()).expect("roundtrip"), t);
        }
    }
}
