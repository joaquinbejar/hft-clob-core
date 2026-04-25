//! Cancellation reason codes.

use std::fmt;

/// Closed enumeration of cancellation reasons emitted in execution
/// reports. Wire-stable: numeric discriminants are part of the public
/// contract and must not be reassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CancelReason {
    /// Client sent an explicit `CancelOrder`.
    UserRequested = 1,
    /// Client sent a `MassCancel` covering this order.
    MassCancel = 2,
    /// IOC remainder cancelled after the partial fill walk.
    IocRemaining = 3,
    /// Self-trade prevention fired with cancel-both policy.
    SelfTradePrevented = 4,
    /// Order superseded by a `CancelReplace` that could not preserve
    /// queue priority.
    Replaced = 5,
}

impl CancelReason {
    /// Numeric discriminant for the wire encoder.
    #[inline(always)]
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for CancelReason {
    type Error = CancelReasonError;
    #[inline]
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            1 => Ok(Self::UserRequested),
            2 => Ok(Self::MassCancel),
            3 => Ok(Self::IocRemaining),
            4 => Ok(Self::SelfTradePrevented),
            5 => Ok(Self::Replaced),
            other => Err(CancelReasonError::Unknown(other)),
        }
    }
}

impl fmt::Display for CancelReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Decode error for [`CancelReason::try_from`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CancelReasonError {
    /// Wire payload contained a discriminant outside the assigned range.
    #[error("unknown CancelReason discriminant: {0}")]
    Unknown(u8),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for CancelReason {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            prop_oneof![
                Just(Self::UserRequested),
                Just(Self::MassCancel),
                Just(Self::IocRemaining),
                Just(Self::SelfTradePrevented),
                Just(Self::Replaced),
            ]
            .boxed()
        }
    }

    #[test]
    fn test_cancel_reason_discriminants_are_stable() {
        assert_eq!(CancelReason::UserRequested.as_u8(), 1);
        assert_eq!(CancelReason::Replaced.as_u8(), 5);
    }

    #[test]
    fn test_cancel_reason_try_from_unknown_returns_err() {
        assert_eq!(
            CancelReason::try_from(0),
            Err(CancelReasonError::Unknown(0))
        );
        assert_eq!(
            CancelReason::try_from(6),
            Err(CancelReasonError::Unknown(6))
        );
    }

    proptest! {
        #[test]
        fn proptest_cancel_reason_u8_roundtrip(r in any::<CancelReason>()) {
            prop_assert_eq!(CancelReason::try_from(r.as_u8()).expect("roundtrip"), r);
        }
    }
}
