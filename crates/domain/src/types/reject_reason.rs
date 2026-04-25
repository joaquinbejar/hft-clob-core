//! Rejection reason codes.

use std::fmt;

/// Closed enumeration of rejection reasons emitted in execution
/// reports. Wire-stable: numeric discriminants are part of the public
/// contract and must not be reassigned.
///
/// New rejection reasons are added by appending — never by renumbering.
/// Decode of any unassigned discriminant returns
/// [`RejectReasonError::Unknown`]; discriminant `255` is reserved as a
/// sentinel that must not be assigned without a coordinated schema-
/// version bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RejectReason {
    /// Engine kill switch is engaged.
    KillSwitched = 1,
    /// Price field outside legal range (zero or negative).
    InvalidPrice = 2,
    /// Quantity field outside legal range (zero).
    InvalidQty = 3,
    /// Price not a multiple of `TICK_SIZE`.
    TickViolation = 4,
    /// Quantity not a multiple of `LOT_SIZE`.
    LotViolation = 5,
    /// Price differs from the reference by more than `PRICE_BAND_BPS`.
    PriceBand = 6,
    /// Account already at `MAX_OPEN_ORDERS_PER_ACCT`.
    MaxOpenOrders = 7,
    /// Adding this order would exceed `MAX_NOTIONAL_PER_ACCT`.
    MaxNotional = 8,
    /// Cancel / cancel-replace targeted an unknown `order_id`.
    UnknownOrderId = 9,
    /// New order's `order_id` already exists for this account.
    DuplicateOrderId = 10,
    /// Post-only order would cross on arrival.
    PostOnlyWouldCross = 11,
    /// Self-trade prevention triggered (cancel-both policy).
    SelfTradePrevented = 12,
    /// Market order arrived against an empty aggressive side.
    MarketOrderInEmptyBook = 13,
    /// Inbound message referenced an account not bound to this session.
    UnknownAccount = 14,
    /// Inbound payload failed wire-protocol validation.
    MalformedMessage = 15,
}

impl RejectReason {
    /// Numeric discriminant for the wire encoder.
    #[inline(always)]
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for RejectReason {
    type Error = RejectReasonError;
    #[inline]
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            1 => Ok(Self::KillSwitched),
            2 => Ok(Self::InvalidPrice),
            3 => Ok(Self::InvalidQty),
            4 => Ok(Self::TickViolation),
            5 => Ok(Self::LotViolation),
            6 => Ok(Self::PriceBand),
            7 => Ok(Self::MaxOpenOrders),
            8 => Ok(Self::MaxNotional),
            9 => Ok(Self::UnknownOrderId),
            10 => Ok(Self::DuplicateOrderId),
            11 => Ok(Self::PostOnlyWouldCross),
            12 => Ok(Self::SelfTradePrevented),
            13 => Ok(Self::MarketOrderInEmptyBook),
            14 => Ok(Self::UnknownAccount),
            15 => Ok(Self::MalformedMessage),
            other => Err(RejectReasonError::Unknown(other)),
        }
    }
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Decode error for [`RejectReason::try_from`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RejectReasonError {
    /// Wire payload contained a discriminant outside the assigned
    /// range. Discriminant `255` is reserved for forward-compatible
    /// decode of future schema versions.
    #[error("unknown RejectReason discriminant: {0}")]
    Unknown(u8),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for RejectReason {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            prop_oneof![
                Just(Self::KillSwitched),
                Just(Self::InvalidPrice),
                Just(Self::InvalidQty),
                Just(Self::TickViolation),
                Just(Self::LotViolation),
                Just(Self::PriceBand),
                Just(Self::MaxOpenOrders),
                Just(Self::MaxNotional),
                Just(Self::UnknownOrderId),
                Just(Self::DuplicateOrderId),
                Just(Self::PostOnlyWouldCross),
                Just(Self::SelfTradePrevented),
                Just(Self::MarketOrderInEmptyBook),
                Just(Self::UnknownAccount),
                Just(Self::MalformedMessage),
            ]
            .boxed()
        }
    }

    #[test]
    fn test_reject_reason_discriminants_are_stable() {
        assert_eq!(RejectReason::KillSwitched.as_u8(), 1);
        assert_eq!(RejectReason::MalformedMessage.as_u8(), 15);
    }

    #[test]
    fn test_reject_reason_try_from_unknown_returns_err() {
        assert_eq!(
            RejectReason::try_from(0),
            Err(RejectReasonError::Unknown(0))
        );
        assert_eq!(
            RejectReason::try_from(16),
            Err(RejectReasonError::Unknown(16))
        );
        assert_eq!(
            RejectReason::try_from(255),
            Err(RejectReasonError::Unknown(255))
        );
    }

    proptest! {
        #[test]
        fn proptest_reject_reason_u8_roundtrip(r in any::<RejectReason>()) {
            prop_assert_eq!(RejectReason::try_from(r.as_u8()).expect("roundtrip"), r);
        }
    }
}
