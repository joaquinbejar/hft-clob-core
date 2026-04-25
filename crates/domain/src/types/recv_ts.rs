//! Engine-stamped receive timestamp.

use std::fmt;

/// Receive timestamp in nanoseconds since the UNIX epoch.
///
/// # Invariants
/// - Stamped by the gateway / engine via an injected `Clock` trait at
///   the inbound decode boundary. The matching core never calls the
///   wall clock directly — replay swaps in a deterministic stub.
/// - This type does not call the wall clock — it carries a value the
///   caller already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct RecvTs(i64);

impl RecvTs {
    /// Zero-valued sentinel for "unset". Useful in tests and as a
    /// neutral default in proptests; not a meaningful timestamp.
    pub const ZERO: Self = Self(0);

    /// Construct from a raw nanosecond count.
    #[inline]
    #[must_use]
    pub const fn new(nanos: i64) -> Self {
        Self(nanos)
    }

    /// Raw nanosecond count.
    #[inline(always)]
    #[must_use]
    pub const fn as_nanos(self) -> i64 {
        self.0
    }
}

impl From<i64> for RecvTs {
    #[inline]
    fn from(v: i64) -> Self {
        Self::new(v)
    }
}

impl fmt::Display for RecvTs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rts:{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for RecvTs {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            any::<i64>().prop_map(RecvTs::new).boxed()
        }
    }

    #[test]
    fn test_recv_ts_zero_is_zero() {
        assert_eq!(RecvTs::ZERO.as_nanos(), 0);
    }

    proptest! {
        #[test]
        fn proptest_recv_ts_roundtrip(ts in any::<RecvTs>()) {
            prop_assert_eq!(RecvTs::new(ts.as_nanos()), ts);
        }
    }
}
