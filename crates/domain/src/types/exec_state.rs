//! Execution-report state.

use std::fmt;

/// Lifecycle state carried in every outbound `ExecReport`. Wire-stable:
/// numeric discriminants are part of the public contract and must not
/// be reassigned.
///
/// State machine (lossy summary; the engine emits a sequence of these
/// per order):
///
/// ```text
///   ┌─────────┐  reject ┌────────┐
///   │  ─────  ├────────►│Rejected│           terminal
///   │         │         └────────┘
///   │         │  ack    ┌────────┐  fill┌─────────────────┐
///   │ ingress ├────────►│Accepted├─────►│ PartiallyFilled │─┐
///   │         │         └────────┘      └─────────────────┘ │
///   └─────────┘                            │       │        │ fill remainder
///                                          │       └────────┴►┌──────┐  terminal
///                                          │                  │Filled│
///                                          │                  └──────┘
///                                          │ user / mass / IOC remainder / STP
///                                          ▼
///                                       ┌─────────┐
///                                       │Cancelled│              terminal
///                                       └─────────┘
///                                       ┌────────┐
///                                       │Replaced│   (CancelReplace, terminal for old order)
///                                       └────────┘
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ExecState {
    /// Order admitted to the engine; not yet matched.
    Accepted = 1,
    /// Order rejected pre-trade; carries a `RejectReason`.
    Rejected = 2,
    /// Order partially filled; remaining `leaves_qty` rests or
    /// continues the walk depending on TIF.
    PartiallyFilled = 3,
    /// Order fully filled; terminal.
    Filled = 4,
    /// Order cancelled; carries a `CancelReason`. Terminal.
    Cancelled = 5,
    /// Order superseded by a `CancelReplace`. Terminal for the old
    /// `order_id`; the new resting order is reported separately.
    Replaced = 6,
}

impl ExecState {
    /// Numeric discriminant for the wire encoder.
    #[inline(always)]
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for ExecState {
    type Error = ExecStateError;
    #[inline]
    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            1 => Ok(Self::Accepted),
            2 => Ok(Self::Rejected),
            3 => Ok(Self::PartiallyFilled),
            4 => Ok(Self::Filled),
            5 => Ok(Self::Cancelled),
            6 => Ok(Self::Replaced),
            other => Err(ExecStateError::Unknown(other)),
        }
    }
}

impl fmt::Display for ExecState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Decode error for [`ExecState::try_from`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExecStateError {
    /// Wire payload contained a discriminant outside `1..=6`.
    #[error("unknown ExecState discriminant: {0}")]
    Unknown(u8),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for ExecState {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            prop_oneof![
                Just(Self::Accepted),
                Just(Self::Rejected),
                Just(Self::PartiallyFilled),
                Just(Self::Filled),
                Just(Self::Cancelled),
                Just(Self::Replaced),
            ]
            .boxed()
        }
    }

    #[test]
    fn test_exec_state_discriminants_are_stable() {
        assert_eq!(ExecState::Accepted.as_u8(), 1);
        assert_eq!(ExecState::Replaced.as_u8(), 6);
    }

    #[test]
    fn test_exec_state_try_from_unknown_returns_err() {
        assert_eq!(ExecState::try_from(0), Err(ExecStateError::Unknown(0)));
        assert_eq!(ExecState::try_from(7), Err(ExecStateError::Unknown(7)));
    }

    proptest! {
        #[test]
        fn proptest_exec_state_u8_roundtrip(s in any::<ExecState>()) {
            prop_assert_eq!(ExecState::try_from(s.as_u8()).expect("roundtrip"), s);
        }
    }
}
